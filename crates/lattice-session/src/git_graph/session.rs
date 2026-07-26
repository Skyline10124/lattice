// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session metadata operations for the Git-backed session graph store.

use chrono::Utc;

use super::helpers::{
    SessionMetadata, count_reachable_commits, parse_oid, read_session_meta, session_ref_name,
    try_ref_target, validate_session_id, write_session_meta,
};
use super::{DEFAULT_BRANCH, GitSessionGraphStore, RepoHandle};
use crate::graph::{
    GraphHead, GraphRef, GraphRefKind, GraphSession, SessionGraphSummary, SessionStatus,
};
use crate::{Result, SessionError};

impl GitSessionGraphStore {
    pub(super) fn create_session_impl(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
    ) -> Result<GraphSession> {
        validate_session_id(session_id)?;
        crate::validate_branch_name(project_target_branch)?;
        // Ensure the git repository exists before writing sidecar meta. Auto-init
        // of a bare store (e.g. sessions.git) requires an empty destination; if
        // we write `.lattice/` first, bare init is refused.
        let _ = self.repo()?;

        let now = Utc::now().to_rfc3339();
        let meta = SessionMetadata {
            session_id: session_id.to_string(),
            label: label.map(|s| s.to_string()),
            active_agent_name: active_agent_name.to_string(),
            project_target_branch: project_target_branch.to_string(),
            created_at: now.clone(),
            updated_at: now,
            head_branch: Some(DEFAULT_BRANCH.to_string()),
            head_commit_id: None,
        };
        write_session_meta(&self.dir, session_id, &meta)?;
        Ok(session_metadata_to_graph_session(&meta))
    }

    pub(super) fn open_session_impl(&self, session_id: &str) -> Result<GraphSession> {
        let meta = read_session_meta(&self.dir, session_id)?;
        Ok(session_metadata_to_graph_session(&meta))
    }

    pub(super) fn status_impl(&self, session_id: &str) -> Result<SessionStatus> {
        let meta = read_session_meta(&self.dir, session_id)?;
        let repo = self.repo()?;

        let branches = enumerate_session_branches(&repo, session_id)?;
        let head_oid = resolve_head_oid(&repo, &meta, session_id)?;
        let commit_count = match head_oid {
            Some(oid) => count_reachable_commits(&repo, oid)?,
            None => 0,
        };

        let head = match &meta.head_branch {
            Some(name) => GraphHead::Branch { name: name.clone() },
            None => match &meta.head_commit_id {
                Some(cid) => GraphHead::Detached {
                    commit_id: cid.clone(),
                },
                None => GraphHead::Branch {
                    name: DEFAULT_BRANCH.to_string(),
                },
            },
        };

        Ok(SessionStatus {
            session_id: session_id.to_string(),
            head,
            current_commit_id: head_oid.map(|o| o.to_string()),
            active_agent_name: meta.active_agent_name.clone(),
            branches,
            commit_count,
            project_target_branch: Some(meta.project_target_branch.clone()),
        })
    }

    pub(super) fn list_impl(&self) -> Result<Vec<SessionGraphSummary>> {
        let sessions_dir = self.dir.join(".lattice").join("sessions");
        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut summaries = Vec::new();
        for entry in std::fs::read_dir(&sessions_dir).map_err(|source| SessionError::Io {
            operation: "reading sessions directory",
            path: sessions_dir.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| SessionError::Io {
                operation: "reading sessions directory entry",
                path: sessions_dir.clone(),
                source,
            })?;
            let session_dir = entry.path();
            let meta_path = session_dir.join("meta.json");
            if !meta_path.is_file() {
                continue;
            }
            let session_id = session_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if session_id.is_empty() {
                continue;
            }
            let meta = match read_session_meta(&self.dir, &session_id) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let repo = self.repo()?;
            let head_oid = resolve_head_oid(&repo, &meta, &session_id).ok().flatten();
            let commit_count = match head_oid {
                Some(oid) => count_reachable_commits(&repo, oid).unwrap_or(0),
                None => 0,
            };

            summaries.push(SessionGraphSummary {
                session_id: meta.session_id,
                label: meta.label,
                active_agent_name: meta.active_agent_name,
                current_branch: meta.head_branch,
                head_commit_id: head_oid.map(|o| o.to_string()),
                commit_count,
                updated_at: meta.updated_at,
                project_target_branch: Some(meta.project_target_branch),
            });
        }
        Ok(summaries)
    }

    pub(super) fn set_active_agent_impl(
        &self,
        session_id: &str,
        agent_name: &str,
    ) -> Result<GraphSession> {
        let mut meta = read_session_meta(&self.dir, session_id)?;
        meta.active_agent_name = agent_name.to_string();
        meta.updated_at = Utc::now().to_rfc3339();
        write_session_meta(&self.dir, session_id, &meta)?;
        Ok(session_metadata_to_graph_session(&meta))
    }
}

pub(super) fn session_metadata_to_graph_session(meta: &SessionMetadata) -> GraphSession {
    GraphSession {
        session_id: meta.session_id.clone(),
        label: meta.label.clone(),
        active_agent_name: meta.active_agent_name.clone(),
        created_at: meta.created_at.clone(),
        updated_at: meta.updated_at.clone(),
        project_target_branch: Some(meta.project_target_branch.clone()),
    }
}

pub(super) fn enumerate_session_branches(
    repo: &RepoHandle<'_>,
    session_id: &str,
) -> Result<Vec<GraphRef>> {
    let prefix = format!("refs/lattice/sessions/{session_id}/");
    let mut refs = Vec::new();
    let platform = repo
        .references()
        .map_err(|e| SessionError::Git(format!("enumerate references: {e}")))?;
    let iter = platform
        .all()
        .map_err(|e| SessionError::Git(format!("iterate references: {e}")))?;
    for r in iter {
        let reference = r.map_err(|e| SessionError::Git(format!("read reference: {e}")))?;
        let name = reference.name().as_bstr().to_string();
        if !name.starts_with(&prefix) {
            continue;
        }
        let branch_name = name.strip_prefix(&prefix).unwrap_or(&name).to_string();
        let commit_id = reference.target().try_id().map(|o| o.to_string());
        // Active agent name is not stored per ref; we leave it empty here and
        // callers may enrich it from meta if needed.
        //
        // NOTE: `updated_at` reflects the time of enumeration, not the
        // time the ref was last updated. Git does not expose per-ref last-write
        // timestamps without parsing the reflog, and the reflog may be absent
        // in bare or shared repositories. Callers that need the true
        // last-update time should read the commit's committer timestamp via
        // `commit_id` instead.
        refs.push(GraphRef {
            session_id: session_id.to_string(),
            name: branch_name,
            kind: GraphRefKind::Branch,
            commit_id,
            active_agent_name: String::new(),
            updated_at: Utc::now().to_rfc3339(),
        });
    }
    Ok(refs)
}

pub(super) fn resolve_head_oid(
    repo: &RepoHandle<'_>,
    meta: &SessionMetadata,
    session_id: &str,
) -> Result<Option<gix::ObjectId>> {
    if let Some(branch) = &meta.head_branch {
        let ref_name = session_ref_name(session_id, branch)?;
        try_ref_target(repo, &ref_name)
    } else if let Some(commit_id) = &meta.head_commit_id {
        let oid = parse_oid(commit_id)
            .map_err(|e| SessionError::Git(format!("parse detached HEAD commit id: {e}")))?;
        Ok(Some(oid))
    } else {
        Ok(None)
    }
}
