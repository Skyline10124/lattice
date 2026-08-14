// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! Shared helpers for the Git-backed session graph store.
//!
//! Manifest serialization, turn tree building/reading, and message JSONL.
//! Session metadata sidecars (`.lattice/sessions/**`) were hard-cut in #143;
//! session facts are immutable `SessionState` commits (see `state.rs`).

use gix::objs::tree::EntryKind;
use serde::{Deserialize, Serialize};

use lattice_store::{StoreRepo, TreeSpec};

use crate::graph::{
    GraphMessage, PreparedTurnCommit, TurnLifecycleEvidence, TurnLogEntry, canonical_json_bytes,
};
use crate::{Result, SessionError};

pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 2;

/// Canonical manifest stored at `turn/manifest.json` in every session commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GitSessionManifest {
    pub schema_version: u32,
    pub session_id: String,
    pub turn_id: String,
    pub parent_oids: Vec<String>,
    pub execution_status: String,
    pub disposition: String,
    pub model: String,
    pub provider: String,
    pub agent_name: String,
    pub profile_snapshot_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_project_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_ref: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumes_turn_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    /// Workspace-history object id this Turn ended at (GitHub #63).
    ///
    /// Durable in the commit tree, so the Turn's LATTICE mutations stay
    /// resolvable after session reopen, branch movement, and process restart.
    /// Omitted when absent, keeping manifests for history-less turns
    /// byte-identical to pre-#63 ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_history_oid: Option<String>,
    // Legacy-ID traceability fields retained for forward-compatible reads of
    // historically migrated manifests. Native commits leave these as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_commit_id: Option<String>,
    pub commit_message: String,
    pub timestamp: String,
}

pub(crate) fn derive_turn_id(prepared: &PreparedTurnCommit) -> String {
    prepared.turn_seed.chars().take(16).collect()
}

/// Build the `turn/agent.json` blob.
pub(crate) fn build_agent_json(agent_name: &str) -> String {
    serde_json::json!({ "agent_name": agent_name }).to_string()
}

/// Build the `turn/audit.json` blob carrying lifecycle ref pointers.
pub(crate) fn build_audit_json(lifecycle: &Option<TurnLifecycleEvidence>) -> String {
    match lifecycle {
        Some(lc) => serde_json::json!({
            "target_project_ref": lc.target_project_ref,
            "baseline_oid": lc.baseline_oid,
            "candidate_oid": lc.candidate_oid,
            "candidate_ref": lc.candidate_ref,
        })
        .to_string(),
        None => "{}".to_string(),
    }
}

pub(crate) fn parse_oid(hex: &str) -> Result<gix::ObjectId> {
    gix::ObjectId::from_hex(hex.as_bytes())
        .map_err(|e| SessionError::Git(format!("parse object id '{hex}': {e}")))
}

pub(crate) fn build_turn_tree(
    store: &StoreRepo,
    manifest_bytes: &[u8],
    messages_jsonl: &str,
    agent_json: &str,
    profile_json: &str,
    audit_json: &str,
) -> Result<gix::ObjectId> {
    let manifest_blob = store.write_blob(manifest_bytes)?;
    let messages_blob = store.write_blob(messages_jsonl.as_bytes())?;
    let agent_blob = store.write_blob(agent_json.as_bytes())?;
    let profile_blob = store.write_blob(profile_json.as_bytes())?;
    let audit_blob = store.write_blob(audit_json.as_bytes())?;

    let mut spec = TreeSpec::new();
    spec.upsert("manifest.json", EntryKind::Blob, manifest_blob)
        .upsert("messages.jsonl", EntryKind::Blob, messages_blob)
        .upsert("agent.json", EntryKind::Blob, agent_blob)
        .upsert("profile.json", EntryKind::Blob, profile_blob)
        .upsert("audit.json", EntryKind::Blob, audit_blob);
    Ok(store.write_tree(&spec)?)
}

pub(crate) fn build_root_tree(
    store: &StoreRepo,
    turn_tree_oid: gix::ObjectId,
) -> Result<gix::ObjectId> {
    let mut spec = TreeSpec::new();
    spec.upsert("turn", EntryKind::Tree, turn_tree_oid);
    Ok(store.write_tree(&spec)?)
}

pub(crate) fn serialize_manifest(manifest: &GitSessionManifest) -> Result<Vec<u8>> {
    let value = serde_json::to_value(manifest)
        .map_err(|e| SessionError::Git(format!("convert manifest to JSON: {e}")))?;
    Ok(canonical_json_bytes(&value))
}

pub(crate) fn parse_manifest(bytes: &[u8]) -> Result<GitSessionManifest> {
    serde_json::from_slice(bytes)
        .map_err(|e| SessionError::Git(format!("parse manifest.json: {e}")))
}

pub(crate) fn messages_to_jsonl(messages: &[GraphMessage]) -> Result<String> {
    let mut lines = Vec::with_capacity(messages.len());
    for m in messages {
        let line = serde_json::to_string(m)
            .map_err(|e| SessionError::Git(format!("serialize message: {e}")))?;
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

pub(crate) fn parse_messages_jsonl(jsonl: &str) -> Result<Vec<GraphMessage>> {
    if jsonl.is_empty() {
        return Ok(Vec::new());
    }
    jsonl
        .lines()
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(line)
                .map_err(|e| SessionError::Git(format!("parse messages.jsonl line {i}: {e}")))
        })
        .collect()
}

/// Walk reachable commits oldest-first (topo parents-before-children).
pub(crate) fn walk_commits_oldest_first(
    store: &StoreRepo,
    tip: gix::ObjectId,
    hide: Option<gix::ObjectId>,
) -> Result<Vec<gix::ObjectId>> {
    let hide_vec: Vec<gix::ObjectId> = hide.into_iter().collect();
    let mut ids = store.rev_walk(&[tip], &hide_vec)?;
    ids.reverse();
    Ok(ids)
}

pub(crate) fn turn_log_entry_from_manifest(
    manifest: &GitSessionManifest,
    commit_oid: &str,
    message_count: usize,
    tool_message_count: usize,
) -> TurnLogEntry {
    let usage = manifest
        .usage
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    TurnLogEntry {
        commit_id: commit_oid.to_string(),
        parent_ids: manifest.parent_oids.clone(),
        status: crate::graph::TurnCommitStatus::parse(&manifest.execution_status)
            .unwrap_or(crate::graph::TurnCommitStatus::Completed),
        disposition: crate::graph::WorkspaceDisposition::parse(&manifest.disposition)
            .unwrap_or(crate::graph::WorkspaceDisposition::NotApplicable),
        agent_name: manifest.agent_name.clone(),
        profile_snapshot_hash: manifest.profile_snapshot_hash.clone(),
        model: manifest.model.clone(),
        provider: manifest.provider.clone(),
        commit_message: manifest.commit_message.clone(),
        message_count,
        tool_message_count,
        usage,
        terminal_reason: manifest.terminal_reason.clone(),
        created_at: manifest.timestamp.clone(),
    }
}
