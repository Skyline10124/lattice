// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Callable managed-Lower preflight and verified promotion composition.
//!
//! Phase 35 exposes [`prepare_promotion_target`] and [`promote_verified_candidate`] as
//! explicit host entry points. They are **not** invoked from [`super::stream_turn`];
//! Phase 36 must supply snapshot-bound verification and disposition ordering before
//! automatic lifecycle activation.

use std::path::{Path, PathBuf};
use std::process::Command;

use git2::Repository;
use lattice_session::{create_promoted_session_commit, PromotedSessionCommitInput};
use lattice_workspace::{
    promotion::{
        classify_path, commit, parse_journal, prepare, ManagedLower, PathClassification,
        PromotionError, PromotionJournal, PromotionLimits, PromotionOutcome, PromotionRequest,
        RecoveryDirection, RefComponent, VerificationResult, resolve_recovery_direction,
    },
    UpperHandle, Workspace,
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

/// Export, prepare, and commit a verified Upper candidate through the Session object callback.
///
/// Requires caller-supplied [`VerificationResult`] evidence; this module never fabricates
/// passing verification. Phase 36 owns when live turns invoke this path.
pub async fn promote_verified_candidate(
    workspace: &dyn Workspace,
    upper: &UpperHandle,
    target: &PromotionTurnTarget,
    metadata: &PromotionSessionMetadata,
    verification: VerificationResult,
    limits: PromotionLimits,
) -> Result<PromotionOutcome, ApiError> {
    let session_id =
        RefComponent::parse("session_id", &metadata.session_id).map_err(map_ref_error)?;
    let turn_id = RefComponent::parse("turn_id", &metadata.turn_id).map_err(map_ref_error)?;

    let export = workspace
        .export_candidate(upper, &target.baseline_oid, limits.max_artifact_bytes)
        .await
        .map_err(map_workspace_error)?;
    let candidate_oid = export.candidate_oid().to_owned();

    let request = PromotionRequest {
        repository_path: target.repository_path.clone(),
        branch: target.target_branch.clone(),
        export,
        verification,
        session_id,
        turn_id,
        limits,
    };

    let prepared = prepare(request).map_err(map_promotion_error)?;
    let metadata = metadata.clone();
    let baseline_oid = target.baseline_oid.clone();

    commit(prepared, move |repo, parent_session_oid| {
        let input = PromotedSessionCommitInput {
            session_id: metadata.session_id,
            turn_id: metadata.turn_id,
            baseline_oid: baseline_oid.clone(),
            candidate_oid: candidate_oid.clone(),
            committed_at_unix_seconds: metadata.committed_at_unix_seconds,
            parent_session_oid,
        };
        create_promoted_session_commit(repo, &input)
            .map_err(|e| PromotionError::CallbackFailed(e.to_string()))
    })
    .map_err(map_promotion_error)
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

    let parsed = parse_journal(&PromotionJournal::path_in(state_dir)).map_err(map_promotion_error)?;
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

fn map_workspace_error(err: lattice_workspace::WorkspaceError) -> ApiError {
    ApiError::Internal(err.to_string())
}

pub(crate) fn map_promotion_error(err: PromotionError) -> ApiError {
    match err {
        PromotionError::RecoveryRequired => ApiError::PromotionRecoveryRequired,
        PromotionError::RecoveryConflict { detail } => {
            ApiError::PromotionRecoveryConflict(detail)
        }
        PromotionError::Dirty(reason) => ApiError::Conflict(format!("lower dirty: {reason:?}")),
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
        other => ApiError::Internal(other.to_string()),
    }
}
