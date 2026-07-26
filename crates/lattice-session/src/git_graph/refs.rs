// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Ref operations for the Git-backed session graph store.

use chrono::Utc;
use gix::refs::transaction::PreviousValue;

use super::GitSessionGraphStore;
use super::helpers::{
    parse_manifest, parse_oid, read_session_meta, read_tree_blob, session_ref_name, try_ref_target,
    turn_log_entry_from_manifest, validate_session_id, write_session_meta,
};
use super::session::resolve_head_oid;
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
        validate_session_id(session_id)?;
        let meta = read_session_meta(&self.dir, session_id)?;
        let repo = self.repo()?;

        let target_oid = match from {
            Some(target) => resolve_target_to_oid(&repo, session_id, target)?,
            None => resolve_head_oid(&repo, &meta, session_id)?.ok_or_else(|| {
                SessionError::Conflict("cannot create branch: no commits on HEAD".to_string())
            })?,
        };

        let ref_name = session_ref_name(session_id, name)?;
        if try_ref_target(&repo, &ref_name)?.is_some() {
            return Err(SessionError::Conflict(format!(
                "branch '{name}' already exists"
            )));
        }
        repo.reference(
            ref_name.as_str(),
            target_oid,
            PreviousValue::MustNotExist,
            "lattice branch",
        )
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("exist") || msg.contains("MustNotExist") {
                SessionError::Conflict(format!("branch '{name}' already exists"))
            } else {
                SessionError::Git(format!("create branch ref: {e}"))
            }
        })?;

        let updated_at = Utc::now().to_rfc3339();
        Ok(GraphRef {
            session_id: session_id.to_string(),
            name: name.to_string(),
            kind: GraphRefKind::Branch,
            commit_id: Some(target_oid.to_string()),
            active_agent_name: meta.active_agent_name,
            updated_at,
        })
    }

    pub(super) fn checkout_impl(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        validate_session_id(session_id)?;
        let mut meta = read_session_meta(&self.dir, session_id)?;
        let repo = self.repo()?;

        // Try as branch first.
        if let Ok(ref_name) = session_ref_name(session_id, target)
            && let Some(oid) = try_ref_target(&repo, &ref_name)?
        {
            meta.head_branch = Some(target.to_string());
            meta.head_commit_id = Some(oid.to_string());
            meta.updated_at = Utc::now().to_rfc3339();
            write_session_meta(&self.dir, session_id, &meta)?;
            return Ok(GraphHead::Branch {
                name: target.to_string(),
            });
        }

        // Try as commit OID.
        let oid = parse_oid(target).map_err(|_| SessionError::MissingCommit(target.to_string()))?;
        if repo.find_commit(oid).is_err() {
            return Err(SessionError::MissingCommit(target.to_string()));
        }
        meta.head_branch = None;
        meta.head_commit_id = Some(target.to_string());
        meta.updated_at = Utc::now().to_rfc3339();
        write_session_meta(&self.dir, session_id, &meta)?;
        Ok(GraphHead::Detached {
            commit_id: target.to_string(),
        })
    }

    pub(super) fn reset_impl(&self, session_id: &str, target: &str) -> Result<ResetResult> {
        validate_session_id(session_id)?;
        let mut meta = read_session_meta(&self.dir, session_id)?;
        let branch_name = meta.head_branch.clone().ok_or_else(|| {
            SessionError::Conflict(
                "cannot reset while HEAD is detached; checkout a branch first".to_string(),
            )
        })?;
        let repo = self.repo()?;

        let target_oid =
            parse_oid(target).map_err(|e| SessionError::Git(format!("parse reset target: {e}")))?;
        // Verify the target exists as a commit.
        let _ = repo
            .find_commit(target_oid)
            .map_err(|_| SessionError::MissingCommit(target.to_string()))?;

        let ref_name = session_ref_name(session_id, &branch_name)?;
        let old_oid = try_ref_target(&repo, &ref_name)?
            .ok_or_else(|| SessionError::Git("branch ref has no target".to_string()))?;

        repo.reference(
            ref_name.as_str(),
            target_oid,
            PreviousValue::MustExistAndMatch(gix::refs::Target::Object(old_oid)),
            "lattice reset",
        )
        .map_err(|e| SessionError::Git(format!("set branch target: {e}")))?;

        meta.head_commit_id = Some(target.to_string());
        meta.updated_at = Utc::now().to_rfc3339();
        write_session_meta(&self.dir, session_id, &meta)?;

        Ok(ResetResult {
            session_id: session_id.to_string(),
            branch: branch_name,
            old_commit_id: Some(old_oid.to_string()),
            new_commit_id: target.to_string(),
        })
    }

    pub(super) fn diff_impl(&self, session_id: &str, from: &str, to: &str) -> Result<GraphDiff> {
        validate_session_id(session_id)?;
        let repo = self.repo()?;
        let from_oid = resolve_target_to_oid(&repo, session_id, from)?;
        let to_oid = resolve_target_to_oid(&repo, session_id, to)?;

        let lca_oid = match repo.merge_base(from_oid, to_oid) {
            Ok(id) => Some(id.detach()),
            Err(_) => None,
        };
        let common_ancestor_id = lca_oid.map(|o| o.to_string());

        let only_in_from = collect_unique_commits(&repo, from_oid, lca_oid)?;
        let only_in_to = collect_unique_commits(&repo, to_oid, lca_oid)?;

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
    repo: &gix::Repository,
    session_id: &str,
    target: &str,
) -> Result<gix::ObjectId> {
    // Try as branch ref first.
    if let Ok(ref_name) = session_ref_name(session_id, target)
        && let Some(oid) = try_ref_target(repo, &ref_name)?
    {
        return Ok(oid);
    }

    // Try as commit OID.
    let oid = parse_oid(target).map_err(|_| SessionError::MissingCommit(target.to_string()))?;
    if repo.find_commit(oid).is_err() {
        return Err(SessionError::MissingCommit(target.to_string()));
    }
    Ok(oid)
}

fn collect_unique_commits(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    hide: Option<gix::ObjectId>,
) -> Result<Vec<TurnLogEntry>> {
    // Newest-first to match historical TOPOLOGICAL (no reverse) order used
    // by the previous implementation of this collector.
    let mut platform = repo.rev_walk([tip]);
    if let Some(hidden) = hide {
        platform = platform.with_hidden(Some(hidden));
    }
    let walk = platform
        .all()
        .map_err(|e| SessionError::Git(format!("create revwalk: {e}")))?;

    let mut entries = Vec::new();
    for item in walk {
        let info = item.map_err(|e| SessionError::Git(format!("revwalk error: {e}")))?;
        let oid = info.id;
        let manifest_bytes = read_tree_blob(repo, oid, "turn/manifest.json")?;
        let manifest = parse_manifest(&manifest_bytes)?;
        let messages_bytes = read_tree_blob(repo, oid, "turn/messages.jsonl")?;
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
