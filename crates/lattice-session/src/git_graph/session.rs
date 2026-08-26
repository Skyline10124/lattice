// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session create/open/status/list over immutable SessionState objects.

use chrono::Utc;
use lattice_store::{Blocking, LockMode, ProjectStoreKey};

use super::state::{self, SessionHead, SessionState};
use super::{DEFAULT_BRANCH, GitSessionGraphStore};
use crate::graph::{GraphRef, GraphRefKind, GraphSession, SessionGraphSummary, SessionStatus};
use crate::{Result, SessionError};

impl GitSessionGraphStore {
    pub(super) fn create_session_impl(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
        project_store_key: &ProjectStoreKey,
    ) -> Result<GraphSession> {
        state::validate_session_ref_id(session_id)?;
        crate::validate_branch_name(project_target_branch)?;

        // Mutation: session execution lock (new session's own lock) then the
        // repository write lock.
        let _exec =
            self.acquire_execution_lock(session_id, LockMode::Exclusive, Blocking::Block)?;
        let _write = self.kernel().write_lock()?;

        if self
            .kernel()
            .ref_target(&state::state_ref_name(session_id)?)?
            .is_some()
        {
            return Err(SessionError::Conflict(format!(
                "session '{session_id}' already exists"
            )));
        }

        let now = Utc::now().to_rfc3339();
        let session_state = SessionState {
            session_id: session_id.to_string(),
            project_store_key: project_store_key.to_string(),
            label: label.map(|s| s.to_string()),
            active_agent_name: active_agent_name.to_string(),
            project_target_branch: project_target_branch.to_string(),
            created_at: now.clone(),
            updated_at: now,
            head: SessionHead::Attached {
                branch: DEFAULT_BRANCH.to_string(),
            },
        };
        state::publish_state(self.kernel(), None, &session_state)?;
        Ok(session_state.to_graph_session())
    }

    pub(super) fn open_session_impl(&self, session_id: &str) -> Result<GraphSession> {
        let (_, session_state) = state::read_state(self.kernel(), session_id)?;
        Ok(session_state.to_graph_session())
    }

    pub(super) fn status_impl(&self, session_id: &str) -> Result<SessionStatus> {
        // Coherent read of mutable session status: non-blocking shared hold;
        // an exclusive owner means a live Turn, which is SessionBusy, never
        // corruption.
        let _shared =
            self.acquire_execution_lock(session_id, LockMode::Shared, Blocking::NonBlocking)?;
        self.status_locked_impl(session_id)
    }

    /// Read status while the caller already holds the exclusive execution
    /// lock. This is the explicit full-Rewind/full-Turn path; it must not try
    /// to infer ownership by acquiring the same path again.
    pub(super) fn status_locked_impl(&self, session_id: &str) -> Result<SessionStatus> {
        let (_, session_state) = state::read_state(self.kernel(), session_id)?;

        let branches = enumerate_session_branches(self.kernel(), session_id)?;
        let head_oid = resolve_head_oid(self.kernel(), &session_state, session_id)?;
        let commit_count = count_turn_commits(self.kernel(), head_oid)?;

        Ok(SessionStatus {
            session_id: session_id.to_string(),
            head: session_state.graph_head(),
            current_commit_id: head_oid.map(|o| o.to_string()),
            active_agent_name: session_state.active_agent_name.clone(),
            branches,
            commit_count,
            project_target_branch: Some(session_state.project_target_branch.clone()),
            project_store_key: session_state.project_store_key.clone(),
        })
    }

    pub(super) fn list_impl(&self) -> Result<Vec<SessionGraphSummary>> {
        let mut summaries = Vec::new();
        for (name, oid) in self.kernel().enumerate_refs(state::sessions_prefix())? {
            let Some(rest) = name.strip_prefix(state::sessions_prefix()) else {
                continue;
            };
            let Some((session_id, "state")) = rest.split_once('/') else {
                continue;
            };
            let (_, session_state) = state::read_state(self.kernel(), session_id)?;
            let head_oid = resolve_head_oid(self.kernel(), &session_state, session_id)?;
            let commit_count = count_turn_commits(self.kernel(), head_oid)?;
            let _ = oid;
            summaries.push(SessionGraphSummary {
                session_id: session_state.session_id.clone(),
                label: session_state.label.clone(),
                active_agent_name: session_state.active_agent_name.clone(),
                current_branch: match &session_state.head {
                    SessionHead::Attached { branch } => Some(branch.clone()),
                    SessionHead::Detached { .. } => None,
                },
                head_commit_id: head_oid.map(|o| o.to_string()),
                commit_count,
                updated_at: session_state.updated_at.clone(),
                project_target_branch: Some(session_state.project_target_branch.clone()),
                project_store_key: session_state.project_store_key.clone(),
            });
        }
        summaries.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        Ok(summaries)
    }

    pub(super) fn set_active_agent_impl(
        &self,
        session_id: &str,
        agent_name: &str,
    ) -> Result<GraphSession> {
        let _exec =
            self.acquire_execution_lock(session_id, LockMode::Exclusive, Blocking::Block)?;
        let _write = self.kernel().write_lock()?;
        let (previous, mut session_state) = state::read_state(self.kernel(), session_id)?;
        session_state.active_agent_name = agent_name.to_string();
        session_state.updated_at = Utc::now().to_rfc3339();
        state::publish_state(self.kernel(), Some(previous), &session_state)?;
        Ok(session_state.to_graph_session())
    }
}

fn count_turn_commits(
    store: &lattice_store::StoreRepo,
    head: Option<gix::ObjectId>,
) -> Result<usize> {
    let Some(head) = head else {
        return Ok(0);
    };
    let mut count = 0;
    for oid in store.rev_walk(&[head], &[])? {
        if !super::pending::is_journal_commit(store, oid)? {
            count += 1;
        }
    }
    Ok(count)
}
pub(super) fn enumerate_session_branches(
    store: &lattice_store::StoreRepo,
    session_id: &str,
) -> Result<Vec<GraphRef>> {
    let prefix = state::heads_prefix(session_id)?;
    let mut refs = Vec::new();
    for (name, oid) in store.enumerate_refs(&prefix)? {
        let branch_name = name.strip_prefix(&prefix).unwrap_or(&name).to_string();
        // `updated_at` reflects enumeration time: Git exposes no per-ref
        // write timestamp without reflog parsing, and bare stores may have
        // no reflog. Callers needing the true time read the commit's
        // committer timestamp via `commit_id`.
        refs.push(GraphRef {
            session_id: session_id.to_string(),
            name: branch_name,
            kind: GraphRefKind::Branch,
            commit_id: Some(oid.to_string()),
            active_agent_name: String::new(),
            updated_at: Utc::now().to_rfc3339(),
        });
    }
    Ok(refs)
}

/// Resolve the current HEAD oid from the session state: the live branch ref
/// when attached, the pinned commit when detached.
pub(super) fn resolve_head_oid(
    store: &lattice_store::StoreRepo,
    session_state: &SessionState,
    session_id: &str,
) -> Result<Option<gix::ObjectId>> {
    match &session_state.head {
        SessionHead::Attached { branch } => {
            let ref_name = state::head_ref_name(session_id, branch)?;
            Ok(store.ref_target(&ref_name)?)
        }
        SessionHead::Detached { commit_id } => {
            let oid = super::helpers::parse_oid(commit_id)
                .map_err(|e| SessionError::Git(format!("parse detached HEAD commit id: {e}")))?;
            Ok(Some(oid))
        }
    }
}
