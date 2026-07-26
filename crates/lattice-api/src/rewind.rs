// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Explicit Rewind (GitHub #64).
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
//! 3. Hand the workspace half to `lancet_api::rewind_workspace`: plan the
//!    reversal, verify every affected path against its recorded post-state,
//!    restore parent-tree content, then move the private history ref — all
//!    under the history's recording lock.
//! 4. Only then move the Session History ref, after pinning the abandoned
//!    head so the rewound-away Turns keep an internal ref.
//! 5. Write the durable audit record — for success, conflict, and every
//!    failure mode that touched the Local Workspace.
//!
//! This is a free function rather than a [`SessionApi`] method: the session
//! facade stays pointer-movement only, and Rewind needs the Local Workspace
//! and the private history, neither of which a session store owns.

use std::path::Path;

use lancet_api::{
    RewindAuditRecord, WorkspaceRewindError, WorkspaceRewindOutcome, plan_workspace_rewind,
    record_rewind_audit, rewind_workspace, workspace_history_backing,
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

/// Reverse every LATTICE mutation installed after `target_commit_id`.
///
/// `project_root` is the Local Workspace the session's Turns mutated — the
/// same canonical root a live Turn runs against; it selects the private
/// history and the audit database.
pub fn rewind_to_turn(
    session: &SessionApi,
    session_id: &str,
    target_commit_id: &str,
    project_root: &Path,
) -> Result<RewindOutcome> {
    // 1. Target resolution. `get_turn_commit` already refuses a commit that
    //    does not exist and one whose manifest names a different session, so
    //    a foreign or bogus target never reaches the workspace.
    let status = session.status(session_id)?;
    let resolved_session_id = status.session_id.clone();
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

    // An unwind-everything plan is only sound when nothing this session
    // installed at or before the target would be reversed by it. The ancestor
    // scan above proves the identity was never recorded on this branch; this
    // proves the same against the history chain itself, which is the record
    // that actually drives the reversal.
    if history_target.is_none() {
        guard_full_unwind(
            project_root,
            &resolved_session_id,
            &target,
            &log,
            target_index,
        )?;
    }

    // 3. Workspace half.
    let backing = workspace_history_backing(project_root)
        .map_err(|err| ApiError::Internal(format!("resolve private workspace history: {err}")))?;
    let audit = RewindAuditContext {
        project_root,
        session_id: &resolved_session_id,
        target_commit_id: &target.commit_id,
    };
    let workspace = match rewind_workspace(project_root, backing, history_target.as_deref()) {
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
            )?;
            return Err(ApiError::Internal(format!(
                "rewind restored {} path(s) but the private history ref did not move: {detail}",
                report.restored.len()
            )));
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
            )?;
            return Err(ApiError::Internal(format!(
                "rewind restore failed after restoring {} path(s): {source}",
                restored.len()
            )));
        }
        Err(WorkspaceRewindError::PlanIncomplete { paths }) => {
            // Nothing moved; the history could not be read as a complete
            // account of the range, so refuse rather than half-restore.
            return Err(ApiError::Conflict(format!(
                "the recorded history does not account for {} path(s) changed since turn '{}': \
                 {paths:?}",
                paths.len(),
                target_commit_id
            )));
        }
        Err(WorkspaceRewindError::TargetUnreachable { target }) => {
            return Err(ApiError::BadRequest(format!(
                "the workspace history recorded for turn '{}' is no longer on the current chain \
                 ({target})",
                target_commit_id
            )));
        }
        Err(err) => return Err(ApiError::Internal(err.to_string())),
    };

    let report = match workspace {
        WorkspaceRewindOutcome::Conflict(conflict) => {
            let steps = steps_of(&conflict.steps);
            let preflight = preflight_of(&conflict.preflight);
            let conflicts = conflicts_of(&conflict.conflicts);
            audit.write(
                "conflict",
                conflict.from_history_oid.as_deref(),
                conflict.to_history_oid.as_deref(),
                &steps,
                &preflight,
                None,
                Some(&conflicts),
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

    // 4. Session History ref, last. The abandoned head is pinned first so the
    //    Turns this Rewind leaves behind keep an internal ref of their own.
    let steps = steps_of(&report.steps);
    let preflight = preflight_of(&report.preflight);
    let restored = restored_of(&report.restored);
    let previous_commit_id = status.current_commit_id.clone();
    let pinned_ref = previous_commit_id
        .as_deref()
        .filter(|head| *head != target.commit_id)
        .and_then(|head| pin_abandoned_head(session, &resolved_session_id, head));

    let session_head = match &status.head {
        GraphHead::Branch { name } => {
            match session.reset(&resolved_session_id, &target.commit_id) {
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
                    )?;
                    return Err(ApiError::Internal(format!(
                        "rewind restored the Local Workspace and moved the private history ref, \
                         but branch '{name}' did not move: {err}"
                    )));
                }
            }
        }
        GraphHead::Detached { .. } => {
            match session.checkout(&resolved_session_id, &target.commit_id) {
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
                    )?;
                    return Err(ApiError::Internal(format!(
                        "rewind restored the Local Workspace and moved the private history ref, \
                         but the detached session HEAD did not move: {err}"
                    )));
                }
            }
        }
    };

    // 5. Durable record of the completed Rewind.
    audit.write(
        "success",
        report.from_history_oid.as_deref(),
        report.to_history_oid.as_deref(),
        &steps,
        &preflight,
        Some(&restored),
        None,
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
    project_root: &Path,
    session_id: &str,
    target: &lattice_session::TurnCommit,
    log: &[lattice_session::TurnLogEntry],
    target_index: usize,
) -> Result<()> {
    let target_commit_id = target.commit_id.as_str();
    let backing = workspace_history_backing(project_root)
        .map_err(|err| ApiError::Internal(format!("resolve private workspace history: {err}")))?;
    let plan = match plan_workspace_rewind(backing, None) {
        Ok(plan) => plan,
        // A history that cannot be planned is caught by the rewind itself.
        Err(_) => return Ok(()),
    };
    // Turns at or before the target, by the parent commit their turn id names.
    let at_or_before: std::collections::BTreeSet<&str> = log
        .iter()
        .take(target_index + 1)
        .flat_map(|entry| entry.parent_ids.first().map(String::as_str))
        .chain(std::iter::once("root"))
        .collect();
    // The private history is per project, so a step may belong to another
    // session entirely. Two independent signals place a step before the
    // target: this session's own turn ids, and — for every session — the
    // moment the mutation was recorded against the moment the target Turn was
    // committed. Both are written by the same machine.
    let target_recorded_by = parse_rfc3339_seconds(&target.created_at);
    for step in &plan.steps {
        let same_session_before =
            step.turn_id
                .split_once(':')
                .is_some_and(|(step_session, parent)| {
                    step_session == session_id && at_or_before.contains(parent)
                });
        let recorded_before = match (step.recorded_at, target_recorded_by) {
            (Some(recorded), Some(target_at)) => recorded <= target_at,
            // An unreadable timestamp on either side is ambiguity, and a full
            // unwind must not proceed on ambiguity.
            _ => true,
        };
        if same_session_before || recorded_before {
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
fn pin_abandoned_head(session: &SessionApi, session_id: &str, head: &str) -> Option<String> {
    let name = format!("rewound/{head}");
    match session.branch(session_id, &name, Some(head)) {
        Ok(_) => Some(name),
        // Already pinned by an earlier Rewind to the same head.
        Err(ApiError::Conflict(_)) => Some(name),
        Err(_) => None,
    }
}

/// Writes one durable Rewind audit record per outcome.
///
/// Records paths, Local Revisions, and object ids only — never file content.
struct RewindAuditContext<'a> {
    project_root: &'a Path,
    session_id: &'a str,
    target_commit_id: &'a str,
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
    ) -> Result<()> {
        let record = RewindAuditRecord {
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
        record_rewind_audit(self.project_root, &record)
            .map_err(|err| ApiError::Internal(format!("persist rewind audit record: {err}")))
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
