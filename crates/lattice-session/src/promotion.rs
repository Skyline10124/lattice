// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Minimal promoted Session commit object builder.
//!
//! Creates Git commit objects with a canonical `turn/manifest.json` tree only.
//! Ref movement remains exclusively in the workspace-owned promotion transaction.
//!
//! Extends the manifest with execution status, disposition, target ref,
//! candidate ref, and structured verification evidence. Only the `promoted`
//! disposition produces a Git Session object; pending/rejected/aborted remain
//! SQLite-only.

use crate::graph::canonical_json_bytes;
use crate::graph::{TurnCommitStatus, TurnVerificationEvidence};
use crate::{Result, SessionError};

/// Schema version for the initial `turn/manifest.json`.
pub const PROMOTION_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Schema version for the lifecycle-aware `turn/manifest.json`.
pub const PROMOTION_MANIFEST_SCHEMA_VERSION_V2: u32 = 2;

const LATTICE_AUTHOR_NAME: &str = "LATTICE";
const LATTICE_AUTHOR_EMAIL: &str = "lattice@localhost";
const PROMOTED_DISPOSITION: &str = "promoted";

/// Canonical promoted-turn manifest stored at `turn/manifest.json`.
///
/// Adds execution status, target project ref, candidate ref, and
/// structured verification evidence to the initial manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedSessionManifest {
    pub schema_version: u32,
    pub session_id: String,
    pub turn_id: String,
    pub baseline_oid: String,
    pub candidate_oid: String,
    pub disposition: String,
    pub committed_at_unix_seconds: i64,
    pub parent_oid: Option<String>,
    /// Target project branch ref (e.g., `refs/heads/main`).
    pub target_project_ref: String,
    /// Candidate ref that was promoted.
    pub candidate_ref: Option<String>,
    /// Execution status string (e.g., `"completed"`).
    pub execution_status: String,
    /// Verification execution evidence, if available.
    pub verification: Option<TurnVerificationEvidence>,
}

/// Input for creating a promoted Session commit object without moving refs.
///
/// Extends this with lifecycle evidence fields that are carried into
/// the manifest JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedSessionCommitInput {
    pub session_id: String,
    pub turn_id: String,
    pub baseline_oid: String,
    pub candidate_oid: String,
    pub committed_at_unix_seconds: i64,
    pub parent_session_oid: Option<git2::Oid>,
    /// Target project branch ref.
    pub target_project_ref: String,
    /// Candidate ref that was promoted.
    pub candidate_ref: Option<String>,
    /// Execution outcome of the turn.
    pub execution_status: TurnCommitStatus,
    /// Verification execution evidence.
    pub verification: Option<TurnVerificationEvidence>,
}

/// Build the canonical `PromotedSessionManifest` from a promoted input.
pub fn build_promoted_manifest(input: &PromotedSessionCommitInput) -> PromotedSessionManifest {
    let parent_oid = input.parent_session_oid.map(|oid| oid.to_string());

    PromotedSessionManifest {
        schema_version: PROMOTION_MANIFEST_SCHEMA_VERSION_V2,
        session_id: input.session_id.clone(),
        turn_id: input.turn_id.clone(),
        baseline_oid: input.baseline_oid.clone(),
        candidate_oid: input.candidate_oid.clone(),
        disposition: PROMOTED_DISPOSITION.to_string(),
        committed_at_unix_seconds: input.committed_at_unix_seconds,
        parent_oid,
        target_project_ref: input.target_project_ref.clone(),
        candidate_ref: input.candidate_ref.clone(),
        execution_status: input.execution_status.as_str().to_string(),
        verification: input.verification.clone(),
    }
}

/// Serialize manifest to canonical JSON bytes (sorted keys, deterministic).
pub fn manifest_json_canonical_bytes(manifest: &PromotedSessionManifest) -> Result<Vec<u8>> {
    let parent_oid = match &manifest.parent_oid {
        Some(oid) => serde_json::Value::String(oid.clone()),
        None => serde_json::Value::Null,
    };

    let candidate_ref = match &manifest.candidate_ref {
        Some(r) => serde_json::Value::String(r.clone()),
        None => serde_json::Value::Null,
    };

    let verification = match &manifest.verification {
        Some(v) => serde_json::json!({
            "argv": v.argv,
            "candidate_oid": v.candidate_oid,
            "exit_code": v.exit_code,
            "expected_exit_code": v.expected_exit_code,
            "passed": v.passed,
            "profile_hash": v.profile_hash,
        }),
        None => serde_json::Value::Null,
    };

    let value = serde_json::json!({
        "baseline_oid": manifest.baseline_oid,
        "candidate_oid": manifest.candidate_oid,
        "candidate_ref": candidate_ref,
        "committed_at_unix_seconds": manifest.committed_at_unix_seconds,
        "disposition": manifest.disposition,
        "execution_status": manifest.execution_status,
        "parent_oid": parent_oid,
        "schema_version": manifest.schema_version,
        "session_id": manifest.session_id,
        "target_project_ref": manifest.target_project_ref,
        "turn_id": manifest.turn_id,
        "verification": verification,
    });
    Ok(canonical_json_bytes(&value))
}

/// Create a promoted Session commit object in `repo` without updating any ref.
pub fn create_promoted_session_commit(
    repo: &git2::Repository,
    input: &PromotedSessionCommitInput,
) -> Result<git2::Oid> {
    validate_non_empty_id("session_id", &input.session_id)?;
    validate_non_empty_id("turn_id", &input.turn_id)?;
    validate_git_oid("baseline_oid", &input.baseline_oid)?;
    validate_git_oid("candidate_oid", &input.candidate_oid)?;

    let manifest = build_promoted_manifest(input);

    let manifest_bytes = manifest_json_canonical_bytes(&manifest)?;
    let blob_oid = repo
        .blob(&manifest_bytes)
        .map_err(|e| SessionError::Git(format!("write manifest blob: {e}")))?;
    let blob = repo
        .find_blob(blob_oid)
        .map_err(|e| SessionError::Git(format!("find manifest blob: {e}")))?;

    let mut turn_builder = repo
        .treebuilder(None)
        .map_err(|e| SessionError::Git(format!("create turn treebuilder: {e}")))?;
    turn_builder
        .insert("manifest.json", blob.id(), 0o100644)
        .map_err(|e| SessionError::Git(format!("insert manifest.json: {e}")))?;
    let turn_tree_oid = turn_builder
        .write()
        .map_err(|e| SessionError::Git(format!("write turn tree: {e}")))?;
    let turn_tree = repo
        .find_tree(turn_tree_oid)
        .map_err(|e| SessionError::Git(format!("find turn tree: {e}")))?;

    let mut root_builder = repo
        .treebuilder(None)
        .map_err(|e| SessionError::Git(format!("create root treebuilder: {e}")))?;
    root_builder
        .insert("turn", turn_tree.id(), 0o040000)
        .map_err(|e| SessionError::Git(format!("insert turn/: {e}")))?;
    let root_tree_oid = root_builder
        .write()
        .map_err(|e| SessionError::Git(format!("write root tree: {e}")))?;
    let root_tree = repo
        .find_tree(root_tree_oid)
        .map_err(|e| SessionError::Git(format!("find root tree: {e}")))?;

    let time = git2::Time::new(input.committed_at_unix_seconds, 0);
    let sig = git2::Signature::new(LATTICE_AUTHOR_NAME, LATTICE_AUTHOR_EMAIL, &time)
        .map_err(|e| SessionError::Git(format!("build commit signature: {e}")))?;

    let parents: Vec<git2::Commit> = match input.parent_session_oid {
        Some(parent_oid) => {
            let parent = repo
                .find_commit(parent_oid)
                .map_err(|e| SessionError::Git(format!("find parent commit: {e}")))?;
            vec![parent]
        }
        None => Vec::new(),
    };

    let message = format!("lattice session: promoted turn {}", input.turn_id);

    repo.commit(
        None,
        &sig,
        &sig,
        &message,
        &root_tree,
        &parents
            .iter()
            .map(|commit| commit as &git2::Commit)
            .collect::<Vec<_>>(),
    )
    .map_err(|e| SessionError::Git(format!("create session commit object: {e}")))
}

fn validate_non_empty_id(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        Err(SessionError::Git(format!("{field} must be non-empty")))
    } else {
        Ok(())
    }
}

fn validate_git_oid(field: &str, oid: &str) -> Result<()> {
    // Accept both SHA-1 (40 hex) and SHA-256 (64 hex) Git OIDs. The
    // previous 40-char-only check silently rejected SHA-256 repositories.
    let len = oid.len();
    let is_hex = !oid.is_empty() && oid.chars().all(|c| c.is_ascii_hexdigit());
    if is_hex && (len == 40 || len == 64) {
        Ok(())
    } else {
        Err(SessionError::Git(format!(
            "{field} must be a 40- (SHA-1) or 64- (SHA-256) character hex Git OID, got '{oid}'"
        )))
    }
}
