// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! Commit creation, transcript reconstruction, and log.

use chrono::Utc;

use super::helpers::{
    GitSessionManifest, LATTICE_AUTHOR_EMAIL, LATTICE_AUTHOR_NAME, MANIFEST_SCHEMA_VERSION,
    build_root_tree, build_turn_tree, candidate_ref_name, derive_turn_id, parse_manifest,
    read_session_meta, read_tree_blob, serialize_manifest, validate_turn_id,
};
use super::session::resolve_head_oid;
use super::{DEFAULT_BRANCH, GitSessionGraphStore};
use crate::graph::{
    HeadTranscript, PreparedTurnCommit, TurnCommit, TurnCommitInput, TurnCommitStatus,
    TurnLifecycleEvidence, TurnLogEntry, WorkspaceDisposition,
};
use crate::{Result, SessionError};

impl GitSessionGraphStore {
    pub(super) fn commit_turn_impl(&self, input: TurnCommitInput) -> Result<TurnCommit> {
        let now = Utc::now().to_rfc3339();
        let prepared = crate::graph::prepare_turn_commit(&input, &now);
        self.commit_prepared_turn_impl(prepared)
    }

    pub(super) fn commit_prepared_turn_impl(
        &self,
        prepared: PreparedTurnCommit,
    ) -> Result<TurnCommit> {
        let commit = &prepared.commit;
        let session_id = &commit.session_id;

        let meta = read_session_meta(&self.dir, session_id)?;
        let branch = meta
            .head_branch
            .clone()
            .unwrap_or_else(|| DEFAULT_BRANCH.to_string());
        let repo = self.repo()?;

        // Resolve current HEAD OID for CAS check.
        let head_oid = resolve_head_oid(&repo, &meta, session_id)?;

        if commit.parent_ids.is_empty() {
            if head_oid.is_some() {
                return Err(SessionError::Conflict(
                    "expected no parent (first commit) but HEAD is not empty".to_string(),
                ));
            }
        } else {
            let expected_parent = &commit.parent_ids[0];
            let expected = git2::Oid::from_str(expected_parent)
                .map_err(|e| SessionError::Git(format!("parse expected parent: {e}")))?;
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
        let messages_jsonl = super::helpers::messages_to_jsonl(&prepared.messages)?;
        let agent_json = super::helpers::build_agent_json(&commit.agent_name);
        let profile_json = commit.profile_snapshot_json.clone();
        let audit_json = super::helpers::build_audit_json(&commit.lifecycle);

        let turn_tree_oid = build_turn_tree(
            &repo,
            &manifest_bytes,
            &messages_jsonl,
            &agent_json,
            &profile_json,
            &audit_json,
        )?;
        let root_tree_oid = build_root_tree(&repo, turn_tree_oid)?;
        let root_tree = repo
            .find_tree(root_tree_oid)
            .map_err(|e| SessionError::Git(format!("find root tree: {e}")))?;

        // Build signature from created_at timestamp.
        let ts = chrono::DateTime::parse_from_rfc3339(&commit.created_at)
            .map_err(|e| SessionError::Git(format!("parse commit timestamp: {e}")))?
            .timestamp();
        let time = git2::Time::new(ts, 0);
        let sig = git2::Signature::new(LATTICE_AUTHOR_NAME, LATTICE_AUTHOR_EMAIL, &time)
            .map_err(|e| SessionError::Git(format!("build signature: {e}")))?;

        // Resolve parents.
        let mut parents: Vec<git2::Commit> = Vec::new();
        for pid in &commit.parent_ids {
            let oid = git2::Oid::from_str(pid)
                .map_err(|e| SessionError::Git(format!("parse parent id: {e}")))?;
            let parent = repo
                .find_commit(oid)
                .map_err(|e| SessionError::Git(format!("find parent commit: {e}")))?;
            parents.push(parent);
        }
        let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

        let message = &commit.commit_message;
        let commit_oid = repo
            .commit(None, &sig, &sig, message, &root_tree, &parent_refs)
            .map_err(|e| SessionError::Git(format!("create commit: {e}")))?;

        // Update session branch ref atomically via compare-and-set.
        //
        // The earlier head_oid check is a non-atomic read; the actual ref
        // mutation must re-verify the current target before writing, otherwise
        // a concurrent process could move the ref between the check and the
        // force update, silently losing the other writer's commit.
        let ref_name = super::helpers::session_ref_name(session_id, &branch)?;
        match repo.find_reference(&ref_name) {
            Ok(mut existing) => {
                let current = existing.target().ok_or_else(|| {
                    SessionError::Git("session branch ref has no target".to_string())
                })?;
                if Some(current) != head_oid {
                    return Err(SessionError::Conflict(
                        "session branch ref moved during commit".to_string(),
                    ));
                }
                existing
                    .set_target(commit_oid, "lattice commit")
                    .map_err(|e| SessionError::Git(format!("update session branch ref: {e}")))?;
            }
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                // First commit on this branch: head_oid must have been None.
                if head_oid.is_some() {
                    return Err(SessionError::Conflict(
                        "session branch ref disappeared during commit".to_string(),
                    ));
                }
                repo.reference(&ref_name, commit_oid, false, "lattice commit")
                    .map_err(|e| SessionError::Git(format!("create session branch ref: {e}")))?;
            }
            Err(e) => return Err(SessionError::Git(format!("find session branch ref: {e}"))),
        }

        // Candidate ref creation (SC4): keep clean-turn commit ranges reachable.
        if let Some(ref lifecycle) = prepared.commit.lifecycle
            && lifecycle.candidate_oid.is_some()
        {
            let turn_id = &manifest.turn_id;
            validate_turn_id(turn_id)?;
            let cand_ref = candidate_ref_name(session_id, turn_id)?;
            match repo.reference(&cand_ref, commit_oid, false, "lattice candidate") {
                Ok(_) => {}
                Err(e) if e.code() == git2::ErrorCode::Exists => {
                    // Idempotent: candidate ref already pinned.
                }
                Err(e) => return Err(SessionError::Git(format!("create candidate ref: {e}"))),
            }
        }

        // Update meta head_commit_id.
        let mut updated_meta = meta;
        updated_meta.head_commit_id = Some(commit_oid.to_string());
        updated_meta.updated_at = Utc::now().to_rfc3339();
        super::helpers::write_session_meta(&self.dir, session_id, &updated_meta)?;

        let oid_str = commit_oid.to_string();
        let message_ids: Vec<String> = (0..prepared.messages.len())
            .map(|i| format!("{oid_str}:{i}"))
            .collect();

        let mut turn_commit = turn_commit_from_manifest_and_oid(commit_oid, &manifest);
        turn_commit.message_ids = message_ids;
        Ok(turn_commit)
    }

    pub(super) fn head_transcript_impl(&self, session_id: &str) -> Result<HeadTranscript> {
        let meta = read_session_meta(&self.dir, session_id)?;
        let repo = self.repo()?;
        let head_oid = match resolve_head_oid(&repo, &meta, session_id)? {
            Some(oid) => oid,
            None => {
                return Ok(HeadTranscript {
                    session_id: session_id.to_string(),
                    parent_commit_id: None,
                    messages: Vec::new(),
                });
            }
        };

        let mut revwalk = repo
            .revwalk()
            .map_err(|e| SessionError::Git(format!("create revwalk: {e}")))?;
        revwalk
            .push(head_oid)
            .map_err(|e| SessionError::Git(format!("push revwalk: {e}")))?;
        revwalk
            .set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)
            .map_err(|e| SessionError::Git(format!("set revwalk sorting: {e}")))?;

        let mut all_messages = Vec::new();
        for oid in revwalk {
            let oid = oid.map_err(|e| SessionError::Git(format!("revwalk error: {e}")))?;
            let commit = repo
                .find_commit(oid)
                .map_err(|e| SessionError::Git(format!("find commit: {e}")))?;
            let bytes = read_tree_blob(&repo, &commit, "turn/messages.jsonl")?;
            let jsonl = String::from_utf8(bytes)
                .map_err(|e| SessionError::Git(format!("messages.jsonl not utf8: {e}")))?;
            let messages = super::helpers::parse_messages_jsonl(&jsonl)?;
            all_messages.extend(messages);
        }

        Ok(HeadTranscript {
            session_id: session_id.to_string(),
            parent_commit_id: Some(head_oid.to_string()),
            messages: all_messages,
        })
    }

    pub(super) fn log_impl(&self, session_id: &str) -> Result<Vec<TurnLogEntry>> {
        let meta = read_session_meta(&self.dir, session_id)?;
        let repo = self.repo()?;
        let head_oid = match resolve_head_oid(&repo, &meta, session_id)? {
            Some(oid) => oid,
            None => return Ok(Vec::new()),
        };

        let mut revwalk = repo
            .revwalk()
            .map_err(|e| SessionError::Git(format!("create revwalk: {e}")))?;
        revwalk
            .push(head_oid)
            .map_err(|e| SessionError::Git(format!("push revwalk: {e}")))?;
        revwalk
            .set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)
            .map_err(|e| SessionError::Git(format!("set revwalk sorting: {e}")))?;

        let mut entries = Vec::new();
        for oid in revwalk {
            let oid = oid.map_err(|e| SessionError::Git(format!("revwalk error: {e}")))?;
            let commit = repo
                .find_commit(oid)
                .map_err(|e| SessionError::Git(format!("find commit: {e}")))?;
            let manifest_bytes = read_tree_blob(&repo, &commit, "turn/manifest.json")?;
            let manifest = parse_manifest(&manifest_bytes)?;
            let messages_bytes = read_tree_blob(&repo, &commit, "turn/messages.jsonl")?;
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
        let repo = self.repo()?;
        let oid = match git2::Oid::from_str(commit_id) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
            Err(e) => return Err(SessionError::Git(format!("find commit: {e}"))),
        };
        let manifest_bytes = read_tree_blob(&repo, &commit, "turn/manifest.json")?;
        let manifest = parse_manifest(&manifest_bytes)?;
        // Enforce session ownership: a commit from session A must not be
        // returned when queried with session B's ID.
        if manifest.session_id != session_id {
            return Ok(None);
        }
        let messages_bytes = read_tree_blob(&repo, &commit, "turn/messages.jsonl")?;
        let messages_jsonl = String::from_utf8(messages_bytes)
            .map_err(|e| SessionError::Git(format!("messages.jsonl not utf8: {e}")))?;
        let messages = super::helpers::parse_messages_jsonl(&messages_jsonl)?;
        let message_ids: Vec<String> = (0..messages.len())
            .map(|i| format!("{commit_id}:{i}"))
            .collect();

        let mut turn = turn_commit_from_manifest_and_oid(oid, &manifest);
        // Read the full profile snapshot JSON from the commit tree so that
        // consumers relying on `profile_snapshot_json` (replay, audit) get the
        // real payload instead of an empty string.
        let profile_bytes = read_tree_blob(&repo, &commit, "turn/profile.json")?;
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
        legacy_session_id: None,
        legacy_commit_id: None,
        commit_message: commit.commit_message.clone(),
        timestamp: commit.created_at.clone(),
    }
}

pub(super) fn turn_commit_from_manifest_and_oid(
    oid: git2::Oid,
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
