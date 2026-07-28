// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
//! Shared helpers for the Git-backed session graph store.
//!
//! DTOs for sidecar files (`meta.json`), ref name construction/validation,
//! Git tree building/reading, and manifest serialization.
use std::fs;
use std::path::{Path, PathBuf};

use gix::objs::tree::EntryKind;
use serde::{Deserialize, Serialize};

use crate::graph::{
    GraphMessage, PreparedTurnCommit, TurnLifecycleEvidence, TurnLogEntry, canonical_json_bytes,
};
use crate::{Result, SessionError};

pub(crate) const LATTICE_AUTHOR_NAME: &str = "LATTICE";
pub(crate) const LATTICE_AUTHOR_EMAIL: &str = "lattice@localhost";
pub(crate) const MANIFEST_SCHEMA_VERSION: u32 = 2;

/// Mutable session metadata stored in `.lattice/sessions/<id>/meta.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SessionMetadata {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub active_agent_name: String,
    pub project_target_branch: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit_id: Option<String>,
}

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

pub(crate) fn session_meta_path(dir: &Path, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    Ok(dir
        .join(".lattice")
        .join("sessions")
        .join(session_id)
        .join("meta.json"))
}

pub(crate) fn read_session_meta(dir: &Path, session_id: &str) -> Result<SessionMetadata> {
    let path = session_meta_path(dir, session_id)?;
    let bytes = fs::read(&path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            SessionError::NotFound(session_id.to_string())
        } else {
            SessionError::Io {
                operation: "reading session meta",
                path: path.clone(),
                source,
            }
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|e| SessionError::InvalidPersistedValue {
        field: "meta.json",
        value: e.to_string(),
    })
}

pub(crate) fn write_session_meta(
    dir: &Path,
    session_id: &str,
    meta: &SessionMetadata,
) -> Result<()> {
    let path = session_meta_path(dir, session_id)?;
    crate::create_dir_all(path.parent().expect("meta.json path has parent"))?;
    let bytes = serde_json::to_vec_pretty(meta)
        .map_err(|e| SessionError::Git(format!("serialize meta.json: {e}")))?;
    atomic_write(&path, &bytes)
}

pub(crate) fn validate_session_id(session_id: &str) -> Result<()> {
    validate_ref_component(session_id, "session_id")
}

pub(crate) fn validate_turn_id(turn_id: &str) -> Result<()> {
    validate_ref_component(turn_id, "turn_id")
}

fn validate_ref_component(value: &str, field: &str) -> Result<()> {
    if value.is_empty() {
        return Err(SessionError::Git(format!("{field} must not be empty")));
    }
    if value.len() > 128 {
        return Err(SessionError::Git(format!("{field} exceeds 128 characters")));
    }
    let mut chars = value.chars();
    let first = chars.next().expect("non-empty");
    if !first.is_ascii_alphanumeric() {
        return Err(SessionError::Git(format!(
            "{field} must start with alphanumeric character"
        )));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err(SessionError::Git(format!(
            "{field} must contain only alphanumeric, '.', '_', or '-'"
        )));
    }
    Ok(())
}

pub(crate) fn derive_turn_id(prepared: &PreparedTurnCommit) -> String {
    prepared.turn_seed.chars().take(16).collect()
}

pub(crate) fn session_ref_name(session_id: &str, branch: &str) -> Result<String> {
    validate_session_id(session_id)?;
    crate::validate_branch_name(branch)?;
    Ok(format!("refs/lattice/sessions/{session_id}/{branch}"))
}

pub(crate) fn candidate_ref_name(session_id: &str, turn_id: &str) -> Result<String> {
    validate_session_id(session_id)?;
    validate_turn_id(turn_id)?;
    Ok(format!("refs/lattice/candidates/{session_id}/{turn_id}"))
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
    repo: &gix::Repository,
    manifest_bytes: &[u8],
    messages_jsonl: &str,
    agent_json: &str,
    profile_json: &str,
    audit_json: &str,
) -> Result<gix::ObjectId> {
    let manifest_blob = write_blob(repo, manifest_bytes, "manifest.json")?;
    let messages_blob = write_blob(repo, messages_jsonl.as_bytes(), "messages.jsonl")?;
    let agent_blob = write_blob(repo, agent_json.as_bytes(), "agent.json")?;
    let profile_blob = write_blob(repo, profile_json.as_bytes(), "profile.json")?;
    let audit_blob = write_blob(repo, audit_json.as_bytes(), "audit.json")?;

    let empty = gix::ObjectId::empty_tree(repo.object_hash());
    let mut editor = repo
        .edit_tree(empty)
        .map_err(|e| SessionError::Git(format!("create turn tree editor: {e}")))?;
    editor
        .upsert("manifest.json", EntryKind::Blob, manifest_blob)
        .map_err(|e| SessionError::Git(format!("insert manifest.json: {e}")))?;
    editor
        .upsert("messages.jsonl", EntryKind::Blob, messages_blob)
        .map_err(|e| SessionError::Git(format!("insert messages.jsonl: {e}")))?;
    editor
        .upsert("agent.json", EntryKind::Blob, agent_blob)
        .map_err(|e| SessionError::Git(format!("insert agent.json: {e}")))?;
    editor
        .upsert("profile.json", EntryKind::Blob, profile_blob)
        .map_err(|e| SessionError::Git(format!("insert profile.json: {e}")))?;
    editor
        .upsert("audit.json", EntryKind::Blob, audit_blob)
        .map_err(|e| SessionError::Git(format!("insert audit.json: {e}")))?;
    editor
        .write()
        .map(|id| id.detach())
        .map_err(|e| SessionError::Git(format!("write turn tree: {e}")))
}

fn write_blob(repo: &gix::Repository, bytes: &[u8], context: &str) -> Result<gix::ObjectId> {
    repo.write_blob(bytes)
        .map(|id| id.detach())
        .map_err(|e| SessionError::Git(format!("write {context} blob: {e}")))
}

pub(crate) fn build_root_tree(
    repo: &gix::Repository,
    turn_tree_oid: gix::ObjectId,
) -> Result<gix::ObjectId> {
    let empty = gix::ObjectId::empty_tree(repo.object_hash());
    let mut editor = repo
        .edit_tree(empty)
        .map_err(|e| SessionError::Git(format!("create root tree editor: {e}")))?;
    editor
        .upsert("turn", EntryKind::Tree, turn_tree_oid)
        .map_err(|e| SessionError::Git(format!("insert turn/: {e}")))?;
    editor
        .write()
        .map(|id| id.detach())
        .map_err(|e| SessionError::Git(format!("write root tree: {e}")))
}

pub(crate) fn read_tree_blob(
    repo: &gix::Repository,
    commit_id: gix::ObjectId,
    path: &str,
) -> Result<Vec<u8>> {
    let commit = repo
        .find_commit(commit_id)
        .map_err(|e| SessionError::Git(format!("find commit for tree read: {e}")))?;
    let tree = commit
        .tree()
        .map_err(|e| SessionError::Git(format!("read commit tree: {e}")))?;
    let entry = tree
        .lookup_entry_by_path(path)
        .map_err(|e| SessionError::Git(format!("lookup tree path {path}: {e}")))?
        .ok_or_else(|| SessionError::Git(format!("find tree path {path}: not found")))?;
    let object = entry
        .object()
        .map_err(|e| SessionError::Git(format!("load blob object for {path}: {e}")))?;
    let blob = object
        .try_into_blob()
        .map_err(|e| SessionError::Git(format!("peel {path} to blob: {e}")))?;
    Ok(blob.data.clone())
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

pub(crate) fn count_reachable_commits(repo: &gix::Repository, oid: gix::ObjectId) -> Result<usize> {
    let walk = repo
        .rev_walk([oid])
        .all()
        .map_err(|e| SessionError::Git(format!("create revwalk: {e}")))?;
    let mut count = 0usize;
    for item in walk {
        item.map_err(|e| SessionError::Git(format!("revwalk error: {e}")))?;
        count += 1;
    }
    Ok(count)
}

/// Walk reachable commits oldest-first (topo parents-before-children).
pub(crate) fn walk_commits_oldest_first(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    hide: Option<gix::ObjectId>,
) -> Result<Vec<gix::ObjectId>> {
    let mut platform = repo.rev_walk([tip]);
    if let Some(hidden) = hide {
        platform = platform.with_hidden(Some(hidden));
    }
    let walk = platform
        .all()
        .map_err(|e| SessionError::Git(format!("create revwalk: {e}")))?;
    let mut ids = Vec::new();
    for item in walk {
        let info = item.map_err(|e| SessionError::Git(format!("revwalk error: {e}")))?;
        ids.push(info.id);
    }
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

pub(crate) fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    // Use a unique temp file name to avoid collisions when two writers target
    // the same path concurrently. Suffix with PID + a monotonic counter
    // to keep it deterministic per-process while still unique across processes.
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().expect("atomic_write path has parent");
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "data".to_string());
    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{file_name}.{pid}.{seq}.tmp"));
    fs::write(&tmp, content).map_err(|source| SessionError::Io {
        operation: "writing temporary file",
        path: tmp.clone(),
        source,
    })?;
    fs::rename(&tmp, path).map_err(|source| SessionError::Io {
        operation: "renaming temporary file",
        path: path.to_path_buf(),
        source,
    })
}

/// Resolve a full ref name to its direct object target, if the ref exists.
pub(crate) fn try_ref_target(
    repo: &gix::Repository,
    ref_name: &str,
) -> Result<Option<gix::ObjectId>> {
    match repo.try_find_reference(ref_name) {
        Ok(Some(r)) => Ok(r.target().try_id().map(|id| id.to_owned())),
        Ok(None) => Ok(None),
        Err(e) => Err(SessionError::Git(format!("find reference {ref_name}: {e}"))),
    }
}

/// Build a lattice author/committer signature at the given unix timestamp.
pub(crate) fn lattice_signature(ts: i64) -> gix::actor::Signature {
    gix::actor::Signature {
        name: LATTICE_AUTHOR_NAME.into(),
        email: LATTICE_AUTHOR_EMAIL.into(),
        time: gix::date::Time::new(ts, 0),
    }
}
