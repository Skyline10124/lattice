// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn lifecycle state machine: workspace-dirty hook, terminal verification,
//! the complete independent outcome classification matrix, and crash-safe
//! disposition finalization.
//!
//! This module centralizes lifecycle ordering and classification.
//! Every terminal condition maps to an explicit `(TurnCommitStatus,
//! WorkspaceDisposition, ResourceAction)` triple — no error-string inference.
//!
//! [`finalize_turn`] orchestrates the journal → retain/release → commit →
//! journal-complete sequence for each disposition.

use std::sync::Arc;

use lattice_agent::{ProvisionalCompletionHook, ProvisionalDecision};
use lattice_session::{GraphMessage, TurnCommitStatus, WorkspaceDisposition};
use lattice_workspace::git_status::{GitStatus, GitStatusEntry};
use lattice_workspace::{UpperHandle, Workspace};

use super::snapshot_journal::{SnapshotJournal, SnapshotJournalRecord, SnapshotReconciler};

/// Internal resource instruction consumed by durable finalization.
///
/// Tells the snapshot manager what to do with the Upper after the turn outcome
/// is determined. Not part of the public session commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceAction {
    /// Destroy the Upper (no snapshot).
    Destroy,
    /// Retain the Upper as a named snapshot for later resume.
    Retain,
    /// Release the Upper after the candidate ref is durably pinned.
    ReleaseAfterCandidatePin,
    /// No resource action (e.g. no workspace turn).
    None,
}

/// Turn lifecycle outcome: the terminal classification of a complete turn.
///
/// Every stage maps to an explicit `(status, disposition, resource_action)`
/// triple. Disposition is never derived from error strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnLifecycleOutcome {
    /// Execution status (completed / error / cancelled).
    pub status: TurnCommitStatus,
    /// Workspace disposition (promoted / pending / rejected / aborted / etc.).
    pub disposition: WorkspaceDisposition,
    /// Lifecycle evidence messages.
    pub messages: Vec<String>,
    /// Resource action for the Upper snapshot lifecycle.
    pub resource_action: ResourceAction,
    /// Whether the candidate was exported before this outcome.
    pub candidate_exported: bool,
    /// Whether verification evidence was acquired.
    pub verification_acquired: bool,
    /// The promoted ref OID, if promotion succeeded.
    pub promoted_ref_committed: Option<String>,
}

/// A turn whose outcome is classified but not yet durably finalized.
///
/// Retains ownership of the Upper handle and audit context until the
/// finalization step commits the session record.
#[derive(Debug)]
pub struct UnfinalizedTurn {
    pub outcome: TurnLifecycleOutcome,
    pub upper_handle: Option<UpperHandle>,
}

/// Workspace-dirty hook for same-loop provisional completion.
///
/// Owns an `Arc<dyn Workspace>` and one `UpperHandle`. When the agent loop
/// reaches a non-tool-call final response, the hook checks workspace
/// cleanliness via `Workspace::git_status`:
///
/// - **Clean:** returns `Complete { cleanup_exhausted: false }`.
/// - **Dirty:** emits a structured `workspace_dirty` system fact and returns
///   `Continue` so the same AgentLoop iteration continues.
/// - **Exhausted:** after `max_cleanup_rounds` dirty checks, returns
///   `Complete { cleanup_exhausted: true }`.
/// - **Error:** propagates as a string error (becomes `AgentTurnStatus::Error`).
///
/// The hook never auto-commits, restores, resets, removes, or discards Upper
/// paths.
pub struct WorkspaceDirtyHook {
    workspace: Arc<dyn Workspace>,
    handle: UpperHandle,
    max_cleanup_rounds: u32,
    completed_checks: u32,
    dirty_rounds: u32,
}

impl WorkspaceDirtyHook {
    /// Create a new hook with the given workspace, handle, and cleanup limit.
    pub fn new(
        workspace: Arc<dyn Workspace>,
        handle: UpperHandle,
        max_cleanup_rounds: u32,
    ) -> Self {
        Self {
            workspace,
            handle,
            max_cleanup_rounds,
            completed_checks: 0,
            dirty_rounds: 0,
        }
    }

    /// Return the number of dirty rounds observed so far.
    pub fn dirty_rounds(&self) -> u32 {
        self.dirty_rounds
    }

    /// Return the total number of completed checks.
    pub fn completed_checks(&self) -> u32 {
        self.completed_checks
    }
}

impl std::fmt::Debug for WorkspaceDirtyHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceDirtyHook")
            .field("max_cleanup_rounds", &self.max_cleanup_rounds)
            .field("completed_checks", &self.completed_checks)
            .field("dirty_rounds", &self.dirty_rounds)
            .finish()
    }
}

#[async_trait::async_trait]
impl ProvisionalCompletionHook for WorkspaceDirtyHook {
    async fn on_provisional_complete(
        &mut self,
        _assistant: &GraphMessage,
    ) -> Result<ProvisionalDecision, String> {
        self.completed_checks += 1;

        let status: GitStatus = self
            .workspace
            .git_status(&self.handle)
            .await
            .map_err(|e| format!("workspace git_status failed: {e}"))?;

        if status.is_clean {
            return Ok(ProvisionalDecision::Complete {
                cleanup_exhausted: false,
                final_message: None,
            });
        }

        // Dirty: build structured workspace_dirty fact.
        self.dirty_rounds += 1;
        let fact = GitStatusEntry::workspace_dirty_fact(&status.entries);
        let system_msg = GraphMessage {
            role: "system".to_string(),
            content: fact,
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        };

        if self.dirty_rounds >= self.max_cleanup_rounds {
            // Exhausted: return Complete with cleanup_exhausted=true and carry
            // the final workspace-dirty fact as `final_message` so the loop
            // appends it to the transcript. This preserves the
            // diagnostic evidence of what was dirty when cleanup was exhausted
            // — critical for understanding a PendingWorktree disposition.
            return Ok(ProvisionalDecision::Complete {
                cleanup_exhausted: true,
                final_message: Some(system_msg),
            });
        }

        Ok(ProvisionalDecision::Continue(system_msg))
    }
}

/// Run terminal verification on an exported candidate.
///
/// Executes the verification argv inside the live Upper, constructs a
/// `VerificationResult`, and checks post-export HEAD stability.
///
/// Returns `None` if verification config is absent (pending_verification,
/// no fabricated evidence).
///
/// Returns the `VerificationResult` if verification was executed, regardless
/// of pass/fail.
pub async fn run_terminal_verification(
    workspace: &dyn Workspace,
    handle: &UpperHandle,
    candidate_oid: &str,
    profile_hash: &str,
    verify_argv: Option<&[String]>,
    expected_exit_code: i32,
) -> Result<Option<lattice_workspace::promotion::VerificationResult>, String> {
    let Some(argv) = verify_argv else {
        // no verify config → pending_verification, no evidence.
        return Ok(None);
    };

    if argv.is_empty() {
        return Ok(None);
    }

    // Execute verification inside the Upper.
    let prog = argv[0].as_str();
    let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();

    let output = workspace
        .exec_upper(handle, prog, &args)
        .await
        .map_err(|e| format!("verification exec failed: {e}"))?;

    let exit_code = output.exit_code;
    let passed = exit_code == expected_exit_code;
    let summary = if passed {
        "verification passed".to_string()
    } else {
        format!("verification failed: exit {exit_code} (expected {expected_exit_code})")
    };

    let result = lattice_workspace::promotion::VerificationResult::new(
        candidate_oid.to_string(),
        argv.to_vec(),
        profile_hash.to_string(),
        expected_exit_code,
        Some(exit_code),
        passed,
        summary,
    )
    .map_err(|e| format!("verification result construction failed: {e}"))?;

    Ok(Some(result))
}

/// Classify a lifecycle failure into a typed outcome.
///
/// Centralizes all stage-to-outcome mapping. Never derives
/// disposition from generic error strings or solely from agent status.
pub fn classify_lifecycle_failure(
    status: TurnCommitStatus,
    candidate_exported: bool,
    verification_acquired: bool,
    failure_kind: LifecycleFailureKind,
) -> TurnLifecycleOutcome {
    let (disposition, resource_action, messages) = match failure_kind {
        LifecycleFailureKind::WorkspaceLess => (
            WorkspaceDisposition::NotApplicable,
            ResourceAction::None,
            vec!["no workspace configured".to_string()],
        ),
        LifecycleFailureKind::CleanupExhaustion => (
            WorkspaceDisposition::PendingWorktree,
            ResourceAction::Retain,
            vec!["cleanup rounds exhausted".to_string()],
        ),
        LifecycleFailureKind::MissingVerifyConfig => (
            WorkspaceDisposition::PendingVerification,
            ResourceAction::Retain,
            vec!["no verification configuration found".to_string()],
        ),
        LifecycleFailureKind::VerifyFailed => (
            WorkspaceDisposition::PendingVerification,
            ResourceAction::Retain,
            vec!["verification failed".to_string()],
        ),
        LifecycleFailureKind::VerifyTimeout => (
            WorkspaceDisposition::PendingVerification,
            ResourceAction::Retain,
            vec!["verification timed out".to_string()],
        ),
        LifecycleFailureKind::PostExportHeadChange => (
            WorkspaceDisposition::PendingVerification,
            ResourceAction::Retain,
            vec!["post-export HEAD change detected".to_string()],
        ),
        LifecycleFailureKind::PinnedTemporarilyBlocked => (
            WorkspaceDisposition::PendingPromotion,
            ResourceAction::ReleaseAfterCandidatePin,
            vec!["promotion temporarily blocked; candidate pinned".to_string()],
        ),
        LifecycleFailureKind::BaselineMovement => (
            WorkspaceDisposition::PendingConflict,
            ResourceAction::None,
            vec!["baseline moved during promotion".to_string()],
        ),
        LifecycleFailureKind::ExportRejected { reason } => (
            WorkspaceDisposition::Rejected,
            ResourceAction::Destroy,
            vec![format!("export rejected: {reason}")],
        ),
        LifecycleFailureKind::ValidationRejected { reason } => (
            WorkspaceDisposition::Rejected,
            ResourceAction::Destroy,
            vec![format!("validation rejected: {reason}")],
        ),
        LifecycleFailureKind::PolicyRejected { reason } => (
            WorkspaceDisposition::Rejected,
            ResourceAction::Destroy,
            vec![format!("policy rejected: {reason}")],
        ),
        LifecycleFailureKind::AgentErrorBeforeCandidate => (
            WorkspaceDisposition::Aborted,
            ResourceAction::Destroy,
            vec!["agent error before candidate export".to_string()],
        ),
        LifecycleFailureKind::CancelledBeforeCandidate => (
            WorkspaceDisposition::Aborted,
            ResourceAction::Destroy,
            vec!["cancelled before candidate export".to_string()],
        ),
    };

    TurnLifecycleOutcome {
        status,
        disposition,
        messages,
        resource_action,
        candidate_exported,
        verification_acquired,
        promoted_ref_committed: None,
    }
}

/// Failure classification for lifecycle outcome mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleFailureKind {
    /// No workspace configured (completed + not_applicable).
    WorkspaceLess,
    /// Cleanup rounds exhausted (completed + pending_worktree).
    CleanupExhaustion,
    /// No verify configuration (completed + pending_verification).
    MissingVerifyConfig,
    /// Verification failed (completed + pending_verification).
    VerifyFailed,
    /// Verification timed out (completed + pending_verification).
    VerifyTimeout,
    /// Post-export HEAD change (completed + pending_verification).
    PostExportHeadChange,
    /// Promotion temporarily blocked, candidate pinned (completed + pending_promotion).
    PinnedTemporarilyBlocked,
    /// Baseline moved during promotion (completed + pending_conflict).
    BaselineMovement,
    /// Export rejected by workspace (error + rejected).
    ExportRejected { reason: String },
    /// Candidate validation rejected (error + rejected).
    ValidationRejected { reason: String },
    /// Policy rejection (error + rejected).
    PolicyRejected { reason: String },
    /// Agent error before candidate export (error + aborted).
    AgentErrorBeforeCandidate,
    /// Cancelled before candidate export (cancelled + aborted).
    CancelledBeforeCandidate,
}

/// Build a successful promoted outcome.
pub fn outcome_promoted(promoted_ref: String, candidate_exported: bool) -> TurnLifecycleOutcome {
    TurnLifecycleOutcome {
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::Promoted,
        messages: vec!["candidate promoted".to_string()],
        resource_action: ResourceAction::ReleaseAfterCandidatePin,
        candidate_exported,
        verification_acquired: true,
        promoted_ref_committed: Some(promoted_ref),
    }
}

/// Build a workspace-less completed outcome (not_applicable).
pub fn outcome_workspace_less() -> TurnLifecycleOutcome {
    TurnLifecycleOutcome {
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        messages: vec!["no workspace configured".to_string()],
        resource_action: ResourceAction::None,
        candidate_exported: false,
        verification_acquired: false,
        promoted_ref_committed: None,
    }
}

// ─── Durable Finalization ───────────────────────────────────────────────────

/// Orchestrate crash-safe journal, retain/release, and commit for a terminal
/// turn outcome.
///
/// The exact order per resource action:
/// - **Retain:** journal intent → retain snapshot → journal retained → commit →
///   journal complete.
/// - **ReleaseAfterCandidatePin:** candidate pin (caller prerequisite) →
///   release snapshot → commit.
/// - **Destroy:** destroy Upper → commit.
/// - **None:** commit only.
///
/// For `pending_conflict`: no snapshot is ever retained.
/// Never rewrites a committed lifecycle row.
///
/// `prepared_commit_id` is the pre-allocated commit identity (UUID) used
/// consistently across all journal records so they group as a single entry
/// during crash recovery. The caller generates this before starting the
/// finalization sequence.
///
/// `commit_fn` is called with the outcome and snapshot name to produce the
/// immutable session commit ID. The caller is responsible for constructing
/// `TurnCommitInput` with the correct lifecycle evidence.
pub fn finalize_turn(
    outcome: &TurnLifecycleOutcome,
    session_id: &str,
    snapshot_name: &str,
    prepared_commit_id: &str,
    journal: &mut SnapshotJournal,
    reconciler: &mut dyn SnapshotReconciler,
    commit_fn: impl FnOnce(&TurnLifecycleOutcome, &str) -> Result<String, String>,
) -> Result<String, String> {
    let commit_id = match outcome.resource_action {
        ResourceAction::Retain => {
            // journal intent BEFORE snapshot creation.
            journal.append_record(SnapshotJournalRecord::Intent {
                session_id: session_id.to_string(),
                prepared_commit_id: prepared_commit_id.to_string(),
                snapshot_name: snapshot_name.to_string(),
            })?;

            // Caller must have already performed the actual snapshot retention
            // via UpperLease::retain. Journal the retained state BEFORE commit.
            journal.append_record(SnapshotJournalRecord::Retained {
                session_id: session_id.to_string(),
                prepared_commit_id: prepared_commit_id.to_string(),
                snapshot_name: snapshot_name.to_string(),
            })?;

            // Commit the session.
            let cid = commit_fn(outcome, snapshot_name)?;

            // Journal complete AFTER successful commit.
            journal.append_record(SnapshotJournalRecord::SessionCommitted {
                session_id: session_id.to_string(),
                commit_id: cid.clone(),
                snapshot_name: snapshot_name.to_string(),
            })?;

            cid
        }
        ResourceAction::ReleaseAfterCandidatePin => {
            // candidate pin is durable before release (caller prerequisite).
            // Release the snapshot now.
            if reconciler.snapshot_exists(snapshot_name)? {
                reconciler.release_snapshot(snapshot_name)?;
            }

            // Commit the session.
            commit_fn(outcome, snapshot_name)?
        }
        ResourceAction::Destroy => {
            // Destroy is handled by the caller via UpperLease::destroy.
            // Just commit the session.
            commit_fn(outcome, snapshot_name)?
        }
        ResourceAction::None => {
            // No resource action; just commit.
            // For pending_conflict: NO snapshot is retained.
            commit_fn(outcome, snapshot_name)?
        }
    };

    Ok(commit_id)
}

/// Commit a promoted outcome with one retry on failure.
///
/// The promotion ref transaction is authoritative. If the first SQLite commit
/// fails, the exact same `commit_fn` is retried once. After a second failure,
/// the error is returned without attempting to roll back the Git refs.
///
/// Both attempts use the same prepared commit identity (same commit ID).
pub fn commit_promoted_with_retry(
    outcome: &TurnLifecycleOutcome,
    snapshot_name: &str,
    commit_fn: impl FnMut(&TurnLifecycleOutcome, &str) -> Result<String, String>,
) -> Result<String, String> {
    let mut commit_fn = commit_fn;
    match commit_fn(outcome, snapshot_name) {
        Ok(id) => Ok(id),
        Err(first_error) => {
            // retry once with the same prepared commit.
            match commit_fn(outcome, snapshot_name) {
                Ok(id) => Ok(id),
                Err(second_error) => Err(format!(
                    "promoted commit failed after retry: first={first_error}, second={second_error}"
                )),
            }
        }
    }
}
