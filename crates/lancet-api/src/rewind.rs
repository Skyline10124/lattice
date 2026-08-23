// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Workspace-side explicit Rewind (GitHub #64).
//!
//! Operator surface in the `query_audit` tradition: a direct composition of
//! the private [`WorkspaceHistory`] and the lancet-native restore executor,
//! never an Agent Tool and never a CEP method. The session-side half —
//! resolving the target Turn and moving the Session History ref — lives
//! above this crate (lancet-api does not see `lattice-session`).
//!
//! Sequence, all under the history's recording lock: plan the reversal from
//! `current` back to the target identity, preflight every affected path
//! against its recorded post-state, restore parent-tree content, then
//! compare-and-set the private `current` ref backwards. A structured
//! same-path conflict aborts before anything is written; a ref durability
//! failure after the restore is an error, never a success.

use std::cell::RefCell;
use std::path::Path;

use lancet_native::rewind::{
    RestoredPath, RewindPathConflict, RewindPathPreflight, RewindRestoreError,
    RewindRestoreOutcome, preflight_and_restore,
};
use lancet_runtime::{
    RewindError, RewindStep, WorkspaceHistory, WorkspaceHistoryBacking, WorkspaceHistoryError,
};

pub use lancet_runtime::rewind_audit::{RewindAuditLog, RewindAuditRecord};

use crate::error::{LancetError, LancetResult};

/// The exclusive `WorkspaceOperationLock` a product-level `RewindCoordinator`
/// holds across the whole workflow (#139-B Commit 3).
///
/// Lower workspace/history operations accept this guard and never reacquire
/// the lock: ownership lives exactly once, in the coordinator.
pub enum HeldWorkspaceOperation {
    /// No project binding to serialize against (disabled backing).
    None,
    /// The held exclusive guard.
    Exclusive(lancet_runtime::FlockGuard),
}

impl HeldWorkspaceOperation {
    /// Acquire the exclusive project operation lock for `backing`.
    pub fn acquire(backing: &WorkspaceHistoryBacking) -> Result<Self, WorkspaceHistoryError> {
        let history = WorkspaceHistory::open(backing.clone())?;
        match history.lock_workspace_operation(
            lancet_runtime::LockMode::Exclusive,
            lancet_runtime::Blocking::Block,
        )? {
            Some(guard) => Ok(Self::Exclusive(guard)),
            None => Ok(Self::None),
        }
    }
}

/// A successfully applied workspace rewind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRewindReport {
    /// Identity the rewind started from (`None` = empty history).
    pub from_history_oid: Option<String>,
    /// Identity the rewind moved to (`None` = empty history).
    pub to_history_oid: Option<String>,
    /// Reversed mutation commits, newest first.
    pub steps: Vec<RewindStep>,
    /// Preflight observations, one per affected path.
    pub preflight: Vec<RewindPathPreflight>,
    /// Applied restores, one per affected path.
    pub restored: Vec<RestoredPath>,
}

/// A rewind refused because at least one path diverged.
///
/// Nothing moved: not the Local Workspace, not the private history ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRewindConflict {
    /// Identity the rewind would have started from.
    pub from_history_oid: Option<String>,
    /// Identity the rewind would have moved to.
    pub to_history_oid: Option<String>,
    /// The mutation commits that would have been reversed.
    pub steps: Vec<RewindStep>,
    /// Preflight observations, one per affected path.
    pub preflight: Vec<RewindPathPreflight>,
    /// The divergent paths.
    pub conflicts: Vec<RewindPathConflict>,
}

/// Outcome of the workspace-side rewind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceRewindOutcome {
    /// Every path matched and was restored; the history ref moved.
    Applied(WorkspaceRewindReport),
    /// A same-path divergence stopped the rewind before any write.
    Conflict(WorkspaceRewindConflict),
}

/// Failure of the workspace-side rewind.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceRewindError {
    /// Reading or writing the private history repository failed.
    #[error(transparent)]
    History(#[from] WorkspaceHistoryError),
    /// The target names a concrete identity but recording is disabled.
    #[error("workspace history is disabled; cannot rewind to '{target}'")]
    Disabled {
        /// The requested target identity.
        target: String,
    },
    /// The target identity is not on the chain reachable from `current`.
    #[error("rewind target '{target}' is not reachable from the current history identity")]
    TargetUnreachable {
        /// The requested target identity.
        target: String,
    },
    /// The plan does not cover every path the reversed range changed.
    ///
    /// Nothing was written and no ref moved: a Rewind that cannot account for
    /// a mutated path must not claim to have restored the workspace.
    #[error("the rewind plan does not cover changed path(s): {paths:?}")]
    PlanIncomplete {
        /// The uncovered paths.
        paths: Vec<String>,
    },
    /// Preflight or restore failed outright (not a same-path conflict).
    ///
    /// `steps` carries the planned reversal and `restored` the paths phase 2
    /// had already written, so callers can record the partial state.
    #[error("rewind restore failed: {source}")]
    Restore {
        /// The executor failure.
        #[source]
        source: RewindRestoreError,
        /// The mutation commits the aborted rewind had planned to reverse.
        steps: Vec<RewindStep>,
        /// Paths already restored when the failure hit.
        restored: Vec<RestoredPath>,
    },
    /// Paths were restored but the history ref did not move durably.
    ///
    /// Callers must never report this as success: the Local Workspace holds
    /// the restored state while `current` still names the old identity.
    /// `report` carries everything that was applied so callers can write a
    /// durable audit record of the partial state.
    #[error("workspace restored but the private history ref move failed: {detail}")]
    RefMove {
        /// The underlying ref-transaction failure.
        detail: String,
        /// What the rewind had already applied when the ref move failed.
        ///
        /// Boxed so the success path is not paying for the failure payload.
        report: Box<WorkspaceRewindReport>,
    },
    /// The caller did not hold the project operation lock for a project
    /// backing. The coordinator owns that lock; lower layers never acquire
    /// it themselves (#139-B Commit 3).
    #[error("rewind against a project workspace requires the held WorkspaceOperationLock")]
    MissingOperationLock,
}

/// Plan — without applying — the reversal back to `target_history_oid`.
///
/// Read-only: it opens the private history, walks the chain, and returns what
/// a Rewind *would* reverse. Callers use it to decide whether a Rewind is
/// admissible before anything is verified or written.
pub fn plan_workspace_rewind(
    backing: WorkspaceHistoryBacking,
    target_history_oid: Option<&str>,
) -> Result<lancet_runtime::RewindPlan, WorkspaceRewindError> {
    let history = WorkspaceHistory::open(backing)?;
    if !history.is_enabled() {
        return Err(WorkspaceRewindError::Disabled {
            target: target_history_oid.unwrap_or("<none>").to_string(),
        });
    }
    match history.plan_rewind(target_history_oid) {
        Ok(plan) => Ok(plan),
        Err(RewindError::History(err)) => Err(WorkspaceRewindError::History(err)),
        Err(RewindError::Disabled) => Err(WorkspaceRewindError::Disabled {
            target: target_history_oid.unwrap_or("<none>").to_string(),
        }),
        Err(RewindError::TargetUnreachable { target }) => {
            Err(WorkspaceRewindError::TargetUnreachable { target })
        }
        Err(RewindError::PlanIncomplete { paths }) => {
            Err(WorkspaceRewindError::PlanIncomplete { paths })
        }
        Err(RewindError::Apply(never)) => match never {},
        Err(RewindError::RefMove(detail)) => Err(WorkspaceRewindError::RefMove {
            detail,
            report: Box::new(WorkspaceRewindReport {
                from_history_oid: None,
                to_history_oid: target_history_oid.map(str::to_string),
                steps: Vec::new(),
                preflight: Vec::new(),
                restored: Vec::new(),
            }),
        }),
    }
}

/// Rewind the Local Workspace to `target_history_oid`.
///
/// `project_root` is the canonical Local Workspace root; `backing` locates
/// the private history repository; `held` is the exclusive
/// `WorkspaceOperationLock` the product-level `RewindCoordinator` acquired
/// and holds across preflight, restore, the history ref move, the Session
/// ref move, and the audit append — lower layers never reacquire it
/// (#139-B Commit 3). A `None` target unwinds every recorded mutation and
/// deletes the `current` ref. Record pins are never touched, so every
/// reversed commit stays reachable afterwards.
pub fn rewind_workspace(
    project_root: &Path,
    backing: WorkspaceHistoryBacking,
    target_history_oid: Option<&str>,
    held: &HeldWorkspaceOperation,
) -> Result<WorkspaceRewindOutcome, WorkspaceRewindError> {
    let history = WorkspaceHistory::open(backing)?;
    if !history.is_enabled() {
        return match target_history_oid {
            // No history and no concrete target: there is nothing to reverse
            // and no ref to move — trivially applied.
            None => Ok(WorkspaceRewindOutcome::Applied(WorkspaceRewindReport {
                from_history_oid: None,
                to_history_oid: None,
                steps: Vec::new(),
                preflight: Vec::new(),
                restored: Vec::new(),
            })),
            Some(target) => Err(WorkspaceRewindError::Disabled {
                target: target.to_string(),
            }),
        };
    }
    // The guard is held by the caller for a project backing; accepting a
    // rewind without it would silently run unserialized against live
    // mutations.
    if matches!(held, HeldWorkspaceOperation::None) {
        return Err(WorkspaceRewindError::MissingOperationLock);
    }

    /// Why the apply closure aborted.
    enum Abort {
        Conflict,
        Restore(RewindRestoreError),
    }

    // Steps are captured before the restore runs so every failure mode —
    // conflict, restore error, ref-move failure — can still surface what the
    // rewind had planned; the outcome joins once the executor returns.
    let captured_steps: RefCell<Vec<RewindStep>> = RefCell::new(Vec::new());
    let captured_from: RefCell<Option<String>> = RefCell::new(None);
    let captured: RefCell<Option<RewindRestoreOutcome>> = RefCell::new(None);
    let result = history.rewind_with(target_history_oid, |plan| {
        *captured_steps.borrow_mut() = plan.steps.clone();
        *captured_from.borrow_mut() = plan.from.clone();
        let outcome =
            preflight_and_restore(project_root, &plan.restores).map_err(Abort::Restore)?;
        let conflicted = matches!(outcome, RewindRestoreOutcome::Conflict { .. });
        *captured.borrow_mut() = Some(outcome);
        if conflicted {
            Err(Abort::Conflict)
        } else {
            Ok(())
        }
    });

    let steps = captured_steps.into_inner();
    match result {
        Ok(plan) => {
            let RewindRestoreOutcome::Applied {
                preflight,
                restored,
            } = captured
                .into_inner()
                .expect("apply ran for a successful rewind")
            else {
                unreachable!("a conflicted restore aborts the rewind");
            };
            Ok(WorkspaceRewindOutcome::Applied(WorkspaceRewindReport {
                from_history_oid: plan.from,
                to_history_oid: plan.to,
                steps,
                preflight,
                restored,
            }))
        }
        Err(RewindError::Apply(Abort::Conflict)) => {
            let RewindRestoreOutcome::Conflict {
                preflight,
                conflicts,
            } = captured
                .into_inner()
                .expect("apply captured the conflict before aborting")
            else {
                unreachable!("only a conflicted restore aborts with Conflict");
            };
            Ok(WorkspaceRewindOutcome::Conflict(WorkspaceRewindConflict {
                from_history_oid: history.current()?,
                to_history_oid: target_history_oid.map(str::to_string),
                steps,
                preflight,
                conflicts,
            }))
        }
        Err(RewindError::Apply(Abort::Restore(source))) => {
            let restored = match &source {
                RewindRestoreError::Io { restored, .. } => restored.clone(),
                _ => Vec::new(),
            };
            Err(WorkspaceRewindError::Restore {
                source,
                steps,
                restored,
            })
        }
        Err(RewindError::History(err)) => Err(WorkspaceRewindError::History(err)),
        Err(RewindError::Disabled) => Err(WorkspaceRewindError::Disabled {
            target: target_history_oid.unwrap_or("<none>").to_string(),
        }),
        Err(RewindError::TargetUnreachable { target }) => {
            Err(WorkspaceRewindError::TargetUnreachable { target })
        }
        Err(RewindError::PlanIncomplete { paths }) => {
            Err(WorkspaceRewindError::PlanIncomplete { paths })
        }
        Err(RewindError::RefMove(detail)) => {
            let RewindRestoreOutcome::Applied {
                preflight,
                restored,
            } = captured
                .into_inner()
                .expect("the ref move only runs after a successful apply")
            else {
                unreachable!("a conflicted restore aborts before the ref move");
            };
            Err(WorkspaceRewindError::RefMove {
                detail,
                report: Box::new(WorkspaceRewindReport {
                    from_history_oid: captured_from.into_inner(),
                    to_history_oid: target_history_oid.map(str::to_string),
                    steps,
                    preflight,
                    restored,
                }),
            })
        }
    }
}

/// Append one durable Rewind audit record to the project's independent
/// append-only chain in `workspace-history.git` (#139-B Commit 3).
///
/// The record holds hashes, paths, and object ids only — never file content.
/// The caller still holds the project `WorkspaceOperationLock`; this is the
/// separate short repository transaction after the Session ref attempt.
pub fn record_rewind_audit(
    backing: &WorkspaceHistoryBacking,
    record: &RewindAuditRecord,
) -> LancetResult<()> {
    let log = RewindAuditLog::open(backing)
        .map_err(|err| LancetError::Internal(format!("open rewind audit chain: {err}")))?;
    log.append(record)
        .map_err(|err| LancetError::Internal(format!("persist rewind audit record: {err}")))
}

/// Bounded read of the newest Rewind audit records for one session.
pub fn rewind_audit_records(
    backing: &WorkspaceHistoryBacking,
    session_id: &str,
) -> LancetResult<Vec<RewindAuditRecord>> {
    let log = RewindAuditLog::open(backing)
        .map_err(|err| LancetError::Internal(format!("open rewind audit chain: {err}")))?;
    log.records_for_session(
        session_id,
        lancet_runtime::rewind_audit::MAX_REWIND_AUDIT_READ,
    )
    .map_err(|err| LancetError::Internal(format!("read rewind audit records: {err}")))
}
