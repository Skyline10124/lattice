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

use lancet_audit::{CepAuditStore, RewindAuditRecord};
use lancet_native::rewind::{
    RestoredPath, RewindPathConflict, RewindPathPreflight, RewindRestoreError,
    RewindRestoreOutcome, preflight_and_restore,
};
use lancet_runtime::{
    RewindError, RewindStep, WorkspaceHistory, WorkspaceHistoryBacking, WorkspaceHistoryError,
};

use crate::config::audit_db_path_for_project;
use crate::error::{LancetError, LancetResult};

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
    /// Preflight or restore failed outright (not a same-path conflict).
    ///
    /// `steps` carries the planned reversal so callers can still write a
    /// durable audit record of the attempt.
    #[error("rewind restore failed: {source}")]
    Restore {
        /// The executor failure.
        #[source]
        source: RewindRestoreError,
        /// The mutation commits the aborted rewind had planned to reverse.
        steps: Vec<RewindStep>,
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
}

/// Rewind the Local Workspace to `target_history_oid`.
///
/// `project_root` is the canonical Local Workspace root; `backing` locates
/// the private history repository. A `None` target unwinds every recorded
/// mutation and deletes the `current` ref. Record pins are never touched, so
/// every reversed commit stays reachable afterwards.
pub fn rewind_workspace(
    project_root: &Path,
    backing: WorkspaceHistoryBacking,
    target_history_oid: Option<&str>,
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
            Err(WorkspaceRewindError::Restore { source, steps })
        }
        Err(RewindError::History(err)) => Err(WorkspaceRewindError::History(err)),
        Err(RewindError::Disabled) => Err(WorkspaceRewindError::Disabled {
            target: target_history_oid.unwrap_or("<none>").to_string(),
        }),
        Err(RewindError::TargetUnreachable { target }) => {
            Err(WorkspaceRewindError::TargetUnreachable { target })
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

/// Append one durable Rewind audit record to the project's audit database.
///
/// Same direct-store shape as `query_audit`/`store_turn_attestation`: the
/// path is resolved and the store opened inside this crate. The record holds
/// hashes, paths, and object ids only — never file content.
pub fn record_rewind_audit(project_root: &Path, record: &RewindAuditRecord) -> LancetResult<()> {
    let db = audit_db_path_for_project(project_root)
        .map_err(|err| LancetError::Config(format!("resolve audit db path: {err}")))?;
    let store = CepAuditStore::open(&db)
        .map_err(|err| LancetError::Internal(format!("open audit store for rewind: {err}")))?;
    store
        .insert_rewind_record(record)
        .map_err(|err| LancetError::Internal(format!("persist rewind audit record: {err}")))
}

/// All durable Rewind records for a session, oldest first.
pub fn rewind_audit_records(
    project_root: &Path,
    session_id: &str,
) -> LancetResult<Vec<RewindAuditRecord>> {
    let db = audit_db_path_for_project(project_root)
        .map_err(|err| LancetError::Config(format!("resolve audit db path: {err}")))?;
    let store = CepAuditStore::open(&db)
        .map_err(|err| LancetError::Internal(format!("open audit store for rewind: {err}")))?;
    store
        .rewind_records_for_session(session_id)
        .map_err(|err| LancetError::Internal(format!("read rewind audit records: {err}")))
}
