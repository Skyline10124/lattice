// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Branch, checkout, reset, and diff over session-scoped refs.

use chrono::Utc;
use lattice_store::{Blocking, LockMode, RefExpectation, RefTxnEdit};

use super::GitSessionGraphStore;
use super::helpers::{parse_manifest, parse_oid, turn_log_entry_from_manifest};
use super::session::resolve_head_oid;
use super::state::{self, SessionHead};
use super::validation;
use crate::graph::{GraphDiff, GraphHead, GraphRef, GraphRefKind, ResetResult, TurnLogEntry};
use crate::{Result, SessionError};

impl GitSessionGraphStore {
    pub(super) fn branch_impl(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
    ) -> Result<GraphRef> {
        crate::validate_branch_name(name)?;
        state::validate_session_ref_id(session_id)?;
        let _exec =
            self.acquire_execution_lock(session_id, LockMode::Exclusive, Blocking::Block)?;
        self.branch_locked_impl(session_id, name, from)
    }

    pub(super) fn branch_locked_impl(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
    ) -> Result<GraphRef> {
        crate::validate_branch_name(name)?;
        state::validate_session_ref_id(session_id)?;
        let _write = self.kernel().write_lock()?;
        let (_, session_state) = state::read_state(self.kernel(), session_id)?;

        let target_oid = match from {
            Some(target) => resolve_target_to_oid(self.kernel(), session_id, target)?,
            None => {
                resolve_head_oid(self.kernel(), &session_state, session_id)?.ok_or_else(|| {
                    SessionError::Conflict("cannot create branch: no commits on HEAD".to_string())
                })?
            }
        };

        let ref_name = state::head_ref_name(session_id, name)?;
        // Branch names are unique even when the target commit coincides with
        // an existing branch (gix MustNotExist is idempotent on identical
        // targets), so the duplicate check is explicit.
        if self.kernel().ref_target(&ref_name)?.is_some() {
            return Err(SessionError::Conflict(format!(
                "branch '{name}' already exists"
            )));
        }
        validation::publish_session_edits(
            self.kernel(),
            session_id,
            vec![RefTxnEdit {
                name: ref_name.clone(),
                expect: RefExpectation::MustNotExist,
                new: Some(target_oid),
            }],
        )
        .map_err(|err| match err {
            lattice_store::StoreError::CasConflict { .. } => {
                SessionError::Conflict(format!("branch '{name}' already exists"))
            }
            other => SessionError::Store(other),
        })?;
        self.kernel().fsync_pending(&[ref_name.as_str()])?;

        Ok(GraphRef {
            session_id: session_id.to_string(),
            name: name.to_string(),
            kind: GraphRefKind::Branch,
            commit_id: Some(target_oid.to_string()),
            active_agent_name: session_state.active_agent_name,
            updated_at: Utc::now().to_rfc3339(),
        })
    }

    pub(super) fn checkout_impl(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        state::validate_session_ref_id(session_id)?;
        let _exec =
            self.acquire_execution_lock(session_id, LockMode::Exclusive, Blocking::Block)?;
        self.checkout_locked_impl(session_id, target)
    }

    pub(super) fn checkout_locked_impl(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        state::validate_session_ref_id(session_id)?;
        let _write = self.kernel().write_lock()?;
        let (previous, mut session_state) = state::read_state(self.kernel(), session_id)?;

        // Try as branch first: attach; HEAD follows the live branch ref.
        if crate::validate_branch_name(target).is_ok() {
            let ref_name = state::head_ref_name(session_id, target)?;
            if self.kernel().ref_target(&ref_name)?.is_some() {
                session_state.head = SessionHead::Attached {
                    branch: target.to_string(),
                };
                session_state.updated_at = Utc::now().to_rfc3339();
                state::publish_state(self.kernel(), Some(previous), &session_state)?;
                return Ok(GraphHead::Branch {
                    name: target.to_string(),
                });
            }
        }

        // Try as commit OID: detach.
        let oid = parse_oid(target).map_err(|_| SessionError::MissingCommit(target.to_string()))?;
        self.kernel()
            .commit_tree(oid)
            .map_err(|_| SessionError::MissingCommit(target.to_string()))?;
        session_state.head = SessionHead::Detached {
            commit_id: target.to_string(),
        };
        session_state.updated_at = Utc::now().to_rfc3339();
        state::publish_state(self.kernel(), Some(previous), &session_state)?;
        Ok(GraphHead::Detached {
            commit_id: target.to_string(),
        })
    }

    pub(super) fn reset_impl(&self, session_id: &str, target: &str) -> Result<ResetResult> {
        state::validate_session_ref_id(session_id)?;
        let _exec =
            self.acquire_execution_lock(session_id, LockMode::Exclusive, Blocking::Block)?;
        self.reset_locked_impl(session_id, target)
    }

    pub(super) fn reset_locked_impl(&self, session_id: &str, target: &str) -> Result<ResetResult> {
        state::validate_session_ref_id(session_id)?;
        let _write = self.kernel().write_lock()?;
        let (previous, mut session_state) = state::read_state(self.kernel(), session_id)?;
        let branch_name = match &session_state.head {
            SessionHead::Attached { branch } => branch.clone(),
            SessionHead::Detached { .. } => {
                return Err(SessionError::Conflict(
                    "cannot reset while HEAD is detached; checkout a branch first".to_string(),
                ));
            }
        };

        let target_oid =
            parse_oid(target).map_err(|e| SessionError::Git(format!("parse reset target: {e}")))?;
        // Verify the target exists as a commit.
        self.kernel()
            .commit_tree(target_oid)
            .map_err(|_| SessionError::MissingCommit(target.to_string()))?;

        let ref_name = state::head_ref_name(session_id, &branch_name)?;
        let old_oid = self
            .kernel()
            .ref_target(&ref_name)?
            .ok_or_else(|| SessionError::Git("branch ref has no target".to_string()))?;

        // One atomic publication: move the branch under CAS and record the
        // head bump in the same transaction.
        session_state.updated_at = Utc::now().to_rfc3339();
        let new_state_oid =
            state::write_state_commit(self.kernel(), Some(previous), &session_state)?;
        validation::publish_session_edits(
            self.kernel(),
            session_id,
            vec![
                RefTxnEdit {
                    name: ref_name.clone(),
                    expect: RefExpectation::MustMatch(old_oid),
                    new: Some(target_oid),
                },
                RefTxnEdit {
                    name: state::state_ref_name(session_id)?,
                    expect: RefExpectation::MustMatch(previous),
                    new: Some(new_state_oid),
                },
            ],
        )
        .map_err(state::map_cas_conflict)?;
        self.kernel().fsync_pending(&[
            ref_name.as_str(),
            state::state_ref_name(session_id)?.as_str(),
        ])?;

        Ok(ResetResult {
            session_id: session_id.to_string(),
            branch: branch_name,
            old_commit_id: Some(old_oid.to_string()),
            new_commit_id: target.to_string(),
        })
    }

    pub(super) fn diff_impl(&self, session_id: &str, from: &str, to: &str) -> Result<GraphDiff> {
        state::validate_session_ref_id(session_id)?;
        let from_oid = resolve_target_to_oid(self.kernel(), session_id, from)?;
        let to_oid = resolve_target_to_oid(self.kernel(), session_id, to)?;

        let lca_oid = self.kernel().merge_base(from_oid, to_oid)?;
        let common_ancestor_id = lca_oid.map(|o| o.to_string());

        let only_in_from = collect_unique_commits(self.kernel(), from_oid, lca_oid)?;
        let only_in_to = collect_unique_commits(self.kernel(), to_oid, lca_oid)?;

        Ok(GraphDiff {
            from_commit_id: from_oid.to_string(),
            to_commit_id: to_oid.to_string(),
            common_ancestor_id,
            only_in_from,
            only_in_to,
        })
    }
}

fn resolve_target_to_oid(
    store: &lattice_store::StoreRepo,
    session_id: &str,
    target: &str,
) -> Result<gix::ObjectId> {
    // Try as branch ref first.
    if crate::validate_branch_name(target).is_ok() {
        let ref_name = state::head_ref_name(session_id, target)?;
        if let Some(oid) = store.ref_target(&ref_name)? {
            return Ok(oid);
        }
    }

    // Try as commit OID.
    let oid = parse_oid(target).map_err(|_| SessionError::MissingCommit(target.to_string()))?;
    store
        .commit_tree(oid)
        .map_err(|_| SessionError::MissingCommit(target.to_string()))?;
    Ok(oid)
}

fn collect_unique_commits(
    store: &lattice_store::StoreRepo,
    tip: gix::ObjectId,
    hide: Option<gix::ObjectId>,
) -> Result<Vec<TurnLogEntry>> {
    let hide_vec: Vec<gix::ObjectId> = hide.into_iter().collect();
    // Newest-first to match historical TOPOLOGICAL (no reverse) order used
    // by the previous implementation of this collector.
    let walk = store.rev_walk(&[tip], &hide_vec)?;

    let mut entries = Vec::new();
    for oid in walk {
        if super::pending::is_journal_commit(store, oid)? {
            continue;
        }
        let manifest_bytes = store.read_tree_blob(oid, "turn/manifest.json")?;
        let manifest = parse_manifest(&manifest_bytes)?;
        let messages_bytes = store.read_tree_blob(oid, "turn/messages.jsonl")?;
        let messages_jsonl = String::from_utf8(messages_bytes)
            .map_err(|e| SessionError::Git(format!("messages.jsonl not utf8: {e}")))?;
        let total = messages_jsonl.lines().filter(|l| !l.is_empty()).count();
        let tools = messages_jsonl
            .lines()
            .filter(|l| !l.is_empty())
            .filter(|l| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
                    v.get("role").and_then(|r| r.as_str()) == Some("tool")
                } else {
                    false
                }
            })
            .count();
        entries.push(turn_log_entry_from_manifest(
            &manifest,
            &oid.to_string(),
            total,
            tools,
        ));
    }
    Ok(entries)
}
