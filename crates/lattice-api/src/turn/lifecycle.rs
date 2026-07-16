// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn lifecycle terminal classification and crash-safe disposition
//! finalization.
//!
//! #37 removed the Upper-bound half of this module (workspace-dirty hook,
//! terminal verification, promotion outcomes, promoted-commit retry). What
//! remains is the terminal-classification vocabulary used by the live turn
//! path (`super::live`) to derive an explicit `(status, disposition,
//! resource_action)` triple for every committed turn, plus the
//! journal/retain/release/commit orchestrator used by non-promotion
//! finalization paths.
//!
//! Every terminal condition maps to an explicit `(TurnCommitStatus,
//! WorkspaceDisposition, ResourceAction)` triple — no error-string inference.

use lattice_session::{TurnCommitStatus, WorkspaceDisposition};

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
            ResourceAction::Destroy,
            vec!["no workspace configured".to_string()],
        ),
        LifecycleFailureKind::CleanupExhaustion => (
            WorkspaceDisposition::PendingWorktree,
            ResourceAction::Retain,
            vec!["cleanup rounds exhausted".to_string()],
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
            ResourceAction::Destroy,
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

/// Build a workspace-less completed outcome (not_applicable).
pub fn outcome_workspace_less() -> TurnLifecycleOutcome {
    TurnLifecycleOutcome {
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        messages: vec!["no workspace configured".to_string()],
        resource_action: ResourceAction::Destroy,
        candidate_exported: false,
        verification_acquired: false,
        promoted_ref_committed: None,
    }
}

// ─── Durable Finalization ───────────────────────────────────────────────────

/// Commit a terminal turn outcome through the caller-supplied commit
/// function.
///
/// #37 removed the Upper snapshot journal and retain/release paths; only the
/// Direct-Local Destroy path remains in production, so this is now a thin
/// delegate over `commit_fn`. The `session_id` / `snapshot_name` /
/// `prepared_commit_id` parameters are retained for signature stability
/// until #40 reworks the session lifecycle contract; they are unused on the
/// Destroy path. Never rewrites a committed lifecycle row.
pub fn finalize_turn<F>(
    outcome: &TurnLifecycleOutcome,
    session_id: &str,
    snapshot_name: &str,
    prepared_commit_id: &str,
    commit_fn: F,
) -> Result<String, String>
where
    F: FnOnce(&TurnLifecycleOutcome, &str) -> Result<String, String>,
{
    let _ = (session_id, snapshot_name, prepared_commit_id);
    // The Retain/Release paths depended on the Upper snapshot journal which
    // #37 deleted. Only the Direct-Local Destroy path remains in production.
    commit_fn(outcome, snapshot_name)
}
