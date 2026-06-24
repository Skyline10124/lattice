// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Callable managed-Lower preflight and verified promotion composition.
//!
//! Exposes [`prepare_promotion_target`] and [`promote_verified_candidate`] as
//! explicit host entry points. They are **not** invoked from [`super::stream_turn`];
//! callers must supply snapshot-bound verification and disposition ordering before
//! automatic lifecycle activation.
//!
//! Adds [`promote_preexported_candidate`] which accepts a caller-owned
//! [`CandidateExport`] and never calls `Workspace::export_candidate` internally.
//! It also adds [`PromotionLifecycleError`] for typed error classification and
//! [`retry_pinned_candidate`] for resuming promotion from a pinned candidate ref.
//!
//! Adds [`TurnTargetError`] and [`resolve_turn_target`] which
//! resolve the managed project target from Session metadata before Upper construction.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use git2::Repository;
use lattice_session::graph::TurnVerificationEvidence;
use lattice_session::{GraphSession, PromotedSessionCommitInput, create_promoted_session_commit};
use lattice_workspace::{
    CandidateExport,
    promotion::{
        ManagedLower, PathClassification, PinnedCandidate, PromotionError, PromotionJournal,
        PromotionLimits, PromotionOutcome, PromotionRequest, RecoveryDirection, RefComponent,
        VerificationResult, classify_path, commit, parse_journal, pin_candidate, prepare,
        prepare_from_validated, prepare_pinned, resolve_recovery_direction,
    },
};

use crate::session::ApiError;

/// Baseline and managed-Lower facts required before an Upper-backed turn may start.
#[derive(Debug, Clone)]
pub struct PromotionTurnTarget {
    pub repository_path: PathBuf,
    pub target_branch: String,
    pub project_ref: String,
    pub baseline_oid: String,
    pub lower: ManagedLower,
}

/// Session identifiers and commit timestamp supplied by the caller at promotion time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionSessionMetadata {
    pub session_id: String,
    pub turn_id: String,
    pub committed_at_unix_seconds: i64,
}

/// Typed turn-target resolution failure.
///
/// Produced when the Session-scoped project target cannot be resolved to a
/// managed Lower. These failures occur before any Upper adapter call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnTargetError {
    /// Session metadata is missing `project_target_branch` (legacy row).
    MissingProjectBranch,

    /// The target branch does not exist in the repository.
    BranchNotFound { branch: String },

    /// The target branch is checked out at a non-managed worktree.
    BranchAlreadyAttached { branch: String, path: PathBuf },

    /// The managed Lower worktree is dirty.
    DirtyLower(String),

    /// Recovery is required before the Lower can be used.
    RecoveryRequired,

    /// Recovery conflict prevents Lower use.
    RecoveryConflict(String),

    /// Infrastructure failure.
    Internal(String),
}

impl fmt::Display for TurnTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProjectBranch => {
                write!(f, "session is missing project_target_branch metadata")
            }
            Self::BranchNotFound { branch } => {
                write!(f, "branch '{branch}' not found in repository")
            }
            Self::BranchAlreadyAttached { branch, path } => {
                write!(
                    f,
                    "branch '{branch}' is already checked out at '{}'",
                    path.display()
                )
            }
            Self::DirtyLower(reason) => write!(f, "lower worktree is dirty: {reason}"),
            Self::RecoveryRequired => write!(f, "promotion recovery required before turn start"),
            Self::RecoveryConflict(detail) => {
                write!(f, "promotion recovery conflict: {detail}")
            }
            Self::Internal(msg) => write!(f, "turn target internal error: {msg}"),
        }
    }
}

impl std::error::Error for TurnTargetError {}

/// Resolve the managed project target from Session metadata.
///
/// Reads `project_target_branch` from the Session and passes it to
/// [`prepare_promotion_target`]. Does not use Session HEAD, current checkout,
/// or repository default branch.
///
/// Returns typed failures for missing metadata, nonexistent branches, or
/// branches attached to other worktrees before any Upper adapter call.
pub fn resolve_turn_target(
    session: &GraphSession,
    repository_path: impl AsRef<Path>,
) -> Result<PromotionTurnTarget, TurnTargetError> {
    let branch = session
        .project_target_branch
        .as_deref()
        .ok_or(TurnTargetError::MissingProjectBranch)?;

    let repo_path = repository_path.as_ref();

    // Verify the branch ref exists in the repository before provisioning.
    // This produces a typed BranchNotFound rather than an opaque git failure.
    let refname = format!("refs/heads/{branch}");
    let repo = Repository::open(repo_path)
        .map_err(|e| TurnTargetError::Internal(format!("open repository: {e}")))?;
    if repo.find_reference(&refname).is_err() {
        return Err(TurnTargetError::BranchNotFound {
            branch: branch.to_owned(),
        });
    }
    drop(repo);

    prepare_promotion_target(repo_path, branch).map_err(|err| match err {
        ApiError::Conflict(msg) if msg.contains("already checked out") => {
            TurnTargetError::BranchAlreadyAttached {
                branch: branch.to_owned(),
                path: PathBuf::from("unknown"),
            }
        }
        ApiError::Conflict(msg) if msg.contains("lower dirty") => TurnTargetError::DirtyLower(msg),
        ApiError::PromotionRecoveryRequired => TurnTargetError::RecoveryRequired,
        ApiError::PromotionRecoveryConflict(detail) => TurnTargetError::RecoveryConflict(detail),
        other => TurnTargetError::Internal(other.to_string()),
    })
}

/// Typed promotion failure classification for lifecycle disposition selection.
///
/// Preserves candidate facts where applicable so callers can pin, retry, or
/// release without re-deriving error context.
#[derive(Debug, Clone)]
pub enum PromotionLifecycleError {
    /// Baseline moved or conflicted during promotion.
    Conflict { detail: String },

    /// Temporary block (e.g. Lower dirty, recoverable journal). The candidate
    /// is pinned before this variant is returned so retry is always possible.
    TemporarilyBlocked { pinned: PinnedCandidate },

    /// Verification, hazard, or evidence rejection — the candidate itself is
    /// not acceptable.
    Rejected { detail: String },

    /// An unresolved recovery conflict requires explicit recovery before retry.
    RecoveryRequired,

    /// Infrastructure failure (git errors, I/O, callback failures).
    Internal(String),
}

impl fmt::Display for PromotionLifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict { detail } => write!(f, "promotion conflict: {detail}"),
            Self::TemporarilyBlocked { pinned } => {
                write!(
                    f,
                    "promotion temporarily blocked (candidate {} pinned at {})",
                    pinned.candidate_oid, pinned.candidate_ref
                )
            }
            Self::Rejected { detail } => write!(f, "{detail}"),
            Self::RecoveryRequired => write!(f, "promotion recovery required"),
            Self::Internal(msg) => write!(f, "promotion internal error: {msg}"),
        }
    }
}

impl std::error::Error for PromotionLifecycleError {}

impl PromotionLifecycleError {
    /// Classify a workspace [`PromotionError`] into a typed lifecycle variant.
    ///
    /// For `TemporarilyBlocked`, the pinned candidate is populated with empty
    /// placeholders; the promotion API layer pins the real candidate before
    /// returning this variant to callers.
    pub fn from_promotion_error(err: PromotionError) -> Self {
        match err {
            PromotionError::BaselineMismatch { expected, actual }
            | PromotionError::BaselineConflict { expected, actual } => Self::Conflict {
                detail: format!("baseline: expected {expected}, got {actual}"),
            },
            PromotionError::Dirty(_reason) => Self::TemporarilyBlocked {
                pinned: PinnedCandidate {
                    candidate_oid: String::new(),
                    candidate_ref: String::new(),
                    baseline_oid: String::new(),
                },
            },
            PromotionError::RecoveryRequired => Self::RecoveryRequired,
            PromotionError::RecoveryConflict { detail: _ } => Self::RecoveryRequired,
            PromotionError::VerificationMismatch { expected, actual } => Self::Rejected {
                detail: format!("verification mismatch: expected {expected}, got {actual}"),
            },
            PromotionError::CandidateHazard(hazard) => Self::Rejected {
                detail: format!("candidate hazard: {hazard:?}"),
            },
            PromotionError::InvalidEvidence { reason } => Self::Rejected { detail: reason },
            PromotionError::NotDescendant {
                baseline,
                candidate,
            } => Self::Rejected {
                detail: format!("candidate {candidate} is not a descendant of {baseline}"),
            },
            PromotionError::MalformedBundle | PromotionError::ConnectivityFailure => {
                Self::Rejected {
                    detail: "bundle validation failed".into(),
                }
            }
            other => Self::Internal(other.to_string()),
        }
    }
}

/// Provision or open managed Lower, reject recovery/dirty state, and return turn-start facts.
///
/// Does not auto-recover interrupted promotion journals; callers must invoke
/// [`lattice_workspace::promotion::recover`] explicitly before retrying preflight.
pub fn prepare_promotion_target(
    repository_path: impl AsRef<Path>,
    target_branch: &str,
) -> Result<PromotionTurnTarget, ApiError> {
    let repository_path = repository_path.as_ref();
    let lower = ManagedLower::provision_or_open(repository_path, target_branch)
        .map_err(map_promotion_error)?;
    inspect_recovery_state(&lower)?;
    let baseline = lower.inspect_clean().map_err(map_promotion_error)?;
    Ok(PromotionTurnTarget {
        repository_path: repository_path.to_path_buf(),
        target_branch: target_branch.to_owned(),
        project_ref: baseline.project_ref,
        baseline_oid: baseline.baseline_oid,
        lower,
    })
}

/// Prepare and commit a pre-exported candidate through the Session object callback.
///
/// Accepts a caller-owned [`CandidateExport`] and never calls
/// `Workspace::export_candidate` internally. The candidate OID is
/// captured before consuming the export so errors retain exact lifecycle facts.
///
/// Requires caller-supplied [`VerificationResult`] evidence; this module never
/// fabricates passing verification.
///
/// On promotion failure, errors are classified through [`PromotionLifecycleError`].
/// For temporary blocks, the candidate is pinned before returning so retry is
/// always possible.
pub async fn promote_preexported_candidate(
    target: &PromotionTurnTarget,
    metadata: &PromotionSessionMetadata,
    export: CandidateExport,
    verification: VerificationResult,
    limits: PromotionLimits,
) -> Result<PromotionOutcome, ApiError> {
    let session_id =
        RefComponent::parse("session_id", &metadata.session_id).map_err(map_ref_error)?;
    let turn_id = RefComponent::parse("turn_id", &metadata.turn_id).map_err(map_ref_error)?;

    // Capture candidate OID before consuming the export.
    let candidate_oid = export.candidate_oid().to_owned();

    let request = PromotionRequest {
        repository_path: target.repository_path.clone(),
        branch: target.target_branch.clone(),
        export,
        verification: verification.clone(),
        session_id,
        turn_id,
        limits,
    };

    let prepared = match prepare(request) {
        Ok(p) => p,
        Err(err) => return Err(classify_and_pin(target, metadata, &candidate_oid, err)),
    };

    let metadata_clone = metadata.clone();
    let baseline_oid = target.baseline_oid.clone();
    let target_branch = target.target_branch.clone();

    match commit(prepared, |repo, parent_session_oid| {
        let evidence = TurnVerificationEvidence {
            argv: verification.argv.clone(),
            profile_hash: verification.profile_hash.clone(),
            candidate_oid: verification.candidate_oid.clone(),
            expected_exit_code: verification.expected_exit_code,
            exit_code: verification.exit_code,
            passed: verification.passed,
        };
        let input = PromotedSessionCommitInput {
            session_id: metadata_clone.session_id,
            turn_id: metadata_clone.turn_id,
            baseline_oid: baseline_oid.clone(),
            candidate_oid: candidate_oid.clone(),
            committed_at_unix_seconds: metadata_clone.committed_at_unix_seconds,
            parent_session_oid,
            target_project_ref: format!("refs/heads/{}", target_branch),
            candidate_ref: None,
            execution_status: lattice_session::graph::TurnCommitStatus::Completed,
            verification: Some(evidence),
        };
        create_promoted_session_commit(repo, &input)
            .map_err(|e| PromotionError::CallbackFailed(e.to_string()))
    }) {
        Ok(outcome) => Ok(outcome),
        Err(err) => Err(classify_and_pin(target, metadata, &candidate_oid, err)),
    }
}

/// Retry promotion from a previously pinned candidate.
///
/// Uses [`prepare_pinned`] to re-validate the candidate from its ref without
/// requiring a bundle or Upper workspace. The same Session object
/// callback and transaction path as [`promote_preexported_candidate`] are used.
pub async fn retry_pinned_candidate(
    target: &PromotionTurnTarget,
    metadata: &PromotionSessionMetadata,
    pinned: &PinnedCandidate,
    verification: VerificationResult,
    limits: PromotionLimits,
) -> Result<PromotionOutcome, ApiError> {
    let session_id =
        RefComponent::parse("session_id", &metadata.session_id).map_err(map_ref_error)?;
    let turn_id = RefComponent::parse("turn_id", &metadata.turn_id).map_err(map_ref_error)?;

    let candidate_oid = pinned.candidate_oid.clone();

    // Validate the pinned candidate from its ref (no bundle required).
    let validated = prepare_pinned(
        &target.repository_path,
        &pinned.baseline_oid,
        &session_id,
        &turn_id,
        &verification,
        &limits,
    )
    .map_err(|err| classify_and_pin(target, metadata, &candidate_oid, err))?;

    // Build a PreparedPromotion from the validated candidate.
    let prepared = prepare_from_validated(
        target.repository_path.clone(),
        target.target_branch.clone(),
        &session_id,
        &turn_id,
        validated,
        &limits,
    )
    .map_err(|err| classify_and_pin(target, metadata, &candidate_oid, err))?;

    let metadata_clone = metadata.clone();
    let baseline_oid = target.baseline_oid.clone();
    let target_branch = target.target_branch.clone();

    commit(prepared, |repo, parent_session_oid| {
        let evidence = TurnVerificationEvidence {
            argv: verification.argv.clone(),
            profile_hash: verification.profile_hash.clone(),
            candidate_oid: verification.candidate_oid.clone(),
            expected_exit_code: verification.expected_exit_code,
            exit_code: verification.exit_code,
            passed: verification.passed,
        };
        let input = PromotedSessionCommitInput {
            session_id: metadata_clone.session_id,
            turn_id: metadata_clone.turn_id,
            baseline_oid: baseline_oid.clone(),
            candidate_oid: candidate_oid.clone(),
            committed_at_unix_seconds: metadata_clone.committed_at_unix_seconds,
            parent_session_oid,
            target_project_ref: format!("refs/heads/{}", target_branch),
            candidate_ref: Some(pinned.candidate_ref.clone()),
            execution_status: lattice_session::graph::TurnCommitStatus::Completed,
            verification: Some(evidence),
        };
        create_promoted_session_commit(repo, &input)
            .map_err(|e| PromotionError::CallbackFailed(e.to_string()))
    })
    .map_err(|err| classify_and_pin(target, metadata, &candidate_oid, err))
}

/// Classify a promotion error and pin the candidate if the error is a temporary block.
///
/// For temporary blocks (Lower dirty, recoverable journal), pins the candidate
/// before returning so retry is always possible. If pinning fails,
/// returns an Internal error.
fn classify_and_pin(
    target: &PromotionTurnTarget,
    metadata: &PromotionSessionMetadata,
    candidate_oid: &str,
    err: PromotionError,
) -> ApiError {
    let lifecycle = PromotionLifecycleError::from_promotion_error(err);
    match lifecycle {
        PromotionLifecycleError::TemporarilyBlocked { .. } => {
            // Pin the candidate before returning.
            let session_id = RefComponent::parse("session_id", &metadata.session_id);
            let turn_id = RefComponent::parse("turn_id", &metadata.turn_id);

            match (session_id, turn_id) {
                (Ok(sid), Ok(tid)) => {
                    match pin_candidate(&target.repository_path, candidate_oid, &sid, &tid) {
                        Ok(pinned) => ApiError::PromotionLifecycle(
                            PromotionLifecycleError::TemporarilyBlocked { pinned },
                        ),
                        Err(pin_err) => {
                            // Pinning failed — return failure, do not permit snapshot release.
                            ApiError::PromotionLifecycle(PromotionLifecycleError::Internal(
                                format!("failed to pin candidate: {pin_err}"),
                            ))
                        }
                    }
                }
                _ => {
                    // Cannot parse ref components — internal error.
                    ApiError::PromotionLifecycle(PromotionLifecycleError::Internal(
                        "invalid session/turn metadata for pinning".into(),
                    ))
                }
            }
        }
        other => ApiError::PromotionLifecycle(other),
    }
}

fn inspect_recovery_state(lower: &ManagedLower) -> Result<(), ApiError> {
    let state_dir = &lower.promotion_paths().promotion_state_dir;
    if !PromotionJournal::exists(state_dir) {
        return Ok(());
    }

    let project_ref_oid = resolve_ref_required(
        lower.repository_path(),
        &format!("refs/heads/{}", lower.branch()),
    )
    .map_err(map_promotion_error)?;

    let parsed =
        parse_journal(&PromotionJournal::path_in(state_dir)).map_err(map_promotion_error)?;
    let direction = resolve_recovery_direction(&project_ref_oid, &parsed.header);
    if direction == RecoveryDirection::RefConflict {
        return Err(map_promotion_error(PromotionError::RecoveryConflict {
            detail: format!(
                "project ref oid {project_ref_oid} matches neither baseline {} nor candidate {}",
                parsed.header.baseline_oid, parsed.header.candidate_oid
            ),
        }));
    }

    let repo = Repository::open(lower.repository_path())
        .map_err(|e| map_promotion_error(PromotionError::Git(e.to_string())))?;

    for entry in parsed.header.path_plan.entries() {
        let class = classify_path(
            lower.worktree_path(),
            &repo,
            &entry.path,
            &entry.old,
            &entry.new,
        )
        .map_err(map_promotion_error)?;
        if class == PathClassification::ThirdState {
            return Err(map_promotion_error(PromotionError::RecoveryConflict {
                detail: format!("third-state path {}", entry.path),
            }));
        }
    }

    Err(map_promotion_error(PromotionError::RecoveryRequired))
}

fn resolve_ref_required(repo: &Path, refname: &str) -> Result<String, PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", refname])
        .output()
        .map_err(PromotionError::Io)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(PromotionError::GitCommand {
            context: format!("resolve {refname}"),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn map_ref_error(err: lattice_workspace::promotion::RefTransactionError) -> ApiError {
    ApiError::BadRequest(err.to_string())
}

pub(crate) fn map_workspace_error(err: lattice_workspace::WorkspaceError) -> ApiError {
    ApiError::Internal(err.to_string())
}

pub(crate) fn map_promotion_error(err: PromotionError) -> ApiError {
    match err {
        PromotionError::RecoveryRequired => ApiError::PromotionRecoveryRequired,
        PromotionError::RecoveryConflict { detail } => ApiError::PromotionRecoveryConflict(detail),
        PromotionError::Dirty(reason) => ApiError::Conflict(format!("lower dirty: {reason:?}")),
        PromotionError::BranchAlreadyAttached { branch, path } => ApiError::Conflict(format!(
            "branch `{branch}` already checked out at `{}`",
            path.display()
        )),
        PromotionError::VerificationMismatch { expected, actual } => ApiError::Conflict(format!(
            "verification mismatch: expected {expected}, got {actual}"
        )),
        PromotionError::BaselineMismatch { expected, actual }
        | PromotionError::BaselineConflict { expected, actual } => ApiError::Conflict(format!(
            "baseline mismatch: expected {expected}, got {actual}"
        )),
        PromotionError::InvalidBranchRef { branch, reason } => {
            ApiError::BadRequest(format!("invalid branch `{branch}`: {reason}"))
        }
        PromotionError::WorktreeAddFailed { ref stderr } => {
            // Map git worktree add failure for non-existent branches to a typed
            // error that resolve_turn_target can classify.
            ApiError::Internal(format!("worktree add failed: {stderr}"))
        }
        other => ApiError::Internal(other.to_string()),
    }
}
