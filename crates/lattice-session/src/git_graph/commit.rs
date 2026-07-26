// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! Commit creation, transcript reconstruction, and log.

use chrono::Utc;
use gix::refs::transaction::PreviousValue;

use super::helpers::{
    GitSessionManifest, MANIFEST_SCHEMA_VERSION, build_root_tree, build_turn_tree,
    candidate_ref_name, derive_turn_id, lattice_signature, parse_manifest, parse_oid,
    read_session_meta, read_tree_blob, serialize_manifest, validate_turn_id,
    walk_commits_oldest_first,
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

        // Build signature from created_at timestamp.
        let ts = chrono::DateTime::parse_from_rfc3339(&commit.created_at)
            .map_err(|e| SessionError::Git(format!("parse commit timestamp: {e}")))?
            .timestamp();
        let sig = lattice_signature(ts);
        let mut time_buf = gix::date::parse::TimeBuf::default();
        let sig_ref = sig.to_ref(&mut time_buf);

        let parent_oids: Result<Vec<gix::ObjectId>> =
            commit.parent_ids.iter().map(|pid| parse_oid(pid)).collect();
        let parent_oids = parent_oids?;

        // Write commit without updating any ref; CAS is applied next.
        let commit_obj = repo
            .new_commit_as(
                sig_ref,
                sig_ref,
                &commit.commit_message,
                root_tree_oid,
                parent_oids.iter().copied(),
            )
            .map_err(|e| SessionError::Git(format!("create commit: {e}")))?;
        let commit_oid = commit_obj.id;

        // Update session branch ref atomically via compare-and-set.
        //
        // The earlier head_oid check is a non-atomic read; the actual ref
        // mutation must re-verify the current target before writing, otherwise
        // a concurrent process could move the ref between the check and the
        // force update, silently losing the other writer's commit.
        let ref_name = super::helpers::session_ref_name(session_id, &branch)?;
        let constraint = match head_oid {
            Some(expected) => PreviousValue::MustExistAndMatch(gix::refs::Target::Object(expected)),
            None => PreviousValue::MustNotExist,
        };
        match repo.reference(ref_name.as_str(), commit_oid, constraint, "lattice commit") {
            Ok(_) => {}
            Err(e) => {
                // Map constraint failures to Conflict so concurrent writers
                // surface a clean CAS error rather than a raw git error.
                let msg = e.to_string();
                if msg.contains("exist") || msg.contains("match") || msg.contains("constraint") {
                    return Err(SessionError::Conflict(
                        "session branch ref moved during commit".to_string(),
                    ));
                }
                return Err(SessionError::Git(format!("update session branch ref: {e}")));
            }
        }

        // Candidate ref creation (SC4): keep clean-turn commit ranges reachable.
        if let Some(ref lifecycle) = prepared.commit.lifecycle
            && lifecycle.candidate_oid.is_some()
        {
            let turn_id = &manifest.turn_id;
            validate_turn_id(turn_id)?;
            let cand_ref = candidate_ref_name(session_id, turn_id)?;
            match repo.reference(
                cand_ref.as_str(),
                commit_oid,
                PreviousValue::MustNotExist,
                "lattice candidate",
            ) {
                Ok(_) => {}
                Err(e) => {
                    // Idempotent: candidate ref already pinned.
                    let msg = e.to_string();
                    if !(msg.contains("exist") || msg.contains("MustNotExist")) {
                        return Err(SessionError::Git(format!("create candidate ref: {e}")));
                    }
                }
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

        let oids = walk_commits_oldest_first(&repo, head_oid, None)?;
        let mut all_messages = Vec::new();
        for oid in oids {
            let bytes = read_tree_blob(&repo, oid, "turn/messages.jsonl")?;
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

        let oids = walk_commits_oldest_first(&repo, head_oid, None)?;
        let mut entries = Vec::new();
        for oid in oids {
            let manifest_bytes = read_tree_blob(&repo, oid, "turn/manifest.json")?;
            let manifest = parse_manifest(&manifest_bytes)?;
            let messages_bytes = read_tree_blob(&repo, oid, "turn/messages.jsonl")?;
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
        let oid = match parse_oid(commit_id) {
            Ok(o) => o,
            Err(_) => return Ok(None),
        };
        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        // Confirm it is a real object of commit kind (find_commit already peels).
        let _ = commit;
        let manifest_bytes = match read_tree_blob(&repo, oid, "turn/manifest.json") {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let manifest = parse_manifest(&manifest_bytes)?;
        // Enforce session ownership: a commit from session A must not be
        // returned when queried with session B's ID.
        if manifest.session_id != session_id {
            return Ok(None);
        }
        let messages_bytes = read_tree_blob(&repo, oid, "turn/messages.jsonl")?;
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
        let profile_bytes = read_tree_blob(&repo, oid, "turn/profile.json")?;
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
