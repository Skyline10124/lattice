// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Explicit Rewind (GitHub #64; `RewindCoordinator` shape from #139-B
//! Commit 3).
//!
//! The product-level entry point: the user selects an earlier Turn of the
//! current session and LATTICE reverses only the Atomic Local Mutations it
//! successfully installed after that Turn. External changes LATTICE never
//! touched survive; a LATTICE-touched path that has drifted from the recorded
//! post-mutation content makes the whole Rewind report a structured conflict
//! instead of overwriting the user's content.
//!
//! Strict order, nothing partial on the failure paths:
//!
//! 1. Resolve the target Turn **in this session**. A commit that does not
//!    exist, belongs to another session, or is not reachable from the current
//!    session HEAD is refused before anything is read.
//! 2. Resolve the Turn's workspace-history identity, refusing a Turn whose
//!    identity is unavailable rather than guessing (see
//!    [`effective_history_target`]).
//! 3. Acquire the exclusive per-project `WorkspaceOperationLock` exactly
//!    once, here in the coordinator (#139-B Commit 3), and hold it across
//!    the workspace preflight/restore, the private history ref movement, the
//!    Session History ref movement, and the audit append. Lower layers
//!    accept the held guard and never reacquire it.
//! 4. Hand the workspace half to `lancet_api::rewind_workspace`: plan the
//!    reversal, verify every affected path against its recorded post-state,
//!    restore parent-tree content, then move the private history ref.
//! 5. Move the Session History ref, after pinning the abandoned head so the
//!    rewound-away Turns keep an internal ref.
//! 6. Append one durable record to the independent
//!    `refs/lattice/rewind-audit/log` chain — for success, conflict, and
//!    every failure mode that touched the Local Workspace — while the
//!    operation lock is still held. A failed append returns the typed
//!    [`RewindError::AuditPublicationFailed`] partial state, never success.
//!
//! This is a free function rather than a [`SessionApi`] method: the session
//! facade stays pointer-movement only, and Rewind needs the Local Workspace
//! and the private history, neither of which a session store owns.

use std::path::Path;

use lancet_api::{
    HeldWorkspaceOperation, REWIND_AUDIT_SCHEMA_VERSION, RewindAuditRecord,
    WorkspaceHistoryBacking, WorkspaceRewindError, WorkspaceRewindOutcome, plan_workspace_rewind,
    record_rewind_audit, rewind_audit_records, rewind_workspace,
};
use lattice_session::GraphHead;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::session::{ApiError, Result, SessionApi};

/// One reversed mutation commit, as reported to the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindStep {
    /// The private-history commit being reversed.
    pub commit_id: String,
    /// CEP method that installed the mutation.
    pub method: String,
    /// Turn the mutation belonged to.
    pub turn_id: String,
    /// CEP call id within that Turn.
    pub call_id: Option<u64>,
    /// Workspace-relative path the mutation touched.
    pub path: String,
    /// Local Revision recorded right after the mutation installed.
    pub recorded_content_hash: Option<String>,
}

/// What preflight observed for one affected path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindPathPreflight {
    /// Workspace-relative path.
    pub path: String,
    /// Local Revision recorded for the path (`None` = recorded as absent).
    pub expected_revision: Option<String>,
    /// Local Revision observed on disk (`None` = absent or not a file).
    pub observed_revision: Option<String>,
    /// Whether the observation diverged from the recorded post-state.
    pub diverged: bool,
}

/// One path the restore phase wrote or removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindRestoredPath {
    /// Workspace-relative path.
    pub path: String,
    /// Local Revision after the restore (`None` = the file was removed).
    pub restored_revision: Option<String>,
}

/// One path whose current Local Revision diverged from the recorded result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindPathConflict {
    /// Workspace-relative path.
    pub path: String,
    /// Local Revision recorded for the path.
    pub expected_revision: Option<String>,
    /// Local Revision observed on disk.
    pub observed_revision: Option<String>,
    /// Why the path diverged. Never file content.
    pub detail: String,
}

/// How the Session History ref moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionHeadMove {
    /// Branch that moved, or `None` when HEAD was detached.
    pub branch: Option<String>,
    /// Commit the session HEAD pointed at before the Rewind.
    pub previous_commit_id: Option<String>,
    /// Commit the session HEAD points at now (the target Turn).
    pub new_commit_id: String,
    /// Internal ref pinning the abandoned head, when one was created.
    pub pinned_ref: Option<String>,
}

/// A completed Rewind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindSuccess {
    /// Resolved session id.
    pub session_id: String,
    /// The selected target Turn.
    pub target_commit_id: String,
    /// Workspace-history identity the Rewind started from.
    pub from_history_oid: Option<String>,
    /// Workspace-history identity the Rewind moved to.
    pub to_history_oid: Option<String>,
    /// Reversed mutation commits, newest first.
    pub steps: Vec<RewindStep>,
    /// Preflight observations, one per affected path.
    pub preflight: Vec<RewindPathPreflight>,
    /// Restored paths.
    pub restored: Vec<RewindRestoredPath>,
    /// How the Session History ref moved.
    pub session_head: SessionHeadMove,
}

/// A Rewind refused because at least one LATTICE-touched path diverged.
///
/// Nothing moved: not the Local Workspace, not the private history ref, not
/// the Session History ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindConflict {
    /// Resolved session id.
    pub session_id: String,
    /// The selected target Turn.
    pub target_commit_id: String,
    /// Workspace-history identity the Rewind would have started from.
    pub from_history_oid: Option<String>,
    /// Workspace-history identity the Rewind would have moved to.
    pub to_history_oid: Option<String>,
    /// The mutation commits the Rewind would have reversed.
    pub steps: Vec<RewindStep>,
    /// Preflight observations, one per affected path.
    pub preflight: Vec<RewindPathPreflight>,
    /// The divergent paths.
    pub conflicts: Vec<RewindPathConflict>,
}

/// Outcome of an explicit Rewind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RewindOutcome {
    /// The workspace and both refs moved to the target Turn.
    Success(RewindSuccess),
    /// A same-path divergence stopped the Rewind; nothing moved.
    Conflict(RewindConflict),
}

/// The durable state a Rewind had reached when its audit record could not be
/// published (#139-B Commit 3). Everything here already happened; the caller
/// must not retry the Rewind blindly, because the actions are not
/// transactional across stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindPartialState {
    /// Resolved session id.
    pub session_id: String,
    /// The selected target Turn.
    pub target_commit_id: String,
    /// The outcome label whose record failed to publish.
    pub outcome: String,
    /// Workspace-history identity the workflow started from.
    pub from_history_oid: Option<String>,
    /// Workspace-history identity the workflow moved to.
    pub to_history_oid: Option<String>,
    /// Reversed (or planned) mutation commits, newest first.
    pub steps: Vec<RewindStep>,
    /// Preflight observations, one per affected path.
    pub preflight: Vec<RewindPathPreflight>,
    /// Restored paths.
    pub restored: Vec<RewindRestoredPath>,
    /// Divergent paths, when the outcome is a conflict.
    pub conflicts: Vec<RewindPathConflict>,
    /// Whether the Session History ref had already moved.
    pub session_head_moved: bool,
}

/// Error of the product-level Rewind workflow.
#[derive(Debug, thiserror::Error)]
pub enum RewindError {
    /// A refused or failed workflow, before or during its actions.
    #[error(transparent)]
    Api(#[from] ApiError),
    /// The Rewind's durable actions completed but its audit record could not
    /// be appended to the independent chain. Never reported as success: the
    /// partial state is returned so the operator can reconcile.
    #[error("rewind actions completed but the audit record could not be published: {detail}")]
    AuditPublicationFailed {
        /// Why the append failed.
        detail: String,
        /// What had already happened when it failed.
        partial: Box<RewindPartialState>,
    },
}

/// Durable facts a [`RewindBarriers`] observer sees at one phase boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindBarrierFacts {
    /// Resolved session id.
    pub session_id: String,
    /// The selected target Turn.
    pub target_commit_id: String,
    /// The workflow outcome label reached so far.
    pub outcome: String,
    /// Records currently on the independent audit chain (`0` when the
    /// backing is disabled). Read live from the chain, so a barrier can
    /// assert the exact durable state it observes.
    pub audit_records_on_chain: usize,
}

/// Deterministic phase barriers for tests (#139-B Commit 3).
///
/// Production passes [`RewindBarriers::none`]. Each observer runs at a
/// specific action → audit window, after the named action is durably
/// complete and before the workflow continues.
#[derive(Default)]
pub struct RewindBarriers<'a> {
    /// After the workspace half (restore + history ref movement).
    pub after_workspace_half: Option<&'a dyn Fn(&RewindBarrierFacts)>,
    /// After the Session History ref movement.
    pub after_session_ref_move: Option<&'a dyn Fn(&RewindBarrierFacts)>,
    /// After the audit append, before the workflow returns.
    pub after_audit_append: Option<&'a dyn Fn(&RewindBarrierFacts)>,
}

impl RewindBarriers<'_> {
    /// No barriers: the production configuration.
    pub fn none() -> Self {
        Self::default()
    }
}

/// Reverse every LATTICE mutation installed after `target_commit_id`.
///
/// `project_root` is the Local Workspace the session's Turns mutated — the
/// same canonical root a live Turn runs against; it selects the private
/// history and the audit chain.
pub fn rewind_to_turn(
    session: &SessionApi,
    session_id: &str,
    target_commit_id: &str,
    project_root: &Path,
) -> std::result::Result<RewindOutcome, RewindError> {
    rewind_to_turn_with_barriers(
        session,
        session_id,
        target_commit_id,
        project_root,
        &RewindBarriers::none(),
    )
}

/// [`rewind_to_turn`] with deterministic phase barriers for tests.
#[allow(clippy::too_many_lines)]
pub fn rewind_to_turn_with_barriers(
    session: &SessionApi,
    session_id: &str,
    target_commit_id: &str,
    project_root: &Path,
    barriers: &RewindBarriers<'_>,
) -> std::result::Result<RewindOutcome, RewindError> {
    // Rewind is Session-bound for its entire read/restore/ref-publication
    // saga. Resolve the id before locking, then keep the explicit guard until
    // the final audit append completes.
    let (resolved_session_id, execution_lock) = session.lock_execution(session_id)?;

    // 1. Target resolution. `get_turn_commit` already refuses a commit that
    //    does not exist and one whose manifest names a different session, so
    //    a foreign or bogus target never reaches the workspace.
    let status = session.status_locked(&resolved_session_id, &execution_lock)?;
    let target = session
        .get_turn_commit(&resolved_session_id, target_commit_id)?
        .ok_or_else(|| {
            ApiError::NotFound(format!(
                "turn '{target_commit_id}' is not a Turn of session '{resolved_session_id}'"
            ))
        })?;

    // Reachability: the target must be an ancestor of the current session
    // HEAD. `log` walks exactly the commits reachable from HEAD, on a branch
    // or detached, so membership is the reachability test.
    let log = session.log(&resolved_session_id)?;
    let target_index = log
        .iter()
        .position(|entry| entry.commit_id == target.commit_id)
        .ok_or_else(|| {
            ApiError::BadRequest(format!(
                "turn '{target_commit_id}' is not reachable from the current HEAD of session \
                 '{resolved_session_id}'"
            ))
        })?;

    // 2. Which workspace-history identity the target Turn arrived at.
    let history_target =
        effective_history_target(session, &resolved_session_id, &target, &log, target_index)?;

    let workspace_root = crate::root::LocalWorkspaceRoot::from_path(project_root)?;
    let expected_key =
        lattice_session::ProjectStoreKey::parse(&status.project_store_key).map_err(|err| {
            ApiError::Internal(format!(
                "session '{resolved_session_id}' has an invalid ProjectStoreKey binding: {err}"
            ))
        })?;
    if expected_key != workspace_root.store_key() {
        return Err(ApiError::Conflict(format!(
            "session '{resolved_session_id}' is bound to project key {expected_key}, but the current Local Workspace resolves to {}",
            workspace_root.store_key()
        ))
        .into());
    }
    let backing = session.workspace_history_backing(expected_key)?;

    // An unwind-everything plan is only sound when nothing this session
    // installed at or before the target would be reversed by it. The ancestor
    // scan above proves the identity was never recorded on this branch; this
    // proves the same against the history chain itself, which is the record
    // that actually drives the reversal.
    if history_target.is_none() {
        guard_full_unwind(
            backing.clone(),
            &resolved_session_id,
            &target,
            &log,
            target_index,
        )?;
    }

    // 3. Acquire the exclusive project `WorkspaceOperationLock` exactly once
    //    (#139-B Commit 3) and hold it across the workspace half, the Session
    //    ref movement, and the audit append. Lower layers accept the guard
    //    and never reacquire it.
    let held = HeldWorkspaceOperation::acquire(&backing).map_err(|err| {
        ApiError::Internal(format!("acquire the workspace operation lock: {err}"))
    })?;

    // 4. Workspace half.
    let audit = RewindAuditContext {
        backing: &backing,
        session_id: &resolved_session_id,
        target_commit_id: &target.commit_id,
        barriers,
    };
    let barrier_facts = |outcome: &str| -> RewindBarrierFacts {
        RewindBarrierFacts {
            session_id: resolved_session_id.clone(),
            target_commit_id: target.commit_id.clone(),
            outcome: outcome.to_string(),
            audit_records_on_chain: rewind_audit_records(&backing, &resolved_session_id)
                .map(|records| records.len())
                .unwrap_or(0),
        }
    };
    let workspace = match rewind_workspace(
        project_root,
        backing.clone(),
        history_target.as_deref(),
        &held,
    ) {
        Ok(outcome) => outcome,
        Err(WorkspaceRewindError::RefMove { detail, report }) => {
            // The Local Workspace was restored but `current` did not move.
            // Record the partial state, then fail — never report success.
            let steps = steps_of(&report.steps);
            let preflight = preflight_of(&report.preflight);
            audit.write(
                "history_ref_failure",
                report.from_history_oid.as_deref(),
                report.to_history_oid.as_deref(),
                &steps,
                &preflight,
                Some(&restored_of(&report.restored)),
                None,
                false,
            )?;
            return Err(ApiError::Internal(format!(
                "rewind restored {} path(s) but the private history ref did not move: {detail}",
                report.restored.len()
            ))
            .into());
        }
        Err(WorkspaceRewindError::Restore {
            source,
            steps,
            restored,
        }) => {
            // Phase 2 writes one path at a time, so this may be a partially
            // restored workspace. Record exactly which paths were written.
            let steps = steps_of(&steps);
            let restored = restored_of(&restored);
            audit.write(
                "restore_failure",
                None,
                history_target.as_deref(),
                &steps,
                &[],
                Some(&restored),
                None,
                false,
            )?;
            return Err(ApiError::Internal(format!(
                "rewind restore failed after restoring {} path(s): {source}",
                restored.len()
            ))
            .into());
        }
        Err(WorkspaceRewindError::PlanIncomplete { paths }) => {
            // Nothing moved; the history could not be read as a complete
            // account of the range, so refuse rather than half-restore.
            return Err(ApiError::Conflict(format!(
                "the recorded history does not account for {} path(s) changed since turn '{}': \
                 {paths:?}",
                paths.len(),
                target_commit_id
            ))
            .into());
        }
        Err(WorkspaceRewindError::TargetUnreachable { target }) => {
            return Err(ApiError::BadRequest(format!(
                "the workspace history recorded for turn '{}' is no longer on the current chain \
                 ({target})",
                target_commit_id
            ))
            .into());
        }
        Err(err) => return Err(ApiError::Internal(err.to_string()).into()),
    };

    let report = match workspace {
        WorkspaceRewindOutcome::Conflict(conflict) => {
            let steps = steps_of(&conflict.steps);
            let preflight = preflight_of(&conflict.preflight);
            let conflicts = conflicts_of(&conflict.conflicts);
            if let Some(barrier) = barriers.after_workspace_half {
                barrier(&barrier_facts("conflict"));
            }
            audit.write(
                "conflict",
                conflict.from_history_oid.as_deref(),
                conflict.to_history_oid.as_deref(),
                &steps,
                &preflight,
                None,
                Some(&conflicts),
                false,
            )?;
            return Ok(RewindOutcome::Conflict(RewindConflict {
                session_id: resolved_session_id,
                target_commit_id: target.commit_id,
                from_history_oid: conflict.from_history_oid,
                to_history_oid: conflict.to_history_oid,
                steps,
                preflight,
                conflicts,
            }));
        }
        WorkspaceRewindOutcome::Applied(report) => report,
    };

    // 5. Session History ref, last. The abandoned head is pinned first so the
    //    Turns this Rewind leaves behind keep an internal ref of their own.
    let steps = steps_of(&report.steps);
    let preflight = preflight_of(&report.preflight);
    let restored = restored_of(&report.restored);
    let previous_commit_id = status.current_commit_id.clone();
    let pinned_ref = previous_commit_id
        .as_deref()
        .filter(|head| *head != target.commit_id)
        .and_then(|head| pin_abandoned_head(session, &resolved_session_id, head, &execution_lock));

    let session_head = match &status.head {
        GraphHead::Branch { name } => {
            match session.reset_locked(&resolved_session_id, &target.commit_id, &execution_lock) {
                Ok(result) => SessionHeadMove {
                    branch: Some(result.branch),
                    previous_commit_id: result.old_commit_id,
                    new_commit_id: result.new_commit_id,
                    pinned_ref,
                },
                Err(err) => {
                    audit.write(
                        "session_ref_failure",
                        report.from_history_oid.as_deref(),
                        report.to_history_oid.as_deref(),
                        &steps,
                        &preflight,
                        Some(&restored),
                        None,
                        false,
                    )?;
                    return Err(ApiError::Internal(format!(
                        "rewind restored the Local Workspace and moved the private history ref, \
                         but branch '{name}' did not move: {err}"
                    ))
                    .into());
                }
            }
        }
        GraphHead::Detached { .. } => {
            match session.checkout_locked(&resolved_session_id, &target.commit_id, &execution_lock)
            {
                Ok(_) => SessionHeadMove {
                    branch: None,
                    previous_commit_id,
                    new_commit_id: target.commit_id.clone(),
                    pinned_ref,
                },
                Err(err) => {
                    audit.write(
                        "session_ref_failure",
                        report.from_history_oid.as_deref(),
                        report.to_history_oid.as_deref(),
                        &steps,
                        &preflight,
                        Some(&restored),
                        None,
                        false,
                    )?;
                    return Err(ApiError::Internal(format!(
                        "rewind restored the Local Workspace and moved the private history ref, \
                         but the detached session HEAD did not move: {err}"
                    ))
                    .into());
                }
            }
        }
    };

    if let Some(barrier) = barriers.after_workspace_half {
        barrier(&barrier_facts("workspace_applied"));
    }
    if let Some(barrier) = barriers.after_session_ref_move {
        barrier(&barrier_facts("session_moved"));
    }

    // 6. Durable record of the completed Rewind.
    audit.write(
        "success",
        report.from_history_oid.as_deref(),
        report.to_history_oid.as_deref(),
        &steps,
        &preflight,
        Some(&restored),
        None,
        true,
    )?;

    Ok(RewindOutcome::Success(RewindSuccess {
        session_id: resolved_session_id,
        target_commit_id: target.commit_id,
        from_history_oid: report.from_history_oid,
        to_history_oid: report.to_history_oid,
        steps,
        preflight,
        restored,
        session_head,
    }))
}

/// The workspace-history identity to rewind back to.
///
/// A Turn records the identity it arrived at, so `Some` is authoritative.
/// `None` is ambiguous: it means "no mutation had ever been recorded" for a
/// Turn from before the first mutation, but the live turn path also stamps
/// `None` when reading the identity failed. Treating the ambiguous case as
/// "unwind everything" would reverse mutations installed *before* the target
/// Turn, so a `None` target is only honoured when no ancestor Turn carries an
/// identity either; otherwise the Rewind refuses.
fn effective_history_target(
    session: &SessionApi,
    session_id: &str,
    target: &lattice_session::TurnCommit,
    log: &[lattice_session::TurnLogEntry],
    target_index: usize,
) -> Result<Option<String>> {
    if let Some(oid) = target.workspace_history_oid.clone() {
        return Ok(Some(oid));
    }
    // `log` is the oldest-first walk of everything reachable from HEAD. For
    // the linear turn graphs the session store produces, the entries before
    // the target are its ancestors; a merge-shaped graph could interleave a
    // sibling, which only makes this scan stricter, never laxer.
    for entry in log.iter().take(target_index) {
        let ancestor = session.get_turn_commit(session_id, &entry.commit_id)?;
        if let Some(oid) = ancestor.and_then(|commit| commit.workspace_history_oid) {
            return Err(ApiError::Conflict(format!(
                "turn '{}' records no workspace-history identity while its ancestor '{}' records \
                 '{oid}'; the identity for this turn is unavailable, so rewinding to it would \
                 reverse mutations installed before it",
                target.commit_id, entry.commit_id
            )));
        }
    }
    Ok(None)
}

/// Refuse a full unwind that would reverse this session's own pre-target work.
///
/// A Turn stamped with no identity is honoured as "before any mutation", but
/// the private history is per project: another session's Turns share the same
/// chain, so a stale or failed identity read can leave a target whose
/// ancestors are all identity-less while the chain already holds mutations
/// this session installed at or before it. Every mutation commit carries the
/// `<session>:<parent-commit>` turn id that installed it, so the plan can be
/// checked directly against this session's log.
fn guard_full_unwind(
    backing: lancet_api::WorkspaceHistoryBacking,
    session_id: &str,
    target: &lattice_session::TurnCommit,
    log: &[lattice_session::TurnLogEntry],
    target_index: usize,
) -> Result<()> {
    let target_commit_id = target.commit_id.as_str();
    // A planning failure must not skip the guard: a transient repository
    // read error that succeeds on the rewind's own second plan would let an
    // unguarded full unwind through. Refuse instead — a persistent error
    // would have refused anyway, so failing closed costs nothing.
    let plan = plan_workspace_rewind(backing, None).map_err(|err| {
        ApiError::Conflict(format!(
            "turn '{}' records no workspace-history identity and the private history could not \
             be read to prove a full unwind is safe: {err}",
            target.commit_id
        ))
    })?;
    // A step's turn id names the Turn that installed it as
    // `<session>:<parent commit>`, so this session's own Turns can be placed
    // exactly against the log. Everything else — another session's Turns, or
    // a Turn on a branch this log does not contain — has no such ordering, so
    // it falls back to when the mutation was recorded against when the target
    // Turn was committed. Both timestamps are written by the same machine.
    let at_or_before: std::collections::BTreeSet<&str> = log
        .iter()
        .take(target_index + 1)
        .flat_map(|entry| entry.parent_ids.first().map(String::as_str))
        .chain(std::iter::once("root"))
        .collect();
    let after: std::collections::BTreeSet<&str> = log
        .iter()
        .skip(target_index + 1)
        .flat_map(|entry| entry.parent_ids.first().map(String::as_str))
        .collect();
    let target_committed_at = parse_rfc3339_seconds(&target.created_at);

    for step in &plan.steps {
        let placement = step
            .turn_id
            .split_once(':')
            .filter(|(step_session, _)| *step_session == session_id)
            .map(|(_, parent)| {
                if at_or_before.contains(parent) {
                    Placement::Before
                } else if after.contains(parent) {
                    Placement::After
                } else {
                    Placement::Unknown
                }
            })
            .unwrap_or(Placement::Unknown);

        let predates = match placement {
            Placement::Before => true,
            Placement::After => false,
            // No exact placement: fall back to recording time, and treat an
            // unreadable timestamp on either side as ambiguity — a full
            // unwind must not proceed on ambiguity.
            Placement::Unknown => match (step.recorded_at, target_committed_at) {
                (Some(recorded), Some(target_at)) => recorded < target_at,
                _ => true,
            },
        };
        if predates {
            return Err(ApiError::Conflict(format!(
                "turn '{target_commit_id}' records no workspace-history identity, but the private \
                 history still holds a mutation recorded at or before it ({} on '{}', turn \
                 '{}'); rewinding would reverse work that predates the target",
                step.method, step.path, step.turn_id
            )));
        }
    }
    Ok(())
}

/// Where a reversed step's Turn sits relative to the rewind target.
enum Placement {
    /// A Turn of this session at or before the target.
    Before,
    /// A Turn of this session after the target.
    After,
    /// Another session's Turn, or one this session's log does not place.
    Unknown,
}

/// Unix seconds from an RFC 3339 timestamp, or `None` if it does not parse.
fn parse_rfc3339_seconds(raw: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|value| value.timestamp())
}

/// Pin the head a Rewind abandons under an internal session ref.
///
/// Keeps the rewound-away Turns reachable instead of leaving them orphaned
/// for the next prune. The ref name is derived from the commit, so re-pinning
/// the same head is a no-op; a pin that cannot be created is not fatal to the
/// Rewind, which is why this returns an `Option` rather than an error.
fn pin_abandoned_head(
    session: &SessionApi,
    session_id: &str,
    head: &str,
    execution_lock: &lattice_session::FlockGuard,
) -> Option<String> {
    let name = format!("rewound/{head}");
    match session.branch_locked(session_id, &name, Some(head), execution_lock) {
        Ok(_) => Some(name),
        // Already pinned by an earlier Rewind to the same head.
        Err(ApiError::Conflict(_)) => Some(name),
        Err(_) => None,
    }
}

/// Writes one durable Rewind audit record per outcome, onto the independent
/// append-only chain in `workspace-history.git`.
///
/// Records paths, Local Revisions, and object ids only — never file content.
/// An append failure is returned as the typed partial-state
/// [`RewindError::AuditPublicationFailed`]: the actions already happened and
/// must not be reported as success.
struct RewindAuditContext<'a> {
    backing: &'a WorkspaceHistoryBacking,
    session_id: &'a str,
    target_commit_id: &'a str,
    barriers: &'a RewindBarriers<'a>,
}

impl RewindAuditContext<'_> {
    #[allow(clippy::too_many_arguments)]
    fn write(
        &self,
        outcome: &str,
        from_history_oid: Option<&str>,
        to_history_oid: Option<&str>,
        steps: &[RewindStep],
        preflight: &[RewindPathPreflight],
        restored: Option<&[RewindRestoredPath]>,
        conflicts: Option<&[RewindPathConflict]>,
        session_head_moved: bool,
    ) -> std::result::Result<(), RewindError> {
        let record = RewindAuditRecord {
            schema_version: REWIND_AUDIT_SCHEMA_VERSION,
            session_id: self.session_id.to_string(),
            target_turn_id: self.target_commit_id.to_string(),
            target_history_oid: to_history_oid.map(str::to_string),
            from_history_oid: from_history_oid.map(str::to_string),
            outcome: outcome.to_string(),
            steps_json: json!(steps).to_string(),
            preflight_json: json!(preflight).to_string(),
            restored_json: restored.map(|r| json!(r).to_string()),
            conflicts_json: conflicts.map(|c| json!(c).to_string()),
            recorded_at: chrono::Utc::now().to_rfc3339(),
        };
        let partial = || RewindPartialState {
            session_id: self.session_id.to_string(),
            target_commit_id: self.target_commit_id.to_string(),
            outcome: outcome.to_string(),
            from_history_oid: from_history_oid.map(str::to_string),
            to_history_oid: to_history_oid.map(str::to_string),
            steps: steps.to_vec(),
            preflight: preflight.to_vec(),
            restored: restored
                .map(<[RewindRestoredPath]>::to_vec)
                .unwrap_or_default(),
            conflicts: conflicts
                .map(<[RewindPathConflict]>::to_vec)
                .unwrap_or_default(),
            session_head_moved,
        };
        record_rewind_audit(self.backing, &record).map_err(|err| {
            let detail = format!("persist rewind audit record: {err}");
            RewindError::AuditPublicationFailed {
                detail,
                partial: Box::new(partial()),
            }
        })?;
        if let Some(barrier) = self.barriers.after_audit_append {
            barrier(&RewindBarrierFacts {
                session_id: self.session_id.to_string(),
                target_commit_id: self.target_commit_id.to_string(),
                outcome: outcome.to_string(),
                audit_records_on_chain: rewind_audit_records(self.backing, self.session_id)
                    .map(|records| records.len())
                    .unwrap_or(0),
            });
        }
        Ok(())
    }
}

fn steps_of(steps: &[lancet_api::RewindStep]) -> Vec<RewindStep> {
    steps
        .iter()
        .map(|step| RewindStep {
            commit_id: step.commit_id.clone(),
            method: step.method.clone(),
            turn_id: step.turn_id.clone(),
            call_id: step.call_id,
            path: step.path.clone(),
            recorded_content_hash: step.recorded_content_hash.clone(),
        })
        .collect()
}

fn preflight_of(preflight: &[lancet_api::RewindPathPreflight]) -> Vec<RewindPathPreflight> {
    preflight
        .iter()
        .map(|entry| RewindPathPreflight {
            path: entry.path.clone(),
            expected_revision: entry.expected_revision.clone(),
            observed_revision: entry.observed_revision.clone(),
            diverged: entry.diverged,
        })
        .collect()
}

fn restored_of(restored: &[lancet_api::RestoredPath]) -> Vec<RewindRestoredPath> {
    restored
        .iter()
        .map(|entry| RewindRestoredPath {
            path: entry.path.clone(),
            restored_revision: entry.restored_revision.clone(),
        })
        .collect()
}

fn conflicts_of(conflicts: &[lancet_api::RewindPathConflict]) -> Vec<RewindPathConflict> {
    conflicts
        .iter()
        .map(|entry| RewindPathConflict {
            path: entry.path.clone(),
            expected_revision: entry.expected_revision.clone(),
            observed_revision: entry.observed_revision.clone(),
            detail: entry.detail.clone(),
        })
        .collect()
}
