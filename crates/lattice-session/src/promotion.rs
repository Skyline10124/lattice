// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Minimal Phase 35 promoted Session commit object builder.
//!
//! Creates Git commit objects with a canonical `turn/manifest.json` tree only.
//! Ref movement remains exclusively in the workspace-owned promotion transaction.

use crate::graph::canonical_json_bytes;
use crate::{Result, SessionError};

/// Schema version for the Phase 35 skeleton `turn/manifest.json`.
pub const PROMOTION_MANIFEST_SCHEMA_VERSION: u32 = 1;

const LATTICE_AUTHOR_NAME: &str = "LATTICE";
const LATTICE_AUTHOR_EMAIL: &str = "lattice@localhost";
const PROMOTED_DISPOSITION: &str = "promoted";

/// Canonical promoted-turn manifest stored at `turn/manifest.json`.
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
}

/// Input for creating a promoted Session commit object without moving refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedSessionCommitInput {
    pub session_id: String,
    pub turn_id: String,
    pub baseline_oid: String,
    pub candidate_oid: String,
    pub committed_at_unix_seconds: i64,
    pub parent_session_oid: Option<git2::Oid>,
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

    let parent_oid = input
        .parent_session_oid
        .map(|oid| oid.to_string());

    let manifest = PromotedSessionManifest {
        schema_version: PROMOTION_MANIFEST_SCHEMA_VERSION,
        session_id: input.session_id.clone(),
        turn_id: input.turn_id.clone(),
        baseline_oid: input.baseline_oid.clone(),
        candidate_oid: input.candidate_oid.clone(),
        disposition: PROMOTED_DISPOSITION.to_string(),
        committed_at_unix_seconds: input.committed_at_unix_seconds,
        parent_oid,
    };

    let manifest_bytes = manifest_json_bytes(&manifest)?;
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

    let message = format!(
        "lattice session: promoted turn {}",
        input.turn_id
    );

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

fn manifest_json_bytes(manifest: &PromotedSessionManifest) -> Result<Vec<u8>> {
    let parent_oid = match &manifest.parent_oid {
        Some(oid) => serde_json::Value::String(oid.clone()),
        None => serde_json::Value::Null,
    };
    let value = serde_json::json!({
        "baseline_oid": manifest.baseline_oid,
        "candidate_oid": manifest.candidate_oid,
        "committed_at_unix_seconds": manifest.committed_at_unix_seconds,
        "disposition": manifest.disposition,
        "parent_oid": parent_oid,
        "schema_version": manifest.schema_version,
        "session_id": manifest.session_id,
        "turn_id": manifest.turn_id,
    });
    Ok(canonical_json_bytes(&value))
}

fn validate_non_empty_id(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        Err(SessionError::Git(format!("{field} must be non-empty")))
    } else {
        Ok(())
    }
}

fn validate_git_oid(field: &str, oid: &str) -> Result<()> {
    if oid.len() == 40 && oid.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(SessionError::Git(format!(
            "{field} must be a 40-character hex Git OID, got '{oid}'"
        )))
    }
}
