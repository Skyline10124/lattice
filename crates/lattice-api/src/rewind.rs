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
//!    per-session `refs/lattice/rewind-audit/sessions/<canonical-id>` chain —
//!    for success, conflict, and every failure mode that touched the Local
//!    Workspace — while the operation lock is still held. A failed append returns
//!    [`RewindError::AuditPublicationFailed`] partial state, never success.
//!
//! This is a free function rather than a [`SessionApi`] method: the session
//! facade stays pointer-movement only, and Rewind needs the Local Workspace
//! and the private history, neither of which a session store owns.

use std::collections::BTreeSet;
use std::path::Path;

use lancet_api::{
    HeldWorkspaceOperation, REWIND_AUDIT_SCHEMA_VERSION, RewindAuditPage, RewindAuditRecord,
    WorkspaceHistory, WorkspaceHistoryBacking, WorkspaceRewindError, WorkspaceRewindOutcome,
    plan_workspace_rewind, record_rewind_audit, rewind_audit_records, rewind_workspace,
};
use lattice_session::GraphHead;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::session::{ApiError, Result, SessionApi};

/// One reversed mutation commit, as reported to the caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewindStep {
    /// The private-history commit being reversed.
    pub commit_id: String,
    /// CEP method that installed the mutation.
    pub method: String,
    /// Turn the mutation belonged to.
    pub turn_id: lancet_protocol::TurnId,
    /// Invocation that installed the mutation.
    pub invocation_id: lancet_protocol::InvocationId,
    /// Workspace-relative path the mutation touched.
    pub path: String,
    /// Local Revision recorded right after the mutation installed.
    pub recorded_content_hash: Option<String>,
}

/// What preflight observed for one affected path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

fn completed_rewind_corruption(detail: impl Into<String>) -> ApiError {
    ApiError::Internal(format!(
        "corrupt completed Rewind audit record: {}",
        detail.into()
    ))
}

fn is_canonical_object_id(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_canonical_content_hash(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_canonical_workspace_path(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && value
            .split('/')
            .all(|component| !matches!(component, "" | "." | ".."))
}

fn replay_completed_success(
    record: &RewindAuditRecord,
    session_id: lancet_protocol::SessionId,
    target_turn_id: lancet_protocol::TurnId,
    target_commit_id: &str,
    target_history_oid: Option<&str>,
    current_head: &GraphHead,
) -> Result<RewindSuccess> {
    if record.outcome != "success"
        || record.session_id != session_id
        || record.target_turn_id != target_turn_id
        || record.target_commit_id != target_commit_id
        || record.target_history_oid.as_deref() != target_history_oid
    {
        return Err(ApiError::Conflict(format!(
            "the newest Rewind audit record does not exactly match completed retry target '{target_commit_id}'"
        )));
    }
    if record.conflicts_json.is_some() {
        return Err(completed_rewind_corruption(
            "a success record carries conflicts",
        ));
    }
    if record
        .from_history_oid
        .as_deref()
        .is_some_and(|oid| !is_canonical_object_id(oid))
        || chrono::DateTime::parse_from_rfc3339(&record.recorded_at).is_err()
    {
        return Err(completed_rewind_corruption("record metadata is malformed"));
    }

    let steps: Vec<RewindStep> = serde_json::from_str(&record.steps_json)
        .map_err(|err| completed_rewind_corruption(format!("steps payload: {err}")))?;
    let preflight: Vec<RewindPathPreflight> = serde_json::from_str(&record.preflight_json)
        .map_err(|err| completed_rewind_corruption(format!("preflight payload: {err}")))?;
    let restored: Vec<RewindRestoredPath> = serde_json::from_str(
        record
            .restored_json
            .as_deref()
            .ok_or_else(|| completed_rewind_corruption("success has no restored payload"))?,
    )
    .map_err(|err| completed_rewind_corruption(format!("restored payload: {err}")))?;
    let session_head: SessionHeadMove =
        serde_json::from_str(record.session_head_json.as_deref().ok_or_else(|| {
            completed_rewind_corruption("success has no SessionHeadMove payload")
        })?)
        .map_err(|err| completed_rewind_corruption(format!("SessionHeadMove payload: {err}")))?;

    let step_paths: BTreeSet<&str> = steps.iter().map(|entry| entry.path.as_str()).collect();
    let preflight_paths: BTreeSet<&str> =
        preflight.iter().map(|entry| entry.path.as_str()).collect();
    let restored_paths: BTreeSet<&str> = restored.iter().map(|entry| entry.path.as_str()).collect();
    if preflight_paths.len() != preflight.len()
        || restored_paths.len() != restored.len()
        || step_paths != preflight_paths
        || preflight_paths != restored_paths
        || preflight.iter().any(|entry| {
            entry.diverged
                || entry.expected_revision != entry.observed_revision
                || entry
                    .expected_revision
                    .as_deref()
                    .is_some_and(|hash| !is_canonical_content_hash(hash))
                || entry
                    .observed_revision
                    .as_deref()
                    .is_some_and(|hash| !is_canonical_content_hash(hash))
                || !is_canonical_workspace_path(&entry.path)
        })
        || restored.iter().any(|entry| {
            !is_canonical_workspace_path(&entry.path)
                || entry
                    .restored_revision
                    .as_deref()
                    .is_some_and(|hash| !is_canonical_content_hash(hash))
        })
        || steps.iter().any(|step| {
            !is_canonical_object_id(&step.commit_id)
                || step.method.is_empty()
                || !is_canonical_workspace_path(&step.path)
                || step
                    .recorded_content_hash
                    .as_deref()
                    .is_none_or(|hash| !is_canonical_content_hash(hash))
        })
    {
        return Err(completed_rewind_corruption(
            "step, preflight, and restored payloads are inconsistent",
        ));
    }

    let expected_branch = match current_head {
        GraphHead::Branch { name } => Some(name.as_str()),
        GraphHead::Detached { .. } => None,
    };
    if session_head.new_commit_id != target_commit_id
        || !is_canonical_object_id(&session_head.new_commit_id)
        || session_head.branch.as_deref() != expected_branch
        || session_head
            .previous_commit_id
            .as_deref()
            .is_some_and(|commit| !is_canonical_object_id(commit))
        || session_head.pinned_ref.as_deref().is_some_and(|pin| {
            session_head
                .previous_commit_id
                .as_deref()
                .is_none_or(|previous| {
                    previous == session_head.new_commit_id || pin != format!("rewound/{previous}")
                })
        })
    {
        return Err(completed_rewind_corruption(
            "SessionHeadMove does not match the current Session target",
        ));
    }

    Ok(RewindSuccess {
        session_id: session_id.as_str(),
        target_commit_id: target_commit_id.to_string(),
        from_history_oid: record.from_history_oid.clone(),
        to_history_oid: record.target_history_oid.clone(),
        steps,
        preflight,
        restored,
        session_head,
    })
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

/// A fresh read of an authoritative ref after an audit append error.
///
/// `Empty` is a successful observation that the ref does not exist. It is
/// deliberately distinct from `Unavailable`, where the workflow could not read
/// the ref at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RewindRefObservation {
    /// The ref currently names this object.
    Present {
        /// Current object id.
        value: String,
    },
    /// The ref is authoritatively absent.
    Empty,
    /// The ref could not be observed.
    Unavailable,
}

impl RewindRefObservation {
    fn observed(value: Option<String>) -> Self {
        value.map_or(Self::Empty, |value| Self::Present { value })
    }
}

/// Visibility of the exact attempted audit record after its append returned an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RewindAuditRecordObservation {
    /// The exact record is reachable, but the failed append did not confirm fsync durability.
    VisibleDurabilityUnconfirmed,
    /// A complete audit read proved that the exact record is not reachable.
    NotVisible,
    /// The bounded page did not contain the record and older records exist.
    Uncertain,
    /// The audit chain could not be read.
    Unavailable,
}

/// The authoritative state observed after a Rewind audit record could not be
/// published. The caller must not retry the Rewind blindly because the actions
/// are not transactional across stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RewindPartialState {
    /// Resolved typed Session identity.
    pub session_id: lancet_protocol::SessionId,
    /// Selected typed Turn identity.
    pub target_turn_id: lancet_protocol::TurnId,
    /// Selected Turn's immutable Session commit id.
    pub target_commit_id: String,
    /// Outcome label whose record failed to publish.
    pub outcome: String,
    /// Workspace-history identity observed when the action started (`None` = empty).
    pub started_from_history_oid: Option<String>,
    /// Workspace-history identity the action attempted to reach (`None` = empty).
    pub attempted_target_history_oid: Option<String>,
    /// Fresh post-error observation of private workspace-history `current`.
    pub actual_workspace_history_oid: RewindRefObservation,
    /// Fresh post-error observation of the Session head commit.
    pub actual_session_head_commit_id: RewindRefObservation,
    /// Post-error visibility of the exact audit record whose append failed.
    pub audit_record_observation: RewindAuditRecordObservation,
    /// Planned mutation commits, newest first.
    pub steps: Vec<RewindStep>,
    /// Preflight observations, one per affected path.
    pub preflight: Vec<RewindPathPreflight>,
    /// Paths actually restored before the failure.
    pub restored: Vec<RewindRestoredPath>,
    /// Divergent paths, when the outcome is a conflict.
    pub conflicts: Vec<RewindPathConflict>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RewindBarrierFacts {
    /// Resolved typed Session identity.
    pub session_id: lancet_protocol::SessionId,
    /// The selected target Turn identity.
    pub target_turn_id: lancet_protocol::TurnId,
    /// The workflow outcome label reached so far.
    pub outcome: String,
    /// Bounded newest-first view of this Session's private audit chain.
    pub audit_page: RewindAuditPage,
    /// Fresh private workspace-history `current` OID at the barrier.
    pub workspace_history_oid: Option<String>,
    /// Fresh Session History commit at the barrier.
    pub session_head_commit_id: Option<String>,
}

#[cfg(feature = "test-crash-barriers")]
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let from = std::ffi::CString::new(from.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rewind barrier temp path contains NUL",
        )
    })?;
    let to = std::ffi::CString::new(to.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rewind barrier ack path contains NUL",
        )
    })?;
    // SAFETY: both C strings outlive the call; absolute paths make AT_FDCWD
    // irrelevant. Linux renameat2 is required and failure is never downgraded.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(feature = "test-crash-barriers")]
fn publish_rewind_barrier_ack(path: &Path, value: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write;

    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rewind barrier ack path must be absolute",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "rewind barrier ack path has no parent",
        )
    })?;
    let bytes = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    let temp = parent.join(format!(".lattice-rewind-ack-{}.tmp", uuid::Uuid::new_v4()));
    let mut temp_created = false;
    let publication = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        temp_created = true;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        rename_noreplace(&temp, path)?;
        std::fs::File::open(parent)?.sync_all()
    })();

    if let Err(error) = publication {
        if temp_created {
            match std::fs::remove_file(&temp) {
                Ok(()) => {}
                Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => {}
                Err(cleanup) => {
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!(
                            "{error}; remove rewind barrier temp '{}': {cleanup}",
                            temp.display()
                        ),
                    ));
                }
            }
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(feature = "test-crash-barriers")]
fn hit_rewind_barrier(label: &str, facts: &RewindBarrierFacts) -> Result<()> {
    if std::env::var("LATTICE_TEST_REWIND_BARRIER").ok().as_deref() != Some(label) {
        return Ok(());
    }
    let path = std::path::PathBuf::from(
        std::env::var_os("LATTICE_TEST_REWIND_BARRIER_ACK")
            .ok_or_else(|| ApiError::Internal("rewind barrier ack path is required".to_string()))?,
    );
    let value = json!({ "barrier": label, "facts": facts });
    publish_rewind_barrier_ack(&path, &value)
        .map_err(|err| ApiError::Internal(format!("publish rewind barrier ack: {err}")))?;
    // SAFETY: test-only Linux feature; the harness SIGKILLs the process group
    // after validating this fsynced durable-state acknowledgement.
    let result = unsafe { libc::raise(libc::SIGSTOP) };
    if result != 0 {
        return Err(ApiError::Internal(format!(
            "raise SIGSTOP for Rewind crash barrier: {}",
            std::io::Error::last_os_error()
        )));
    }
    panic!("rewind crash barrier resumed unexpectedly");
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

fn rewind_barrier_facts(
    backing: &WorkspaceHistoryBacking,
    session: &SessionApi,
    execution_lock: &lattice_session::FlockGuard,
    session_id: lancet_protocol::SessionId,
    turn_id: lancet_protocol::TurnId,
    outcome: &str,
) -> Result<RewindBarrierFacts> {
    let audit_page = rewind_audit_records(backing, &session_id)
        .map_err(|err| ApiError::Internal(format!("read Rewind audit barrier facts: {err}")))?;
    let workspace_history_oid = WorkspaceHistory::open(backing.clone())
        .and_then(|history| history.current())
        .map_err(|err| {
            ApiError::Internal(format!(
                "read workspace-history current for Rewind barrier facts: {err}"
            ))
        })?;
    let session_head_commit_id = session
        .status_locked(&session_id.as_str(), execution_lock)
        .map_err(|err| {
            ApiError::Internal(format!(
                "read locked Session head for Rewind barrier facts: {err}"
            ))
        })?
        .current_commit_id;
    Ok(RewindBarrierFacts {
        session_id,
        target_turn_id: turn_id,
        outcome: outcome.to_string(),
        audit_page,
        workspace_history_oid,
        session_head_commit_id,
    })
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
    // Rewind 全程持有会话执行权；忙时拒绝而非让后台排队等待另一个所有者。
    let (resolved_session_id, execution_lock) = session.try_lock_execution(session_id)?;
    let audit_session_id =
        lancet_protocol::SessionId::parse(&resolved_session_id).map_err(|err| {
            ApiError::Internal(format!(
                "session store returned non-canonical session id '{resolved_session_id}': {err}"
            ))
        })?;

    // 1. Target resolution. `get_turn_commit` already refuses a commit that
    //    does not exist and one whose manifest names a different session, so
    //    a foreign or bogus target never reaches the workspace.
    let initial_status = session.status_locked(&resolved_session_id, &execution_lock)?;
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
    let expected_key = lattice_session::ProjectStoreKey::parse(&initial_status.project_store_key)
        .map_err(|err| {
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

    // 3. Acquire the exclusive project `WorkspaceOperationLock` exactly once
    //    before any workspace-history plan, then retain it through the workspace
    //    half, Session ref movement, audit append, and every partial return.
    let held = HeldWorkspaceOperation::acquire(&backing).map_err(|err| {
        ApiError::Internal(format!("acquire the workspace operation lock: {err}"))
    })?;
    let started_from_history_oid = WorkspaceHistory::open(backing.clone())
        .and_then(|history| history.current())
        .map_err(|err| ApiError::Internal(format!("read workspace-history current: {err}")))?;
    let status = session.status_locked(&resolved_session_id, &execution_lock)?;

    let audit = RewindAuditContext {
        backing: &backing,
        session,
        execution_lock: &execution_lock,
        session_id: audit_session_id,
        target_turn_id: target.turn_id,
        target_commit_id: &target.commit_id,
        started_from_history_oid: started_from_history_oid.as_deref(),
        attempted_target_history_oid: history_target.as_deref(),
        barriers,
    };

    // Once both authoritative refs already name the requested target, no action
    // may run again. An exact newest success for this target strictly replays its
    // original public result. A newest record for another target, or a complete
    // empty page, means this completion still lacks an audit record; close that
    // crash window as one no-op success. A same-target mismatch fails closed.
    if started_from_history_oid.as_deref() == history_target.as_deref()
        && status.current_commit_id.as_deref() == Some(target.commit_id.as_str())
    {
        let page = rewind_audit_records(&backing, &audit_session_id).map_err(|err| {
            ApiError::Internal(format!(
                "read completed Rewind audit record for session '{resolved_session_id}': {err}"
            ))
        })?;
        let replay_record = match page.records.first() {
            Some(record)
                if record.session_id == audit_session_id
                    && record.target_turn_id == target.turn_id
                    && record.target_commit_id == target.commit_id =>
            {
                Some(record)
            }
            Some(_) => None,
            None if !page.has_older => None,
            None => {
                return Err(
                    completed_rewind_corruption("audit page omitted its newest record").into(),
                );
            }
        };
        let success = if let Some(record) = replay_record {
            replay_completed_success(
                record,
                audit_session_id,
                target.turn_id,
                &target.commit_id,
                history_target.as_deref(),
                &status.head,
            )?
        } else {
            let session_head = SessionHeadMove {
                branch: match &status.head {
                    GraphHead::Branch { name } => Some(name.clone()),
                    GraphHead::Detached { .. } => None,
                },
                previous_commit_id: status.current_commit_id.clone(),
                new_commit_id: target.commit_id.clone(),
                pinned_ref: None,
            };
            audit.write("success", &[], &[], Some(&[]), None, Some(&session_head))?;
            RewindSuccess {
                session_id: resolved_session_id.clone(),
                target_commit_id: target.commit_id.clone(),
                from_history_oid: started_from_history_oid.clone(),
                to_history_oid: history_target.clone(),
                steps: Vec::new(),
                preflight: Vec::new(),
                restored: Vec::new(),
                session_head,
            }
        };
        drop(held);
        return Ok(RewindOutcome::Success(success));
    }

    // An unwind-everything plan is only sound when nothing this session
    // installed at or before the target would be reversed by it. The ancestor
    // scan above proves the identity was never recorded on this branch; this
    // proves the same against the history chain itself, which is the record
    // that actually drives the reversal.
    if history_target.is_none() {
        guard_full_unwind(backing.clone(), &held, &target, &log, target_index)?;
    }

    // 4. Workspace half.
    let barrier_facts = |outcome: &str| {
        rewind_barrier_facts(
            &backing,
            session,
            &execution_lock,
            audit_session_id,
            target.turn_id,
            outcome,
        )
    };
    let workspace = match rewind_workspace(
        project_root,
        backing.clone(),
        history_target.as_deref(),
        &held,
    ) {
        Ok(outcome) => outcome,
        Err(WorkspaceRewindError::RefMove { detail, report }) => {
            // The Local Workspace was restored but the `current` publication
            // did not complete durably. Record the failure, then fail.
            let steps = steps_of(&report.steps);
            let preflight = preflight_of(&report.preflight);
            if let Some(barrier) = barriers.after_workspace_half {
                barrier(&barrier_facts("history_ref_failure")?);
            }
            audit.write(
                "history_ref_failure",
                &steps,
                &preflight,
                Some(&restored_of(&report.restored)),
                None,
                None,
            )?;
            return Err(ApiError::Internal(format!(
                "rewind restored {} path(s) but the private history ref publication did not \
                 complete durably: {detail}",
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
            audit.write("restore_failure", &steps, &[], Some(&restored), None, None)?;
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
                barrier(&barrier_facts("conflict")?);
            }
            audit.write("conflict", &steps, &preflight, None, Some(&conflicts), None)?;
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

    let workspace_facts = barrier_facts("workspace_applied")?;
    if let Some(barrier) = barriers.after_workspace_half {
        barrier(&workspace_facts);
    }
    #[cfg(feature = "test-crash-barriers")]
    hit_rewind_barrier("rewind_workspace_applied", &workspace_facts)?;

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
                        &steps,
                        &preflight,
                        Some(&restored),
                        None,
                        None,
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
                        &steps,
                        &preflight,
                        Some(&restored),
                        None,
                        None,
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

    let session_facts = barrier_facts("session_moved")?;
    if let Some(barrier) = barriers.after_session_ref_move {
        barrier(&session_facts);
    }
    #[cfg(feature = "test-crash-barriers")]
    hit_rewind_barrier("rewind_session_moved", &session_facts)?;

    // 6. Durable record of the completed Rewind.
    audit.write(
        "success",
        &steps,
        &preflight,
        Some(&restored),
        None,
        Some(&session_head),
    )?;
    drop(held);

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

/// The project-global workspace-history checkpoint to rewind back to.
fn effective_history_target(
    _session: &SessionApi,
    _session_id: &str,
    target: &lattice_session::TurnCommit,
    log: &[lattice_session::TurnLogEntry],
    target_index: usize,
) -> Result<Option<String>> {
    match &target.workspace_history_checkpoint {
        lattice_session::WorkspaceHistoryCheckpoint::Oid(oid) => Ok(Some(oid.clone())),
        lattice_session::WorkspaceHistoryCheckpoint::Empty => {
            if let Some(ancestor) = log[..target_index].iter().rev().find(|entry| {
                matches!(
                    entry.workspace_history_checkpoint,
                    lattice_session::WorkspaceHistoryCheckpoint::Oid(_)
                )
            }) {
                return Err(ApiError::Conflict(format!(
                    "turn '{}' records an empty workspace-history checkpoint after ancestor '{}' recorded one",
                    target.commit_id, ancestor.commit_id
                )));
            }
            Ok(None)
        }
    }
}

/// Refuse a full unwind unless every mutation belongs to a uniquely placed
/// post-target Turn in the selected Session log.
fn guard_full_unwind(
    backing: lancet_api::WorkspaceHistoryBacking,
    held: &HeldWorkspaceOperation,
    target: &lattice_session::TurnCommit,
    log: &[lattice_session::TurnLogEntry],
    target_index: usize,
) -> Result<()> {
    let target_commit_id = target.commit_id.as_str();
    // A planning failure must not skip the guard: a transient repository
    // read error that succeeds on the rewind's own second plan would let an
    // unguarded full unwind through. Refuse instead — a persistent error
    // would have refused anyway, so failing closed costs nothing.
    let plan = plan_workspace_rewind(backing, None, held).map_err(|err| {
        ApiError::Conflict(format!(
            "turn '{}' records no workspace-history identity and the private history could not \
             be read to prove a full unwind is safe: {err}",
            target.commit_id
        ))
    })?;

    let mut placement = std::collections::BTreeMap::new();
    for (index, entry) in log.iter().enumerate() {
        if placement
            .insert(entry.turn_id, index > target_index)
            .is_some()
        {
            return Err(ApiError::Conflict(format!(
                "turn '{target_commit_id}' records no workspace-history identity, but the \
                 selected Session log repeats TurnId '{}'; full-unwind placement is ambiguous",
                entry.turn_id
            )));
        }
    }

    for step in &plan.steps {
        match placement.get(&step.turn_id) {
            Some(true) => {}
            Some(false) => {
                return Err(ApiError::Conflict(format!(
                    "turn '{target_commit_id}' records no workspace-history identity, but the \
                     private history still holds a mutation recorded at or before it ({} on '{}', \
                     turn '{}'); rewinding would reverse work that predates the target",
                    step.method, step.path, step.turn_id
                )));
            }
            None => {
                return Err(ApiError::Conflict(format!(
                    "turn '{target_commit_id}' records no workspace-history identity, but a \
                     private-history mutation ({} on '{}') carries TurnId '{}' that is not \
                     uniquely reachable in the selected Session log; full-unwind placement is \
                     ambiguous",
                    step.method, step.path, step.turn_id
                )));
            }
        }
    }
    Ok(())
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
    session: &'a SessionApi,
    execution_lock: &'a lattice_session::FlockGuard,
    session_id: lancet_protocol::SessionId,
    target_turn_id: lancet_protocol::TurnId,
    target_commit_id: &'a str,
    started_from_history_oid: Option<&'a str>,
    attempted_target_history_oid: Option<&'a str>,
    barriers: &'a RewindBarriers<'a>,
}

impl RewindAuditContext<'_> {
    fn barrier_facts(&self, outcome: &str) -> Result<RewindBarrierFacts> {
        rewind_barrier_facts(
            self.backing,
            self.session,
            self.execution_lock,
            self.session_id,
            self.target_turn_id,
            outcome,
        )
    }
    fn write(
        &self,
        outcome: &str,
        steps: &[RewindStep],
        preflight: &[RewindPathPreflight],
        restored: Option<&[RewindRestoredPath]>,
        conflicts: Option<&[RewindPathConflict]>,
        session_head: Option<&SessionHeadMove>,
    ) -> std::result::Result<(), RewindError> {
        let record = RewindAuditRecord {
            schema_version: REWIND_AUDIT_SCHEMA_VERSION,
            session_id: self.session_id,
            target_turn_id: self.target_turn_id,
            target_commit_id: self.target_commit_id.to_string(),
            target_history_oid: self.attempted_target_history_oid.map(str::to_string),
            from_history_oid: self.started_from_history_oid.map(str::to_string),
            outcome: outcome.to_string(),
            steps_json: json!(steps).to_string(),
            preflight_json: json!(preflight).to_string(),
            restored_json: restored.map(|r| json!(r).to_string()),
            conflicts_json: conflicts.map(|c| json!(c).to_string()),
            session_head_json: session_head.map(|head| json!(head).to_string()),
            recorded_at: chrono::Utc::now().to_rfc3339(),
        };
        if record_rewind_audit(self.backing, &record).is_err() {
            // The append error is not an observation. Re-read each authoritative
            // store under the still-held execution/operation locks and expose no
            // host paths or guessed movement in the public partial state.
            let actual_workspace_history_oid = match WorkspaceHistory::open(self.backing.clone())
                .and_then(|history| history.current())
            {
                Ok(value) => RewindRefObservation::observed(value),
                Err(_) => RewindRefObservation::Unavailable,
            };
            let session_id = self.session_id.as_str();
            let actual_session_head_commit_id =
                match self.session.status_locked(&session_id, self.execution_lock) {
                    Ok(status) => RewindRefObservation::observed(status.current_commit_id),
                    Err(_) => RewindRefObservation::Unavailable,
                };
            let audit_record_observation =
                match rewind_audit_records(self.backing, &self.session_id) {
                    Ok(page) if page.records.iter().any(|candidate| candidate == &record) => {
                        RewindAuditRecordObservation::VisibleDurabilityUnconfirmed
                    }
                    Ok(page) if page.has_older => RewindAuditRecordObservation::Uncertain,
                    Ok(_) => RewindAuditRecordObservation::NotVisible,
                    Err(_) => RewindAuditRecordObservation::Unavailable,
                };
            return Err(RewindError::AuditPublicationFailed {
                detail: "audit append returned an error; see typed partial state".to_string(),
                partial: Box::new(RewindPartialState {
                    session_id: self.session_id,
                    target_turn_id: self.target_turn_id,
                    target_commit_id: self.target_commit_id.to_string(),
                    outcome: outcome.to_string(),
                    started_from_history_oid: self.started_from_history_oid.map(str::to_string),
                    attempted_target_history_oid: self
                        .attempted_target_history_oid
                        .map(str::to_string),
                    actual_workspace_history_oid,
                    actual_session_head_commit_id,
                    audit_record_observation,
                    steps: steps.to_vec(),
                    preflight: preflight.to_vec(),
                    restored: restored
                        .map(<[RewindRestoredPath]>::to_vec)
                        .unwrap_or_default(),
                    conflicts: conflicts
                        .map(<[RewindPathConflict]>::to_vec)
                        .unwrap_or_default(),
                }),
            });
        }
        let facts = self.barrier_facts(outcome)?;
        if let Some(barrier) = self.barriers.after_audit_append {
            barrier(&facts);
        }
        #[cfg(feature = "test-crash-barriers")]
        hit_rewind_barrier("rewind_audit_appended", &facts)?;
        Ok(())
    }
}

fn steps_of(steps: &[lancet_api::RewindStep]) -> Vec<RewindStep> {
    steps
        .iter()
        .map(|step| RewindStep {
            commit_id: step.commit_id.clone(),
            method: step.method.clone(),
            turn_id: step.turn_id,
            invocation_id: step.invocation_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn partial_with(observation: RewindRefObservation) -> RewindPartialState {
        RewindPartialState {
            session_id: lancet_protocol::SessionId::parse("0123456789abcdef0123456789abcdef")
                .expect("SessionId"),
            target_turn_id: lancet_protocol::TurnId::parse("10000000-0000-4000-8000-000000000001")
                .expect("TurnId"),
            target_commit_id: "a".repeat(40),
            outcome: "success".to_string(),
            started_from_history_oid: Some("b".repeat(40)),
            attempted_target_history_oid: None,
            actual_workspace_history_oid: observation,
            actual_session_head_commit_id: RewindRefObservation::Present {
                value: "c".repeat(40),
            },
            audit_record_observation: RewindAuditRecordObservation::VisibleDurabilityUnconfirmed,
            steps: Vec::new(),
            preflight: Vec::new(),
            restored: Vec::new(),
            conflicts: Vec::new(),
        }
    }

    #[test]
    fn partial_state_serde_round_trip_preserves_typed_identity_and_observations() {
        let partial = partial_with(RewindRefObservation::Empty);
        let json = serde_json::to_string(&partial).expect("serialize partial state");
        let decoded: RewindPartialState =
            serde_json::from_str(&json).expect("deserialize partial state");
        assert_eq!(decoded, partial);
        assert!(json.contains("0123456789abcdef0123456789abcdef"));
        assert!(json.contains("10000000-0000-4000-8000-000000000001"));
    }

    #[test]
    fn known_empty_and_unavailable_ref_observations_are_distinct() {
        let empty = serde_json::to_value(partial_with(RewindRefObservation::Empty))
            .expect("serialize empty observation");
        let unavailable = serde_json::to_value(partial_with(RewindRefObservation::Unavailable))
            .expect("serialize unavailable observation");
        assert_eq!(empty["actual_workspace_history_oid"]["state"], "empty");
        assert_eq!(
            unavailable["actual_workspace_history_oid"]["state"],
            "unavailable"
        );
        assert_ne!(empty, unavailable);
    }

    fn replay_record() -> RewindAuditRecord {
        let session_id = lancet_protocol::SessionId::parse("0123456789abcdef0123456789abcdef")
            .expect("SessionId");
        let target_turn_id =
            lancet_protocol::TurnId::parse("10000000-0000-4000-8000-000000000001").expect("TurnId");
        let hash = format!("blake3:{}", "d".repeat(64));
        RewindAuditRecord {
            schema_version: REWIND_AUDIT_SCHEMA_VERSION,
            session_id,
            target_turn_id,
            target_commit_id: "a".repeat(40),
            target_history_oid: Some("b".repeat(40)),
            from_history_oid: Some("c".repeat(40)),
            outcome: "success".to_string(),
            steps_json: serde_json::to_string(&vec![RewindStep {
                commit_id: "d".repeat(40),
                method: "mutate.create".to_string(),
                turn_id: target_turn_id,
                invocation_id: lancet_protocol::InvocationId::parse(
                    "20000000-0000-4000-8000-000000000001",
                )
                .expect("InvocationId"),
                path: "notes.md".to_string(),
                recorded_content_hash: Some(hash.clone()),
            }])
            .expect("steps JSON"),
            preflight_json: serde_json::to_string(&vec![RewindPathPreflight {
                path: "notes.md".to_string(),
                expected_revision: Some(hash.clone()),
                observed_revision: Some(hash),
                diverged: false,
            }])
            .expect("preflight JSON"),
            restored_json: Some(
                serde_json::to_string(&vec![RewindRestoredPath {
                    path: "notes.md".to_string(),
                    restored_revision: None,
                }])
                .expect("restored JSON"),
            ),
            conflicts_json: None,
            session_head_json: Some(
                serde_json::to_string(&SessionHeadMove {
                    branch: Some("main".to_string()),
                    previous_commit_id: Some("e".repeat(40)),
                    new_commit_id: "a".repeat(40),
                    pinned_ref: Some(format!("rewound/{}", "e".repeat(40))),
                })
                .expect("SessionHeadMove JSON"),
            ),
            recorded_at: "2026-08-26T12:00:00Z".to_string(),
        }
    }

    fn replay(record: &RewindAuditRecord) -> Result<RewindSuccess> {
        replay_completed_success(
            record,
            record.session_id,
            record.target_turn_id,
            &record.target_commit_id,
            record.target_history_oid.as_deref(),
            &GraphHead::Branch {
                name: "main".to_string(),
            },
        )
    }

    #[test]
    fn completed_rewind_replay_requires_the_exact_target_and_payload() {
        let record = replay_record();
        let success = replay(&record).expect("strict replay succeeds");
        assert_eq!(success.session_id, record.session_id.to_string());
        assert_eq!(success.target_commit_id, record.target_commit_id);
        assert_eq!(success.steps.len(), 1);
        assert_eq!(success.preflight.len(), 1);
        assert_eq!(success.restored.len(), 1);

        let error = replay_completed_success(
            &record,
            record.session_id,
            record.target_turn_id,
            &record.target_commit_id,
            Some(&"f".repeat(40)),
            &GraphHead::Branch {
                name: "main".to_string(),
            },
        )
        .expect_err("same target with conflicting history must fail closed");
        assert!(
            matches!(error, ApiError::Conflict(_))
                && error.to_string().contains("does not exactly match"),
            "got {error:?}"
        );
    }

    #[test]
    fn completed_rewind_replay_rejects_corrupt_strict_payloads() {
        let record = replay_record();

        let mut unknown_field = record.clone();
        let mut steps: serde_json::Value =
            serde_json::from_str(&unknown_field.steps_json).expect("steps JSON");
        steps[0]["unexpected"] = serde_json::Value::Bool(true);
        unknown_field.steps_json = steps.to_string();
        let error = replay(&unknown_field).expect_err("unknown fields must fail closed");
        assert!(
            matches!(error, ApiError::Internal(_))
                && error.to_string().contains("steps payload")
                && error.to_string().contains("unknown field"),
            "got {error:?}"
        );

        let mut extra_path = record.clone();
        let mut preflight: Vec<RewindPathPreflight> =
            serde_json::from_str(&extra_path.preflight_json).expect("preflight JSON");
        preflight.push(RewindPathPreflight {
            path: "extra.md".to_string(),
            expected_revision: None,
            observed_revision: None,
            diverged: false,
        });
        extra_path.preflight_json = serde_json::to_string(&preflight).expect("preflight JSON");
        let mut restored: Vec<RewindRestoredPath> = serde_json::from_str(
            extra_path
                .restored_json
                .as_deref()
                .expect("restored payload"),
        )
        .expect("restored JSON");
        restored.push(RewindRestoredPath {
            path: "extra.md".to_string(),
            restored_revision: None,
        });
        extra_path.restored_json = Some(serde_json::to_string(&restored).expect("restored JSON"));
        let error = replay(&extra_path).expect_err("unmatched paths must fail closed");
        assert!(
            matches!(error, ApiError::Internal(_))
                && error.to_string().contains("payloads are inconsistent"),
            "got {error:?}"
        );

        let mut invalid_hash = record;
        let mut steps: Vec<RewindStep> =
            serde_json::from_str(&invalid_hash.steps_json).expect("steps JSON");
        steps[0].recorded_content_hash = Some("blake3:not-canonical".to_string());
        invalid_hash.steps_json = serde_json::to_string(&steps).expect("steps JSON");
        let error = replay(&invalid_hash).expect_err("non-canonical hashes must fail closed");
        assert!(
            matches!(error, ApiError::Internal(_))
                && error.to_string().contains("payloads are inconsistent"),
            "got {error:?}"
        );
    }

    fn placement_turn(
        turn_id: lancet_protocol::TurnId,
        commit_id: char,
    ) -> lattice_session::TurnCommit {
        lattice_session::TurnCommit {
            commit_id: commit_id.to_string().repeat(40),
            session_id: "0123456789abcdef0123456789abcdef".to_string(),
            turn_id,
            parent_ids: Vec::new(),
            message_ids: Vec::new(),
            agent_name: "default".to_string(),
            profile_snapshot_hash: "profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "model".to_string(),
            provider: "provider".to_string(),
            outcome: lattice_session::TurnOutcome::Completed {
                finish_reason: lattice_session::TurnFinishReason::Stop,
            },
            origin: lattice_session::ClosureOrigin::Live,
            commit_message: "test Turn".to_string(),
            usage: None,
            workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Empty,
            created_at: "2026-08-26T12:00:00Z".to_string(),
        }
    }

    fn placement_log_entry(
        turn_id: lancet_protocol::TurnId,
        commit_id: char,
        parent_ids: Vec<String>,
    ) -> lattice_session::TurnLogEntry {
        let turn = placement_turn(turn_id, commit_id);
        lattice_session::TurnLogEntry {
            commit_id: turn.commit_id,
            turn_id,
            parent_ids,
            outcome: turn.outcome,
            origin: turn.origin,
            agent_name: turn.agent_name,
            profile_snapshot_hash: turn.profile_snapshot_hash,
            model: turn.model,
            provider: turn.provider,
            commit_message: turn.commit_message,
            message_count: 0,
            tool_message_count: 0,
            usage: None,
            workspace_history_checkpoint: turn.workspace_history_checkpoint,
            created_at: turn.created_at,
        }
    }

    fn placement_backing(
        turn_id: Option<lancet_protocol::TurnId>,
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        WorkspaceHistoryBacking,
    ) {
        let root = tempfile::tempdir().expect("project root");
        let state = tempfile::tempdir().expect("state root");
        let backing = WorkspaceHistoryBacking::for_project_in(state.path(), root.path())
            .expect("derive backing");
        let history = WorkspaceHistory::create(backing.clone()).expect("create history");
        if let Some(turn_id) = turn_id {
            history
                .record(&lancet_runtime::MutationRecord {
                    method: "mutate.create".to_string(),
                    turn_id,
                    invocation_id: lancet_protocol::InvocationId::parse(
                        "20000000-0000-4000-8000-000000000001",
                    )
                    .expect("InvocationId"),
                    path: "placed.md".to_string(),
                    before: None,
                    after: b"placed\n".to_vec(),
                    content_hash: lancet_runtime::workspace::content_hash(b"placed\n"),
                    recorded_at: 1_700_000_000,
                })
                .expect("record mutation")
                .expect("history enabled");
        }
        (root, state, backing)
    }

    #[test]
    fn full_unwind_placement_uses_unique_typed_turn_ids() {
        let target_id = lancet_protocol::TurnId::parse("10000000-0000-4000-8000-000000000001")
            .expect("target TurnId");
        let later_id = lancet_protocol::TurnId::parse("10000000-0000-4000-8000-000000000002")
            .expect("later TurnId");
        let sibling_id = lancet_protocol::TurnId::parse("10000000-0000-4000-8000-000000000003")
            .expect("sibling TurnId");
        let target = placement_turn(target_id, 'a');

        let (_root, _state, backing) = placement_backing(None);
        let held = HeldWorkspaceOperation::acquire(&backing).expect("operation lock");
        let duplicate_log = vec![
            placement_log_entry(target_id, 'a', Vec::new()),
            placement_log_entry(target_id, 'b', vec!["a".repeat(40)]),
        ];
        let error = guard_full_unwind(backing, &held, &target, &duplicate_log, 0)
            .expect_err("duplicate TurnIds must fail closed");
        assert!(
            error.to_string().contains("repeats TurnId")
                && error.to_string().contains(&target_id.to_string()),
            "got {error:?}"
        );

        let (_root, _state, backing) = placement_backing(Some(target_id));
        let held = HeldWorkspaceOperation::acquire(&backing).expect("operation lock");
        let error = guard_full_unwind(
            backing,
            &held,
            &target,
            &[placement_log_entry(target_id, 'a', Vec::new())],
            0,
        )
        .expect_err("at-target work must not be unwound");
        assert!(
            error.to_string().contains("placed.md") && error.to_string().contains("at or before"),
            "got {error:?}"
        );

        let (_root, _state, backing) = placement_backing(Some(sibling_id));
        let held = HeldWorkspaceOperation::acquire(&backing).expect("operation lock");
        let selected_parent = "a".repeat(40);
        let selected_log = vec![
            placement_log_entry(target_id, 'a', Vec::new()),
            placement_log_entry(later_id, 'b', vec![selected_parent.clone()]),
        ];
        let sibling = placement_log_entry(sibling_id, 'c', vec![selected_parent]);
        assert_eq!(sibling.parent_ids, selected_log[1].parent_ids);
        assert_ne!(sibling.turn_id, selected_log[1].turn_id);
        let error = guard_full_unwind(backing, &held, &target, &selected_log, 0)
            .expect_err("foreign or unselected sibling work must fail closed");
        assert!(
            error.to_string().contains("uniquely reachable")
                && error.to_string().contains(&sibling_id.to_string()),
            "got {error:?}"
        );

        let (_root, _state, backing) = placement_backing(Some(later_id));
        let held = HeldWorkspaceOperation::acquire(&backing).expect("operation lock");
        guard_full_unwind(
            backing,
            &held,
            &target,
            &[
                placement_log_entry(target_id, 'a', Vec::new()),
                placement_log_entry(later_id, 'b', vec!["a".repeat(40)]),
            ],
            0,
        )
        .expect("a uniquely placed post-target mutation is safe to unwind");
    }

    #[cfg(feature = "test-crash-barriers")]
    #[test]
    fn rewind_barrier_ack_is_parse_complete_and_newline_terminated() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("rewind-ack.json");
        let expected = json!({ "barrier": "rewind_workspace_applied", "facts": { "count": 3 } });

        publish_rewind_barrier_ack(&path, &expected).expect("publish Rewind barrier ack");

        let bytes = std::fs::read(&path).expect("read Rewind barrier ack");
        assert_eq!(bytes.last(), Some(&b'\n'));
        let parsed: serde_json::Value =
            serde_json::from_slice(&bytes).expect("parse complete Rewind barrier ack");
        assert_eq!(parsed, expected);
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("read ack directory")
                .count(),
            1,
            "successful publication must leave only the final ack"
        );
    }

    #[cfg(feature = "test-crash-barriers")]
    #[test]
    fn rewind_barrier_ack_preserves_existing_final_and_cleans_temp() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("rewind-ack.json");
        let original = b"{\"existing\":true}\n";
        std::fs::write(&path, original).expect("seed existing final ack");

        let error = publish_rewind_barrier_ack(&path, &json!({ "replacement": true }))
            .expect_err("existing final ack must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&path).expect("read existing final ack"),
            original,
            "existing final ack must never be truncated or overwritten"
        );
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("read ack directory")
                .count(),
            1,
            "failed publication must remove its temporary file"
        );
    }
}
