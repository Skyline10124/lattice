// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! End-to-end promotion orchestration: prepare, commit, and recover.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use git2::Repository;
use uuid::Uuid;

use super::candidate::{ValidatedCandidate, VerificationResult, validate_candidate};
use super::journal::{
    FaultPoint, JournalHeader, PromotionJournal, RecoveryOutcome, recover_journal,
};
use super::lock::{
    DurabilityRecorder, LowerLockGuard, ProcIdentityReader, ProcessIdentityReader,
    StaleLockDisposition, recover_stale_lock,
};
use super::materialize::{DurableFs, MaterializationPlan, build_materialization_plan};
use super::refs::{PromotionRefs, RefComponent, RefTransactionRequest, run_ref_transaction};
use super::{ManagedLower, PromotionError, PromotionLimits};

/// Inputs for the long-running prepare phase (no Lower lock or journal).
#[derive(Debug)]
pub struct PromotionRequest {
    pub repository_path: PathBuf,
    pub branch: String,
    pub export: crate::CandidateExport,
    pub verification: VerificationResult,
    pub session_id: RefComponent,
    pub turn_id: RefComponent,
    pub limits: PromotionLimits,
}

/// Immutable prepared promotion consumed exactly once by [`commit`].
#[derive(Debug, Clone)]
pub struct PreparedPromotion {
    repository_path: PathBuf,
    branch: String,
    refs: PromotionRefs,
    validated: ValidatedCandidate,
    path_plan: MaterializationPlan,
    journal_id: String,
    old_project_oid: String,
    fault: Option<FaultPoint>,
}

/// Successful promotion outcome after refs and Lower converge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionOutcome {
    pub project_oid: String,
    pub session_oid: String,
    pub candidate_oid: String,
}

/// Prepare validates the candidate and builds the immutable plan without Lower effects.
pub fn prepare(request: PromotionRequest) -> Result<PreparedPromotion, PromotionError> {
    let refs = PromotionRefs::new(&request.branch, &request.session_id, &request.turn_id)?;

    let validated = validate_candidate(
        &request.repository_path,
        &request.export,
        request.export.baseline_oid(),
        &request.verification,
        &request.limits,
    )?;

    let path_plan = build_materialization_plan(
        &request.repository_path,
        &validated.baseline_oid,
        &validated.candidate_oid,
        &request.limits,
    )?;

    let old_project_oid =
        resolve_ref_required(&request.repository_path, refs.project_ref.as_str())?;

    if old_project_oid != validated.baseline_oid {
        return Err(PromotionError::BaselineMismatch {
            expected: validated.baseline_oid.clone(),
            actual: old_project_oid,
        });
    }

    Ok(PreparedPromotion {
        repository_path: request.repository_path,
        branch: request.branch,
        refs,
        validated,
        path_plan,
        journal_id: Uuid::new_v4().to_string(),
        old_project_oid,
        fault: None,
    })
}

/// Build a [`PreparedPromotion`] from an already-validated candidate.
///
/// Used for the pinned-retry path where the candidate has been validated
/// via [`prepare_pinned`] without requiring a bundle. The caller supplies
/// the validated candidate and the function builds the materialization plan
/// and checks baseline consistency.
pub fn prepare_from_validated(
    repository_path: PathBuf,
    branch: String,
    session_id: &RefComponent,
    turn_id: &RefComponent,
    validated: ValidatedCandidate,
    limits: &PromotionLimits,
) -> Result<PreparedPromotion, PromotionError> {
    let refs = PromotionRefs::new(&branch, session_id, turn_id)?;

    let path_plan = build_materialization_plan(
        &repository_path,
        &validated.baseline_oid,
        &validated.candidate_oid,
        limits,
    )?;

    let old_project_oid = resolve_ref_required(&repository_path, refs.project_ref.as_str())?;

    if old_project_oid != validated.baseline_oid {
        return Err(PromotionError::BaselineMismatch {
            expected: validated.baseline_oid.clone(),
            actual: old_project_oid,
        });
    }

    Ok(PreparedPromotion {
        repository_path,
        branch,
        refs,
        validated,
        path_plan,
        journal_id: Uuid::new_v4().to_string(),
        old_project_oid,
        fault: None,
    })
}

/// Commit is the only API that writes managed Lower (D-01/D-02).
pub fn commit<F>(
    prepared: PreparedPromotion,
    make_session_commit: F,
) -> Result<PromotionOutcome, PromotionError>
where
    F: FnOnce(&Repository, Option<git2::Oid>) -> Result<git2::Oid, PromotionError>,
{
    commit_with_reader(prepared, &ProcIdentityReader, None, make_session_commit)
}

/// Commit with injectable identity/durability hooks (tests and stale-lock recovery).
pub fn commit_with_reader<F>(
    prepared: PreparedPromotion,
    identity_reader: &dyn ProcessIdentityReader,
    durability: Option<Arc<dyn DurabilityRecorder>>,
    make_session_commit: F,
) -> Result<PromotionOutcome, PromotionError>
where
    F: FnOnce(&Repository, Option<git2::Oid>) -> Result<git2::Oid, PromotionError>,
{
    let lower = ManagedLower::provision_or_open(&prepared.repository_path, &prepared.branch)?;
    let state_dir = lower.promotion_paths().promotion_state_dir.clone();

    let _guard = establish_exclusive_ownership(&lower, identity_reader, durability.clone())?;

    replay_pending_journal_if_any(&lower)?;

    let baseline = lower.inspect_clean()?;
    if baseline.baseline_oid != prepared.validated.baseline_oid {
        return Err(PromotionError::BaselineConflict {
            expected: prepared.validated.baseline_oid.clone(),
            actual: baseline.baseline_oid,
        });
    }

    let observed_project_oid =
        resolve_ref_required(lower.repository_path(), prepared.refs.project_ref.as_str())?;
    if observed_project_oid != prepared.old_project_oid {
        return Err(PromotionError::BaselineConflict {
            expected: prepared.old_project_oid.clone(),
            actual: observed_project_oid,
        });
    }

    // D-14: if candidate ref already exists, verify it matches (pinned retry path).
    let existing_candidate_oid = resolve_ref(
        lower.repository_path(),
        prepared.refs.candidate_ref.as_str(),
    )?;
    if let Some(ref existing) = existing_candidate_oid
        && *existing != prepared.validated.candidate_oid
    {
        return Err(PromotionError::CandidateSubstitution {
            expected: existing.clone(),
            actual: prepared.validated.candidate_oid.clone(),
        });
    }

    let old_session_oid = resolve_ref(lower.repository_path(), prepared.refs.session_ref.as_str())?;

    let header = JournalHeader {
        journal_id: prepared.journal_id.clone(),
        project_ref: prepared.refs.project_ref.as_str().to_owned(),
        baseline_oid: prepared.validated.baseline_oid.clone(),
        candidate_oid: prepared.validated.candidate_oid.clone(),
        path_plan: prepared.path_plan.clone(),
    };

    let mut journal = PromotionJournal::create_new(&state_dir, header.clone())?;
    maybe_fault(prepared.fault, FaultPoint::AfterHeaderSync)?;

    let repo = Repository::open(lower.repository_path())
        .map_err(|e| PromotionError::Git(e.to_string()))?;

    let parent_session_oid = old_session_oid.as_deref().map(parse_git_oid).transpose()?;

    let session_oid = match make_session_commit(&repo, parent_session_oid) {
        Ok(oid) => oid.to_string(),
        Err(err) => {
            let _ = converge_journal_toward_baseline(&lower, &prepared);
            return Err(err);
        }
    };

    let txn_request = RefTransactionRequest::with_existing_candidate(
        prepared.refs.clone(),
        prepared.old_project_oid.clone(),
        old_session_oid.clone(),
        prepared.validated.candidate_oid.clone(),
        session_oid.clone(),
        existing_candidate_oid.clone(),
    )?;

    let txn_outcome = match run_ref_transaction(lower.repository_path(), &txn_request) {
        Ok(outcome) => outcome,
        Err(err) => {
            let _ = converge_journal_toward_baseline(&lower, &prepared);
            return Err(err.into());
        }
    };

    journal.record_refs_committed(
        &txn_outcome.project_oid,
        &txn_outcome.session_oid,
        &txn_outcome.candidate_oid,
    )?;
    if let Err(err) = maybe_fault(prepared.fault, FaultPoint::AfterRefsCommitted) {
        return Err(map_post_ref_crash(err));
    }

    let materialize_result =
        materialize_journaled_paths(&lower, &mut journal, &header, prepared.fault);

    if let Err(err) = materialize_result {
        return Err(map_post_ref_crash(err));
    }

    journal.finish().map_err(map_post_ref_crash)?;
    if let Err(err) = maybe_fault(prepared.fault, FaultPoint::AfterFinished) {
        return Err(map_post_ref_crash(err));
    }

    verify_lower_matches_project_ref(&lower, &txn_outcome.project_oid)?;

    if let Err(err) = maybe_fault(prepared.fault, FaultPoint::BeforeJournalRemove) {
        return Err(map_post_ref_crash(err));
    }
    journal.remove_durable()?;

    Ok(PromotionOutcome {
        project_oid: txn_outcome.project_oid,
        session_oid: txn_outcome.session_oid,
        candidate_oid: txn_outcome.candidate_oid,
    })
}

/// Recover converges an interrupted promotion journal before another commit.
pub fn recover(lower: &ManagedLower) -> Result<RecoveryOutcome, PromotionError> {
    recover_with_reader(lower, &ProcIdentityReader, None)
}

/// Recover with injectable identity/durability hooks.
pub fn recover_with_reader(
    lower: &ManagedLower,
    identity_reader: &dyn ProcessIdentityReader,
    durability: Option<&dyn DurabilityRecorder>,
) -> Result<RecoveryOutcome, PromotionError> {
    let state_dir = &lower.promotion_paths().promotion_state_dir;
    if !PromotionJournal::exists(state_dir) {
        return Ok(RecoveryOutcome::Complete);
    }

    let lock_path = &lower.promotion_paths().lock_path;
    match LowerLockGuard::inspect_existing(lock_path, identity_reader)? {
        None => {
            let project_oid = resolve_ref_required(
                lower.repository_path(),
                &format!("refs/heads/{}", lower.branch()),
            )?;
            let outcome = replay_journal(lower, &project_oid)?;
            if outcome == RecoveryOutcome::Complete {
                reset_lower_head(lower, &project_oid)?;
            }
            Ok(outcome)
        }
        Some(StaleLockDisposition::Live) => Err(PromotionError::LockHeld),
        Some(StaleLockDisposition::Malformed) => Err(PromotionError::GitCommand {
            context: "malformed lower lock".into(),
            stderr: lock_path.display().to_string(),
        }),
        Some(StaleLockDisposition::IdentityUnavailable) => Err(PromotionError::GitCommand {
            context: "lock identity unavailable".into(),
            stderr: lock_path.display().to_string(),
        }),
        Some(StaleLockDisposition::Stale { metadata }) => {
            let project_oid = resolve_ref_required(
                lower.repository_path(),
                &format!("refs/heads/{}", lower.branch()),
            )?;
            let lower = lower.clone();
            recover_stale_lock(
                lock_path,
                &metadata.lock_id,
                identity_reader,
                durability,
                || match replay_journal(&lower, &project_oid)? {
                    RecoveryOutcome::Complete => Ok(()),
                    RecoveryOutcome::RecoveryRequired => Err(PromotionError::RecoveryRequired),
                    RecoveryOutcome::RecoveryConflict { detail } => {
                        Err(PromotionError::RecoveryConflict { detail })
                    }
                },
            )?;
            Ok(RecoveryOutcome::Complete)
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl PreparedPromotion {
    pub fn with_fault(mut self, fault: FaultPoint) -> Self {
        self.fault = Some(fault);
        self
    }
}

fn establish_exclusive_ownership(
    lower: &ManagedLower,
    identity_reader: &dyn ProcessIdentityReader,
    durability: Option<Arc<dyn DurabilityRecorder>>,
) -> Result<LowerLockGuard, PromotionError> {
    let lock_path = &lower.promotion_paths().lock_path;
    match LowerLockGuard::inspect_existing(lock_path, identity_reader)? {
        None => LowerLockGuard::acquire(lock_path, identity_reader, durability),
        Some(StaleLockDisposition::Live) => Err(PromotionError::LockHeld),
        Some(StaleLockDisposition::Malformed) => Err(PromotionError::GitCommand {
            context: "malformed lower lock".into(),
            stderr: lock_path.display().to_string(),
        }),
        Some(StaleLockDisposition::IdentityUnavailable) => Err(PromotionError::GitCommand {
            context: "lock identity unavailable".into(),
            stderr: lock_path.display().to_string(),
        }),
        Some(StaleLockDisposition::Stale { metadata }) => {
            let project_oid = resolve_ref_required(
                lower.repository_path(),
                &format!("refs/heads/{}", lower.branch()),
            )?;
            let lower = lower.clone();
            recover_stale_lock(
                lock_path,
                &metadata.lock_id,
                identity_reader,
                durability.as_deref(),
                || match replay_journal(&lower, &project_oid)? {
                    RecoveryOutcome::Complete => Ok(()),
                    RecoveryOutcome::RecoveryRequired => Err(PromotionError::RecoveryRequired),
                    RecoveryOutcome::RecoveryConflict { detail } => {
                        Err(PromotionError::RecoveryConflict { detail })
                    }
                },
            )?;
            LowerLockGuard::acquire(lock_path, identity_reader, durability)
        }
    }
}

fn replay_pending_journal_if_any(lower: &ManagedLower) -> Result<(), PromotionError> {
    if !PromotionJournal::exists(&lower.promotion_paths().promotion_state_dir) {
        return Ok(());
    }
    let project_oid = resolve_ref_required(
        lower.repository_path(),
        &format!("refs/heads/{}", lower.branch()),
    )?;
    match replay_journal(lower, &project_oid)? {
        RecoveryOutcome::Complete => {
            reset_lower_head(lower, &project_oid)?;
            Ok(())
        }
        RecoveryOutcome::RecoveryRequired => Err(PromotionError::RecoveryRequired),
        RecoveryOutcome::RecoveryConflict { detail } => {
            Err(PromotionError::RecoveryConflict { detail })
        }
    }
}

fn replay_journal(
    lower: &ManagedLower,
    project_ref_oid: &str,
) -> Result<RecoveryOutcome, PromotionError> {
    recover_journal(
        lower.repository_path(),
        lower.worktree_path(),
        &lower.promotion_paths().promotion_state_dir,
        project_ref_oid,
    )
}

fn converge_journal_toward_baseline(
    lower: &ManagedLower,
    prepared: &PreparedPromotion,
) -> Result<(), PromotionError> {
    if !PromotionJournal::exists(&lower.promotion_paths().promotion_state_dir) {
        return Ok(());
    }
    match replay_journal(lower, &prepared.old_project_oid)? {
        RecoveryOutcome::Complete => Ok(()),
        RecoveryOutcome::RecoveryRequired | RecoveryOutcome::RecoveryConflict { .. } => {
            let path = PromotionJournal::path_in(&lower.promotion_paths().promotion_state_dir);
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            Ok(())
        }
    }
}

fn materialize_journaled_paths(
    lower: &ManagedLower,
    journal: &mut PromotionJournal,
    header: &JournalHeader,
    fault: Option<FaultPoint>,
) -> Result<(), PromotionError> {
    let repo = Repository::open(lower.repository_path())
        .map_err(|e| PromotionError::Git(e.to_string()))?;
    let mut fs = DurableFs::new(lower.worktree_path(), &repo, None);
    for entry in header.path_plan.entries() {
        let index = journal.record_intent(entry.clone())?;
        maybe_fault(fault, FaultPoint::AfterIntent { index })?;
        fs.apply_entry(entry)?;
        maybe_fault(fault, FaultPoint::AfterEffect { index })?;
        journal.record_complete(index)?;
        maybe_fault(fault, FaultPoint::AfterComplete { index })?;
    }
    Ok(())
}

fn verify_lower_matches_project_ref(
    lower: &ManagedLower,
    project_oid: &str,
) -> Result<(), PromotionError> {
    reset_lower_head(lower, project_oid)?;
    let baseline = lower.inspect_clean()?;
    if baseline.baseline_oid != project_oid {
        return Err(PromotionError::BaselineConflict {
            expected: project_oid.to_owned(),
            actual: baseline.baseline_oid,
        });
    }
    Ok(())
}

fn reset_lower_head(lower: &ManagedLower, oid: &str) -> Result<(), PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(lower.worktree_path())
        .args(["reset", "--hard", oid])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(PromotionError::GitCommand {
            context: "reset lower HEAD".into(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn resolve_ref(repo: &Path, refname: &str) -> Result<Option<String>, PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", refname])
        .output()?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    } else {
        Ok(None)
    }
}

fn resolve_ref_required(repo: &Path, refname: &str) -> Result<String, PromotionError> {
    resolve_ref(repo, refname)?.ok_or_else(|| PromotionError::GitCommand {
        context: format!("resolve {refname}"),
        stderr: "missing ref".into(),
    })
}

fn parse_git_oid(raw: &str) -> Result<git2::Oid, PromotionError> {
    git2::Oid::from_str(raw).map_err(|e| PromotionError::Git(format!("parse oid: {e}")))
}

fn maybe_fault(active: Option<FaultPoint>, point: FaultPoint) -> Result<(), PromotionError> {
    if active == Some(point) {
        return Err(PromotionError::SimulatedCrash);
    }
    Ok(())
}

fn map_post_ref_crash(err: PromotionError) -> PromotionError {
    if matches!(err, PromotionError::SimulatedCrash) {
        PromotionError::RecoveryRequired
    } else {
        err
    }
}
