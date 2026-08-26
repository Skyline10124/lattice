// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Immutable `SessionState` objects and the session ref namespace
//! (ADR 0005 §6, GitHub #143).
//!
//! Mutable `.lattice/sessions/**` sidecars are gone. Every fact about a
//! Session lives in `sessions.git`:
//!
//! ```text
//! refs/lattice/sessions/<SessionId>/state            -> SessionState commit
//! refs/lattice/sessions/<SessionId>/heads/<Branch>   -> conversation branch
//! refs/lattice/candidates/<SessionId>/<TurnId>       -> clean-turn reachability
//! refs/lattice/session-validation/<SessionId>        -> derived open projection
//! ```
//!
//! (`turns/<TurnId>` pins and the `active-turn` journal arrive with #144.)
//! The validation projection is derived, never a second authority; see
//! `validation.rs`.
//! A state mutation writes a new immutable commit whose parent is the
//! previous state commit and CASes the `state` ref; the SessionState payload
//! is canonical JSON at `state.json` in the commit tree.

use serde::{Deserialize, Serialize};

use crate::graph::{GraphHead, GraphSession};
use crate::{Result, SessionError};
use lattice_store::{RefExpectation, RefTxnEdit, StoreRepo, TreeSpec};

/// Domain store kind recorded in `refs/lattice/meta`.
pub(crate) const SESSION_STORE_KIND: &str = "session";
/// Domain schema version of the session store contents.
pub(crate) const SESSION_STORE_SCHEMA_VERSION: u32 = 3;

/// File inside a state commit's tree carrying the canonical JSON payload.
pub(crate) const STATE_TREE_FILE: &str = "state.json";

/// Ref holding the immutable SessionState of one session.
pub(crate) fn state_ref_name(session_id: &str) -> Result<String> {
    validate_session_ref_id(session_id)?;
    Ok(format!("refs/lattice/sessions/{session_id}/state"))
}

/// Ref holding one conversation branch head.
pub(crate) fn head_ref_name(session_id: &str, branch: &str) -> Result<String> {
    validate_session_ref_id(session_id)?;
    crate::validate_branch_name(branch)?;
    Ok(format!("refs/lattice/sessions/{session_id}/heads/{branch}"))
}

/// Prefix enumerating every conversation branch head of one session.
pub(crate) fn heads_prefix(session_id: &str) -> Result<String> {
    validate_session_ref_id(session_id)?;
    Ok(format!("refs/lattice/sessions/{session_id}/heads/"))
}

/// Prefix enumerating every session-scoped ref (used by list/open
/// invariant validation).
pub(crate) fn sessions_prefix() -> &'static str {
    "refs/lattice/sessions/"
}

pub(crate) fn validate_session_ref_id(session_id: &str) -> Result<()> {
    validate_ref_component(session_id, "session_id")
}

pub(crate) fn validate_turn_ref_id(turn_id: &str) -> Result<()> {
    validate_ref_component(turn_id, "turn_id")
}

fn validate_ref_component(value: &str, field: &str) -> Result<()> {
    if value.is_empty() {
        return Err(SessionError::InvalidInput(format!(
            "{field} must not be empty"
        )));
    }
    if value.len() > 128 {
        return Err(SessionError::InvalidInput(format!(
            "{field} exceeds 128 characters"
        )));
    }
    let mut chars = value.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_alphanumeric() {
        return Err(SessionError::InvalidInput(format!(
            "{field} must start with alphanumeric character"
        )));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err(SessionError::InvalidInput(format!(
            "{field} must contain only alphanumeric, '.', '_', or '-'"
        )));
    }
    Ok(())
}

/// HEAD selector inside a SessionState: attached to a branch or detached at
/// a commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum SessionHead {
    /// Attached: HEAD follows `refs/lattice/sessions/<id>/heads/<branch>`.
    Attached {
        /// Branch name.
        branch: String,
    },
    /// Detached: HEAD is pinned at one commit.
    Detached {
        /// Commit id.
        commit_id: String,
    },
}

/// One immutable SessionState: the complete authority for a Session's
/// identity, project binding, and head selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionState {
    pub session_id: String,
    /// Lowercase-hex `ProjectStoreKey` this Session is permanently bound to.
    pub project_store_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub active_agent_name: String,
    pub project_target_branch: String,
    pub created_at: String,
    pub updated_at: String,
    pub head: SessionHead,
}

impl SessionState {
    /// The public head view of this state.
    pub(crate) fn graph_head(&self) -> GraphHead {
        match &self.head {
            SessionHead::Attached { branch } => GraphHead::Branch {
                name: branch.clone(),
            },
            SessionHead::Detached { commit_id } => GraphHead::Detached {
                commit_id: commit_id.clone(),
            },
        }
    }

    /// The public session view of this state.
    pub(crate) fn to_graph_session(&self) -> GraphSession {
        GraphSession {
            session_id: self.session_id.clone(),
            label: self.label.clone(),
            active_agent_name: self.active_agent_name.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            project_target_branch: Some(self.project_target_branch.clone()),
            project_store_key: self.project_store_key.clone(),
        }
    }
}

/// Read the current SessionState for `session_id` and the state commit oid
/// it was read from. A missing state ref is `NotFound`; a malformed object
/// is typed corruption.
pub(crate) fn read_state(
    store: &StoreRepo,
    session_id: &str,
) -> Result<(gix::ObjectId, SessionState)> {
    let ref_name = state_ref_name(session_id)?;
    let oid = store
        .ref_target(&ref_name)
        .map_err(SessionError::from)?
        .ok_or_else(|| SessionError::NotFound(session_id.to_string()))?;
    let bytes = store.read_tree_blob(oid, STATE_TREE_FILE).map_err(|_| {
        SessionError::Store(lattice_store::StoreError::Corruption {
            detail: format!("session '{session_id}' state commit has no {STATE_TREE_FILE}"),
        })
    })?;
    let state: SessionState =
        serde_json::from_slice(&bytes).map_err(|e| SessionError::InvalidPersistedValue {
            field: "state.json",
            value: e.to_string(),
        })?;
    if state.session_id != session_id {
        return Err(SessionError::Store(lattice_store::StoreError::Corruption {
            detail: format!(
                "state ref for '{session_id}' names session '{}'",
                state.session_id
            ),
        }));
    }
    Ok((oid, state))
}

/// Serialize a SessionState and write it as a new immutable commit whose
/// parent is the previous state commit (if any). Returns the new commit oid;
/// the caller publishes it via CAS.
pub(crate) fn write_state_commit(
    store: &StoreRepo,
    previous: Option<gix::ObjectId>,
    state: &SessionState,
) -> Result<gix::ObjectId> {
    let value = serde_json::to_value(state)
        .map_err(|e| SessionError::InvalidInput(format!("serialize state: {e}")))?;
    let bytes = crate::graph::canonical_json_bytes(&value);
    let blob = store.write_blob(&bytes)?;
    let tree = store.write_tree(TreeSpec::new().upsert(
        STATE_TREE_FILE,
        gix::objs::tree::EntryKind::Blob,
        blob,
    ))?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(&state.updated_at)
        .map_err(|e| SessionError::InvalidInput(format!("parse state timestamp: {e}")))?
        .timestamp();
    let parents: Vec<gix::ObjectId> = previous.into_iter().collect();
    let oid = store.write_commit_at(
        tree,
        &parents,
        &format!("session {} state", state.session_id),
        timestamp,
    )?;
    Ok(oid)
}

/// Publish a new state commit under CAS on the state ref and fsync the
/// publication. Caller holds the session execution lock and the repo write
/// lock.
pub(crate) fn publish_state(
    store: &StoreRepo,
    previous: Option<gix::ObjectId>,
    state: &SessionState,
) -> Result<gix::ObjectId> {
    let oid = write_state_commit(store, previous, state)?;
    let ref_name = state_ref_name(&state.session_id)?;
    let expect = match previous {
        Some(old) => RefExpectation::MustMatch(old),
        None => RefExpectation::MustNotExist,
    };
    super::validation::publish_session_edits(
        store,
        &state.session_id,
        vec![RefTxnEdit {
            name: ref_name.clone(),
            expect,
            new: Some(oid),
        }],
    )
    .map_err(map_cas_conflict)?;
    Ok(oid)
}

/// Map a kernel CAS conflict on a session ref to `SessionError::Conflict`.
pub(crate) fn map_cas_conflict(err: lattice_store::StoreError) -> SessionError {
    match err {
        lattice_store::StoreError::CasConflict { detail, .. } => SessionError::Conflict(detail),
        other => SessionError::Store(other),
    }
}
