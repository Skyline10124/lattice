// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Dedicated git2 migration writer (D-05, D-07, D-09, D-10).
//!
//! Creates Git commit objects on arbitrary idmap-remapped parents by calling
//! `repo.commit(None, ...)` directly — bypassing the CAS-enforcing prepared
//! commit path, which cannot express multi-branch or arbitrary-parent writes
//! (D-05). Reuses the proven Phase 38 tree helpers (`build_turn_tree`,
//! `build_root_tree`, `serialize_manifest`, `messages_to_jsonl`) and the
//! idempotent ref-creation pattern from `git_graph/commit.rs:165-175`.
//!
//! Write order (D-10): commit objects → refs → sidecars → idmap fsync.
//! No backend marker is written (D-13: no marker in the no-compat model).
//! The source is not archived here — `run_migration` archives only after
//! verification passes (D-18).

use std::path::Path;

use crate::graph::TurnLifecycleEvidence;
use crate::git_graph::helpers::{
    LATTICE_AUTHOR_EMAIL, LATTICE_AUTHOR_NAME, MANIFEST_SCHEMA_VERSION, GitSessionManifest,
    atomic_write, build_root_tree, build_turn_tree, candidate_ref_name, messages_to_jsonl,
    serialize_manifest, session_ref_name, write_ops_state, write_session_meta, OperationalState,
    SessionMetadata,
};
use crate::{Result, SessionError};

use super::extract::{ExtractedCommit, ExtractedRef, ExtractedSession, Idmap};

/// Stats from a write pass — surfaced in `MigrationResult`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct WriteStats {
    pub sessions_migrated: usize,
    pub commits_migrated: usize,
    pub refs_migrated: usize,
}

/// Run the full D-10 write order against `repo` for all extracted sessions.
///
/// `target_repo_path` is the bare-repo directory (used for sidecar paths and
/// the idmap checkpoint location at `<target_repo>/.lattice/migration/idmap.json`).
pub(crate) fn write_migration(
    repo: &git2::Repository,
    sessions: &[ExtractedSession],
    idmap: &mut Idmap,
    target_repo_path: &Path,
) -> Result<WriteStats> {
    let mut stats = WriteStats::default();

    // Step 1 — commit objects (D-05, D-06). Topological order is already
    // guaranteed by the extractor (parents before children), so writing each
    // session's commits in their extracted order is correct.
    for session in sessions {
        for commit in &session.commits {
            if idmap.get(&commit.commit_id).is_some() {
                // Restartability (D-09): already written. Skip.
                stats.commits_migrated += 1;
                continue;
            }
            let oid = write_commit_object(repo, session, commit, idmap)?;
            idmap.insert(commit.commit_id.clone(), oid.to_string());
            stats.commits_migrated += 1;
        }
        stats.sessions_migrated += 1;
    }

    // Step 2 — refs (D-07, D-09). After ALL commit objects so refs always
    // resolve (Pitfall #3).
    for session in sessions {
        write_session_refs(repo, session, idmap, &mut stats)?;
    }

    // Step 3 — sidecars (D-07): meta.json per session + ops/<oid>.json per
    // commit with snapshot_lost.
    for session in sessions {
        write_session_sidecars(repo, target_repo_path, session, idmap)?;
    }

    // Step 4 — idmap checkpoint (D-09). Atomic write so a crash mid-migration
    // leaves the previous checkpoint intact.
    idmap.save(target_repo_path)?;

    Ok(stats)
}

/// Create a single Git commit object on idmap-remapped parents (D-05, D-06).
fn write_commit_object(
    repo: &git2::Repository,
    session: &ExtractedSession,
    commit: &ExtractedCommit,
    idmap: &Idmap,
) -> Result<git2::Oid> {
    // Remap parents through the idmap (D-06). Topological order guarantees
    // every parent is already in the idmap.
    let parent_oids: Vec<String> = commit
        .parent_ids
        .iter()
        .map(|pid| {
            idmap.get(pid)
                .map(|s| s.to_string())
                .ok_or_else(|| SessionError::Git(format!(
                    "parent {pid} not in idmap for commit {} (topological order violated?)",
                    commit.commit_id
                )))
        })
        .collect::<Result<Vec<_>>>()?;

    // Decode lifecycle to extract resumes_turn_oid for remapping (Pitfall #6).
    let lifecycle = super::extract::decode_lifecycle_json(&commit.lifecycle_json)?;
    let resumes_turn_oid = lifecycle
        .as_ref()
        .and_then(|lc| lc.resumes_turn_oid.as_ref())
        .and_then(|legacy| idmap.get(legacy).map(|s| s.to_string()));

    // Turn ID: first 16 hex chars of the legacy commit_id (RESEARCH.md §Turn
    // ID Derivation). Migration is internally self-consistent; migrated
    // candidate ref names differ from native, accepted per CONTEXT.md.
    let turn_id: String = commit.commit_id.chars().take(16).collect();

    let manifest = GitSessionManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        session_id: session.session_id.clone(),
        turn_id: turn_id.clone(),
        parent_oids: parent_oids.clone(),
        execution_status: commit.status.clone(),
        disposition: commit.disposition.clone(),
        model: commit.model.clone(),
        provider: commit.provider.clone(),
        agent_name: commit.agent_name.clone(),
        profile_snapshot_hash: commit.profile_snapshot_hash.clone(),
        baseline_oid: lifecycle.as_ref().and_then(|lc| lc.baseline_oid.clone()),
        candidate_oid: lifecycle.as_ref().and_then(|lc| lc.candidate_oid.clone()),
        candidate_ref: lifecycle.as_ref().and_then(|lc| lc.candidate_ref.clone()),
        target_project_ref: lifecycle.as_ref().map(|lc| lc.target_project_ref.clone()),
        snapshot_ref: lifecycle
            .as_ref()
            .and_then(|lc| lc.snapshot_ref.as_ref())
            .map(|s| serde_json::to_value(s).unwrap_or(serde_json::Value::Null)),
        verification: lifecycle
            .as_ref()
            .and_then(|lc| lc.verification.as_ref())
            .map(|v| serde_json::to_value(v).unwrap_or(serde_json::Value::Null)),
        resumes_turn_oid,
        usage: commit
            .usage_json
            .as_ref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok()),
        terminal_reason: commit.terminal_reason.clone(),
        legacy_session_id: Some(session.session_id.clone()),
        legacy_commit_id: Some(commit.commit_id.clone()),
        commit_message: commit.commit_message.clone(),
        timestamp: commit.created_at.clone(),
    };

    let manifest_bytes = serialize_manifest(&manifest)?;
    let messages_jsonl = messages_to_jsonl(&commit.messages)?;
    let agent_json = serde_json::json!({ "agent_name": commit.agent_name }).to_string();
    let profile_json = commit.profile_snapshot_json.clone();
    let audit_json = build_audit_json(&lifecycle);

    let turn_tree_oid = build_turn_tree(
        repo,
        &manifest_bytes,
        &messages_jsonl,
        &agent_json,
        &profile_json,
        &audit_json,
    )?;
    let root_tree_oid = build_root_tree(repo, turn_tree_oid)?;
    let root_tree = repo
        .find_tree(root_tree_oid)
        .map_err(|e| SessionError::Git(format!("find root tree: {e}")))?;

    // Signature from the SQLite created_at timestamp — matches
    // git_graph/commit.rs:95-99 so OIDs are deterministic (D-09).
    let ts = chrono::DateTime::parse_from_rfc3339(&commit.created_at)
        .map_err(|e| SessionError::Git(format!("parse commit timestamp: {e}")))?
        .timestamp();
    let time = git2::Time::new(ts, 0);
    let sig = git2::Signature::new(LATTICE_AUTHOR_NAME, LATTICE_AUTHOR_EMAIL, &time)
        .map_err(|e| SessionError::Git(format!("build signature: {e}")))?;

    // Resolve remapped parent commits.
    let mut parents: Vec<git2::Commit> = Vec::new();
    for pid in &parent_oids {
        let oid = git2::Oid::from_str(pid)
            .map_err(|e| SessionError::Git(format!("parse parent id: {e}")))?;
        parents.push(
            repo.find_commit(oid)
                .map_err(|e| SessionError::Git(format!("find parent commit: {e}")))?,
        );
    }
    let parent_refs: Vec<&git2::Commit> = parents.iter().collect();

    // D-05: repo.commit(None, ...) — update_ref=None; refs written separately.
    let commit_oid = repo
        .commit(None, &sig, &sig, &commit.commit_message, &root_tree, &parent_refs)
        .map_err(|e| SessionError::Git(format!("create migrated commit: {e}")))?;
    Ok(commit_oid)
}

/// Write session refs and candidate refs idempotently (D-07, D-09).
fn write_session_refs(
    repo: &git2::Repository,
    session: &ExtractedSession,
    idmap: &Idmap,
    stats: &mut WriteStats,
) -> Result<()> {
    for r in &session.refs {
        write_branch_ref(repo, session.session_id.as_str(), r, idmap, stats)?;
    }

    // Candidate refs: for each commit whose lifecycle carried a candidate_oid,
    // pin a candidate ref at the migrated OID.
    for commit in &session.commits {
        let lifecycle = super::extract::decode_lifecycle_json(&commit.lifecycle_json)?;
        if lifecycle.as_ref().and_then(|lc| lc.candidate_oid.clone()).is_some() {
            let turn_id: String = commit.commit_id.chars().take(16).collect();
            let Some(new_oid_str) = idmap.get(&commit.commit_id) else {
                return Err(SessionError::Git(format!(
                    "commit {} not in idmap for candidate ref", commit.commit_id
                )));
            };
            let oid = git2::Oid::from_str(new_oid_str)
                .map_err(|e| SessionError::Git(format!("parse candidate oid: {e}")))?;
            let cand_ref = candidate_ref_name(&session.session_id, &turn_id)?;
            create_ref_idempotent(repo, &cand_ref, oid, "lattice migration candidate")?;
            stats.refs_migrated += 1;
        }
    }
    Ok(())
}

fn write_branch_ref(
    repo: &git2::Repository,
    session_id: &str,
    r: &ExtractedRef,
    idmap: &Idmap,
    stats: &mut WriteStats,
) -> Result<()> {
    let Some(legacy_cid) = r.commit_id.as_ref() else {
        // Branch with no commits yet — no ref to write.
        return Ok(());
    };
    let Some(new_oid_str) = idmap.get(legacy_cid) else {
        // The ref tip was not in the reachable closure (D-04 only migrates
        // reachable commits). Skip rather than fail — matches the
        // "unreachable commits are not migrated" semantics.
        return Ok(());
    };
    let oid = git2::Oid::from_str(new_oid_str)
        .map_err(|e| SessionError::Git(format!("parse ref oid: {e}")))?;
    let ref_name = session_ref_name(session_id, &r.name)?;
    create_ref_idempotent(repo, &ref_name, oid, "lattice migration")?;
    stats.refs_migrated += 1;
    Ok(())
}

/// Idempotent ref creation — `ErrorCode::Exists` is OK (D-09, Phase 38
/// candidate-ref pattern from commit.rs:165-175).
fn create_ref_idempotent(
    repo: &git2::Repository,
    name: &str,
    oid: git2::Oid,
    reason: &str,
) -> Result<()> {
    match repo.reference(name, oid, false, reason) {
        Ok(_) => Ok(()),
        Err(e) if e.code() == git2::ErrorCode::Exists => Ok(()),
        Err(e) => Err(SessionError::Git(format!("create ref {name}: {e}"))),
    }
}

/// Write per-session `meta.json` and per-commit `ops/<oid>.json` sidecars
/// (D-07).
fn write_session_sidecars(
    _repo: &git2::Repository,
    target_repo_path: &Path,
    session: &ExtractedSession,
    idmap: &Idmap,
) -> Result<()> {
    // meta.json with head_branch/head_commit_id from the SQLite session HEAD.
    let head_branch = session.head_ref.clone();
    let head_commit_id = session
        .head_commit_id
        .as_ref()
        .and_then(|legacy| idmap.get(legacy).map(|s| s.to_string()));

    let meta = SessionMetadata {
        session_id: session.session_id.clone(),
        label: session.label.clone(),
        active_agent_name: session.active_agent_name.clone(),
        project_target_branch: session.project_target_branch.clone(),
        created_at: session.created_at.clone(),
        updated_at: session.updated_at.clone(),
        head_branch,
        head_commit_id,
    };
    write_session_meta(target_repo_path, &session.session_id, &meta)?;

    // ops/<oid>.json for commits with snapshot_lost.
    for commit in &session.commits {
        if commit.snapshot_lost {
            let Some(new_oid_str) = idmap.get(&commit.commit_id) else {
                return Err(SessionError::Git(format!(
                    "commit {} not in idmap for ops sidecar", commit.commit_id
                )));
            };
            let state = OperationalState {
                snapshot_lost: true,
                updated_at: session.updated_at.clone(),
            };
            write_ops_state(target_repo_path, &session.session_id, new_oid_str, &state)?;
        }
    }
    Ok(())
}

/// Build the audit.json blob — mirrors `git_graph/commit.rs:build_audit_json`
/// (private to that module). The audit blob carries lifecycle ref pointers
/// for the tree's `turn/audit.json` entry.
fn build_audit_json(lifecycle: &Option<TurnLifecycleEvidence>) -> String {
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

// `atomic_write` is re-exported above for the idmap checkpoint path; keep the
// import live even if the direct call in this module is only via `idmap.save`.
#[allow(unused_imports)]
use atomic_write as _atomic_write;
