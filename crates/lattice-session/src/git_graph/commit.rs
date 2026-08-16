// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Commit creation, transcript reconstruction, and log.

use chrono::Utc;
use lattice_store::{Blocking, LockMode, RefExpectation, RefTxnEdit};

use super::GitSessionGraphStore;
use super::helpers::{
    GitSessionManifest, MANIFEST_SCHEMA_VERSION, build_agent_json, build_audit_json,
    build_root_tree, build_turn_tree, derive_turn_id, messages_to_jsonl, parse_manifest,
    parse_messages_jsonl, parse_oid, serialize_manifest, walk_commits_oldest_first,
};
use super::session::resolve_head_oid;
use super::state::{self, SessionHead};
use crate::graph::{
    HeadTranscript, PreparedTurnCommit, TurnCommit, TurnCommitInput, TurnCommitStatus,
    TurnLifecycleEvidence, TurnLogEntry, WorkspaceDisposition,
};
use crate::{Result, SessionError};

impl GitSessionGraphStore {
    pub(super) fn commit_turn_impl(&self, input: TurnCommitInput) -> Result<TurnCommit> {
        let now = Utc::now().to_rfc3339();
        let prepared = crate::graph::prepare_turn_commit(&input, &now);
        let _exec = self.acquire_execution_lock(
            &prepared.commit.session_id,
            LockMode::Exclusive,
            Blocking::Block,
        )?;
        self.commit_prepared_turn_locked_impl(prepared)
    }

    pub(super) fn commit_turn_impl_locked(&self, input: TurnCommitInput) -> Result<TurnCommit> {
        let now = Utc::now().to_rfc3339();
        let prepared = crate::graph::prepare_turn_commit(&input, &now);
        self.commit_prepared_turn_locked_impl(prepared)
    }

    pub(super) fn commit_prepared_turn_impl(
        &self,
        prepared: PreparedTurnCommit,
    ) -> Result<TurnCommit> {
        let commit = &prepared.commit;
        let session_id = &commit.session_id;

        // Mutation: session execution lock (re-entrant under the caller's
        // explicit full-Turn hold) then the repository write lock.
        let _exec =
            self.acquire_execution_lock(session_id, LockMode::Exclusive, Blocking::Block)?;
        self.commit_prepared_turn_locked_impl(prepared)
    }

    /// Commit with an already-held SessionExecutionLock. The caller supplies
    /// the lock capability through the trait's `commit_turn_locked` method;
    /// this implementation only takes the narrower repository lock.
    pub(super) fn commit_prepared_turn_locked_impl(
        &self,
        prepared: PreparedTurnCommit,
    ) -> Result<TurnCommit> {
        let commit = &prepared.commit;
        let session_id = &commit.session_id;
        let _write = self.kernel().write_lock()?;

        let (state_oid, session_state) = state::read_state(self.kernel(), session_id)?;
        let branch = match &session_state.head {
            SessionHead::Attached { branch } => branch.clone(),
            SessionHead::Detached { .. } => {
                return Err(SessionError::Conflict(
                    "cannot commit a Turn while HEAD is detached; checkout a branch first"
                        .to_string(),
                ));
            }
        };

        // Resolve current HEAD OID for the parent checks and the CAS.
        let head_oid = resolve_head_oid(self.kernel(), &session_state, session_id)?;

        if commit.parent_ids.is_empty() {
            if head_oid.is_some() {
                return Err(SessionError::Conflict(
                    "expected no parent (first commit) but HEAD is not empty".to_string(),
                ));
            }
        } else {
            let expected_parent = &commit.parent_ids[0];
            let expected = parse_oid(expected_parent)?;
            match head_oid {
                Some(head) if head == expected => {}
                Some(head) => {
                    return Err(SessionError::Conflict(format!(
                        "expected parent '{expected_parent}' but HEAD is '{head}'"
                    )));
                }
                None => {
                    return Err(SessionError::Conflict(format!(
                        "expected parent '{expected_parent}' but HEAD is empty"
                    )));
                }
            }
        }

        // Build manifest and tree.
        let manifest = build_manifest_from_prepared(&prepared);
        let manifest_bytes = serialize_manifest(&manifest)?;
        let messages_jsonl = messages_to_jsonl(&prepared.messages)?;
        let agent_json = build_agent_json(&commit.agent_name);
        let profile_json = commit.profile_snapshot_json.clone();
        let audit_json = build_audit_json(&commit.lifecycle);

        let turn_tree_oid = build_turn_tree(
            self.kernel(),
            &manifest_bytes,
            &messages_jsonl,
            &agent_json,
            &profile_json,
            &audit_json,
        )?;
        let root_tree_oid = build_root_tree(self.kernel(), turn_tree_oid)?;

        // Build signature from created_at timestamp.
        let ts = chrono::DateTime::parse_from_rfc3339(&commit.created_at)
            .map_err(|e| SessionError::Git(format!("parse commit timestamp: {e}")))?
            .timestamp();

        let parent_oids: Result<Vec<gix::ObjectId>> =
            commit.parent_ids.iter().map(|pid| parse_oid(pid)).collect();
        let parent_oids = parent_oids?;

        // Write the commit; publication is one multi-ref transaction below.
        let commit_oid = self.kernel().write_commit_at(
            root_tree_oid,
            &parent_oids,
            &commit.commit_message,
            ts,
        )?;

        // One atomic publication: advance the conversation branch under CAS,
        // record the new head in the state object, and (when present) pin
        // the candidate ref. Any precondition failure leaves the whole
        // transaction unpublished.
        let head_ref = state::head_ref_name(session_id, &branch)?;
        let branch_expect = match head_oid {
            Some(expected) => RefExpectation::MustMatch(expected),
            None => RefExpectation::MustNotExist,
        };

        let mut new_state = session_state.clone();
        new_state.updated_at = Utc::now().to_rfc3339();
        let new_state_oid = state::write_state_commit(self.kernel(), Some(state_oid), &new_state)?;

        let mut edits = vec![
            RefTxnEdit {
                name: head_ref.clone(),
                expect: branch_expect,
                new: Some(commit_oid),
            },
            RefTxnEdit {
                name: state::state_ref_name(session_id)?,
                expect: RefExpectation::MustMatch(state_oid),
                new: Some(new_state_oid),
            },
        ];

        // Candidate ref (SC4): keep clean-turn commit ranges reachable.
        let mut candidate_ref = None;
        if let Some(lifecycle) = &prepared.commit.lifecycle
            && lifecycle.candidate_oid.is_some()
        {
            let turn_id = &manifest.turn_id;
            state::validate_turn_ref_id(turn_id)?;
            let cand_ref = state::candidate_ref_name(session_id, turn_id)?;
            // Idempotent: a candidate already pinned at this exact commit is
            // a no-op under gix MustNotExist semantics; a different target
            // aborts the whole transaction.
            if self.kernel().ref_target(&cand_ref)? != Some(commit_oid) {
                edits.push(RefTxnEdit {
                    name: cand_ref.clone(),
                    expect: RefExpectation::MustNotExist,
                    new: Some(commit_oid),
                });
                candidate_ref = Some(cand_ref);
            }
        }

        self.kernel()
            .ref_transaction(&edits)
            .map_err(|err| match err {
                lattice_store::StoreError::CasConflict { .. } => {
                    SessionError::Conflict("session branch ref moved during commit".to_string())
                }
                other => SessionError::Store(other),
            })?;

        let state_ref_string = state::state_ref_name(session_id)?;
        let mut published_refs: Vec<&str> = vec![head_ref.as_str(), state_ref_string.as_str()];
        if let Some(cand) = &candidate_ref {
            published_refs.push(cand.as_str());
        }
        self.kernel().fsync_pending(&published_refs)?;

        let oid_str = commit_oid.to_string();
        let message_ids: Vec<String> = (0..prepared.messages.len())
            .map(|i| format!("{oid_str}:{i}"))
            .collect();

        let mut turn_commit = turn_commit_from_manifest_and_oid(commit_oid, &manifest);
        turn_commit.message_ids = message_ids;
        Ok(turn_commit)
    }

    pub(super) fn head_transcript_impl(&self, session_id: &str) -> Result<HeadTranscript> {
        let (_, session_state) = state::read_state(self.kernel(), session_id)?;
        let head_oid = match resolve_head_oid(self.kernel(), &session_state, session_id)? {
            Some(oid) => oid,
            None => {
                return Ok(HeadTranscript {
                    session_id: session_id.to_string(),
                    parent_commit_id: None,
                    messages: Vec::new(),
                });
            }
        };

        let oids = walk_commits_oldest_first(self.kernel(), head_oid, None)?;
        let mut all_messages = Vec::new();
        for oid in oids {
            let bytes = self.kernel().read_tree_blob(oid, "turn/messages.jsonl")?;
            let jsonl = String::from_utf8(bytes)
                .map_err(|e| SessionError::Git(format!("messages.jsonl not utf8: {e}")))?;
            let messages = parse_messages_jsonl(&jsonl)?;
            all_messages.extend(messages);
        }

        Ok(HeadTranscript {
            session_id: session_id.to_string(),
            parent_commit_id: Some(head_oid.to_string()),
            messages: all_messages,
        })
    }

    pub(super) fn log_impl(&self, session_id: &str) -> Result<Vec<TurnLogEntry>> {
        let (_, session_state) = state::read_state(self.kernel(), session_id)?;
        let head_oid = match resolve_head_oid(self.kernel(), &session_state, session_id)? {
            Some(oid) => oid,
            None => return Ok(Vec::new()),
        };

        let oids = walk_commits_oldest_first(self.kernel(), head_oid, None)?;
        let mut entries = Vec::new();
        for oid in oids {
            let manifest_bytes = self.kernel().read_tree_blob(oid, "turn/manifest.json")?;
            let manifest = parse_manifest(&manifest_bytes)?;
            let messages_bytes = self.kernel().read_tree_blob(oid, "turn/messages.jsonl")?;
            let messages_jsonl = String::from_utf8(messages_bytes)
                .map_err(|e| SessionError::Git(format!("messages.jsonl not utf8: {e}")))?;
            let (message_count, tool_message_count) = count_messages(&messages_jsonl);
            let entry = super::helpers::turn_log_entry_from_manifest(
                &manifest,
                &oid.to_string(),
                message_count,
                tool_message_count,
            );
            entries.push(entry);
        }
        Ok(entries)
    }

    pub(super) fn get_turn_commit_impl(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> Result<Option<TurnCommit>> {
        let oid = match parse_oid(commit_id) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        if self.kernel().commit_tree(oid).is_err() {
            return Ok(None);
        }
        let manifest_bytes = match self.kernel().read_tree_blob(oid, "turn/manifest.json") {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let manifest = parse_manifest(&manifest_bytes)?;
        // Enforce session ownership: a commit from session A must not be
        // returned when queried with session B's ID.
        if manifest.session_id != session_id {
            return Ok(None);
        }
        let messages_bytes = self.kernel().read_tree_blob(oid, "turn/messages.jsonl")?;
        let messages_jsonl = String::from_utf8(messages_bytes)
            .map_err(|e| SessionError::Git(format!("messages.jsonl not utf8: {e}")))?;
        let messages = parse_messages_jsonl(&messages_jsonl)?;
        let message_ids: Vec<String> = (0..messages.len())
            .map(|i| format!("{commit_id}:{i}"))
            .collect();

        let mut turn = turn_commit_from_manifest_and_oid(oid, &manifest);
        // Read the full profile snapshot JSON from the commit tree so that
        // consumers relying on `profile_snapshot_json` (replay, audit) get the
        // real payload instead of an empty string.
        let profile_bytes = self.kernel().read_tree_blob(oid, "turn/profile.json")?;
        let profile_json = String::from_utf8(profile_bytes)
            .map_err(|e| SessionError::Git(format!("profile.json not utf8: {e}")))?;
        turn.profile_snapshot_json = profile_json;
        turn.message_ids = message_ids;
        Ok(Some(turn))
    }
}

pub(super) fn build_manifest_from_prepared(prepared: &PreparedTurnCommit) -> GitSessionManifest {
    let commit = &prepared.commit;
    let lifecycle = commit.lifecycle.as_ref();
    GitSessionManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        session_id: commit.session_id.clone(),
        turn_id: derive_turn_id(prepared),
        parent_oids: commit.parent_ids.clone(),
        execution_status: commit.status.as_str().to_string(),
        disposition: commit.disposition.as_str().to_string(),
        model: commit.model.clone(),
        provider: commit.provider.clone(),
        agent_name: commit.agent_name.clone(),
        profile_snapshot_hash: commit.profile_snapshot_hash.clone(),
        baseline_oid: lifecycle.and_then(|lc| lc.baseline_oid.clone()),
        candidate_oid: lifecycle.and_then(|lc| lc.candidate_oid.clone()),
        candidate_ref: lifecycle.and_then(|lc| lc.candidate_ref.clone()),
        target_project_ref: lifecycle.map(|lc| lc.target_project_ref.clone()),
        snapshot_ref: lifecycle
            .and_then(|lc| lc.snapshot_ref.as_ref())
            .map(|s| serde_json::to_value(s).unwrap_or(serde_json::Value::Null)),
        verification: lifecycle
            .and_then(|lc| lc.verification.as_ref())
            .map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null)),
        resumes_turn_oid: lifecycle.and_then(|lc| lc.resumes_turn_oid.clone()),
        usage: commit
            .usage
            .as_ref()
            .map(|u| serde_json::to_value(u).unwrap_or(serde_json::Value::Null)),
        terminal_reason: commit.terminal_reason.clone(),
        workspace_history_oid: commit.workspace_history_oid.clone(),
        legacy_session_id: None,
        legacy_commit_id: None,
        commit_message: commit.commit_message.clone(),
        timestamp: commit.created_at.clone(),
    }
}

pub(super) fn turn_commit_from_manifest_and_oid(
    oid: gix::ObjectId,
    manifest: &GitSessionManifest,
) -> TurnCommit {
    let status =
        TurnCommitStatus::parse(&manifest.execution_status).unwrap_or(TurnCommitStatus::Completed);
    let disposition = WorkspaceDisposition::parse(&manifest.disposition)
        .unwrap_or(WorkspaceDisposition::NotApplicable);
    let usage = manifest
        .usage
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let lifecycle = build_lifecycle_from_manifest(manifest);

    TurnCommit {
        commit_id: oid.to_string(),
        session_id: manifest.session_id.clone(),
        parent_ids: manifest.parent_oids.clone(),
        message_ids: Vec::new(),
        agent_name: manifest.agent_name.clone(),
        profile_snapshot_hash: manifest.profile_snapshot_hash.clone(),
        profile_snapshot_json: String::new(),
        model: manifest.model.clone(),
        provider: manifest.provider.clone(),
        status,
        disposition,
        lifecycle,
        commit_message: manifest.commit_message.clone(),
        usage,
        terminal_reason: manifest.terminal_reason.clone(),
        workspace_history_oid: manifest.workspace_history_oid.clone(),
        created_at: manifest.timestamp.clone(),
    }
}

fn build_lifecycle_from_manifest(manifest: &GitSessionManifest) -> Option<TurnLifecycleEvidence> {
    let target_project_ref = manifest.target_project_ref.as_ref()?;
    let verification = manifest
        .verification
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let snapshot_ref = manifest
        .snapshot_ref
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    Some(TurnLifecycleEvidence {
        target_project_ref: target_project_ref.clone(),
        baseline_oid: manifest.baseline_oid.clone(),
        candidate_oid: manifest.candidate_oid.clone(),
        candidate_ref: manifest.candidate_ref.clone(),
        verification,
        snapshot_ref,
        resumes_turn_oid: manifest.resumes_turn_oid.clone(),
    })
}

pub(super) fn count_messages(jsonl: &str) -> (usize, usize) {
    let total = jsonl.lines().filter(|l| !l.is_empty()).count();
    let tools = jsonl
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
    (total, tools)
}
