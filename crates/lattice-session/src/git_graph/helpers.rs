// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers for the Git-backed session graph store.
//!
//! DTOs for sidecar files (`meta.json`, `ops/<oid>.json`), ref name
//! construction/validation, Git tree building/reading, and manifest serialization.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::graph::{
    canonical_json_bytes, GraphMessage, PreparedTurnCommit, TurnLifecycleEvidence, TurnLogEntry,
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

/// Operational state stored in `.lattice/sessions/<id>/ops/<oid>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct OperationalState {
    pub snapshot_lost: bool,
    pub updated_at: String,
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
    // Legacy-ID traceability: populated only by SQLite→Git migration.
    // Native commits leave these as `None`; `skip_serializing_if` omits them
    // from native manifest JSON, so native commit OIDs are unchanged.
    // `deny_unknown_fields` rejects unknown JSON keys during deserialization,
    // but new Rust fields with `#[serde(default)]` deserialize to `None` when
    // absent from old JSON, so this addition is forward-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_commit_id: Option<String>,
    pub commit_message: String,
    pub timestamp: String,
}

pub(crate) fn session_meta_path(dir: &Path, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    Ok(dir.join(".lattice").join("sessions").join(session_id).join("meta.json"))
}

pub(crate) fn session_ops_dir(dir: &Path, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    Ok(dir.join(".lattice").join("sessions").join(session_id).join("ops"))
}

pub(crate) fn session_ops_path(dir: &Path, session_id: &str, commit_oid: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    // Defense-in-depth: commit_oid is used to build a filesystem path. Reject
    // anything that is not a plausible hex OID so `/` or `..` cannot escape the
    // ops directory.
    if commit_oid.is_empty() || !commit_oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(SessionError::Git(format!("invalid commit oid: {commit_oid}")));
    }
    Ok(session_ops_dir(dir, session_id)?.join(format!("{commit_oid}.json")))
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

pub(crate) fn read_ops_state(
    dir: &Path,
    session_id: &str,
    commit_oid: &str,
) -> Result<Option<OperationalState>> {
    let path = session_ops_path(dir, session_id, commit_oid)?;
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| SessionError::InvalidPersistedValue {
                field: "ops state",
                value: e.to_string(),
            }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SessionError::Io {
            operation: "reading ops state",
            path,
            source,
        }),
    }
}

pub(crate) fn write_ops_state(
    dir: &Path,
    session_id: &str,
    commit_oid: &str,
    state: &OperationalState,
) -> Result<()> {
    let path = session_ops_path(dir, session_id, commit_oid)?;
    crate::create_dir_all(path.parent().expect("ops path has parent"))?;
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|e| SessionError::Git(format!("serialize ops state: {e}")))?;
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

/// Build the `turn/agent.json` blob. Single source of truth shared by
/// the native commit path and the migration writer so the tree's agent entry
/// stays byte-identical (and OIDs stay consistent) between both.
pub(crate) fn build_agent_json(agent_name: &str) -> String {
    serde_json::json!({ "agent_name": agent_name }).to_string()
}

/// Build the `turn/audit.json` blob carrying lifecycle ref pointers. Single
/// source of truth shared by the native commit path and the migration
/// writer. The format must stay identical between both so migrated and natively
/// created commits produce consistent tree content.
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

pub(crate) fn build_turn_tree(
    repo: &git2::Repository,
    manifest_bytes: &[u8],
    messages_jsonl: &str,
    agent_json: &str,
    profile_json: &str,
    audit_json: &str,
) -> Result<git2::Oid> {
    let manifest_blob = write_blob(repo, manifest_bytes, "manifest.json")?;
    let messages_blob = write_blob(repo, messages_jsonl.as_bytes(), "messages.jsonl")?;
    let agent_blob = write_blob(repo, agent_json.as_bytes(), "agent.json")?;
    let profile_blob = write_blob(repo, profile_json.as_bytes(), "profile.json")?;
    let audit_blob = write_blob(repo, audit_json.as_bytes(), "audit.json")?;

    let mut turn_builder = repo
        .treebuilder(None)
        .map_err(|e| SessionError::Git(format!("create turn treebuilder: {e}")))?;
    turn_builder
        .insert("manifest.json", manifest_blob, 0o100644)
        .map_err(|e| SessionError::Git(format!("insert manifest.json: {e}")))?;
    turn_builder
        .insert("messages.jsonl", messages_blob, 0o100644)
        .map_err(|e| SessionError::Git(format!("insert messages.jsonl: {e}")))?;
    turn_builder
        .insert("agent.json", agent_blob, 0o100644)
        .map_err(|e| SessionError::Git(format!("insert agent.json: {e}")))?;
    turn_builder
        .insert("profile.json", profile_blob, 0o100644)
        .map_err(|e| SessionError::Git(format!("insert profile.json: {e}")))?;
    turn_builder
        .insert("audit.json", audit_blob, 0o100644)
        .map_err(|e| SessionError::Git(format!("insert audit.json: {e}")))?;

    turn_builder
        .write()
        .map_err(|e| SessionError::Git(format!("write turn tree: {e}")))
}

fn write_blob(repo: &git2::Repository, bytes: &[u8], context: &str) -> Result<git2::Oid> {
    repo.blob(bytes)
        .map_err(|e| SessionError::Git(format!("write {context} blob: {e}")))
}

pub(crate) fn build_root_tree(repo: &git2::Repository, turn_tree_oid: git2::Oid) -> Result<git2::Oid> {
    let turn_tree = repo
        .find_tree(turn_tree_oid)
        .map_err(|e| SessionError::Git(format!("find turn tree: {e}")))?;
    let mut root_builder = repo
        .treebuilder(None)
        .map_err(|e| SessionError::Git(format!("create root treebuilder: {e}")))?;
    root_builder
        .insert("turn", turn_tree.id(), 0o040000)
        .map_err(|e| SessionError::Git(format!("insert turn/: {e}")))?;
    root_builder
        .write()
        .map_err(|e| SessionError::Git(format!("write root tree: {e}")))
}

pub(crate) fn read_tree_blob(repo: &git2::Repository, commit: &git2::Commit, path: &str) -> Result<Vec<u8>> {
    let tree = commit.tree().map_err(|e| SessionError::Git(format!("read commit tree: {e}")))?;
    let entry = tree
        .get_path(Path::new(path))
        .map_err(|e| SessionError::Git(format!("find tree path {path}: {e}")))?;
    let blob = entry
        .to_object(repo)
        .map_err(|e| SessionError::Git(format!("load blob object for {path}: {e}")))?
        .peel_to_blob()
        .map_err(|e| SessionError::Git(format!("peel {path} to blob: {e}")))?;
    Ok(blob.content().to_vec())
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
            serde_json::from_str(line).map_err(|e| {
                SessionError::Git(format!("parse messages.jsonl line {i}: {e}"))
            })
        })
        .collect()
}

pub(crate) fn count_reachable_commits(repo: &git2::Repository, oid: git2::Oid) -> Result<usize> {
    let mut revwalk = repo
        .revwalk()
        .map_err(|e| SessionError::Git(format!("create revwalk: {e}")))?;
    revwalk
        .push(oid)
        .map_err(|e| SessionError::Git(format!("push revwalk: {e}")))?;
    revwalk
        .set_sorting(git2::Sort::TOPOLOGICAL)
        .map_err(|e| SessionError::Git(format!("set revwalk sorting: {e}")))?;
    Ok(revwalk.count())
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
