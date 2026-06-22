// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session lifecycle and read-only metadata operations.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};

use crate::graph::{
    GraphHead, GraphRef, GraphRefKind, GraphSession, PreparedTurnCommit, SessionGraphStore,
    SessionGraphSummary, SessionStatus, TurnCommit,
};
use crate::{Result, SessionError};

use super::{DEFAULT_BRANCH, SqliteSessionGraphStore};

impl SqliteSessionGraphStore {
    pub(super) fn create_session_impl(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
    ) -> Result<GraphSession> {
        // Validate project target branch syntax before insertion.
        crate::validate_branch_name(project_target_branch)?;

        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO graph_sessions (session_id, label, active_agent_name, head_ref, head_commit_id, project_target_branch, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7)",
            params![session_id, label, active_agent_name, DEFAULT_BRANCH, project_target_branch, now, now],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        // Insert initial main branch ref with no commit yet.
        conn.execute(
            "INSERT INTO graph_refs (session_id, name, kind, commit_id, active_agent_name, updated_at)
             VALUES (?1, ?2, 'branch', NULL, ?3, ?4)",
            params![session_id, DEFAULT_BRANCH, active_agent_name, now],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        Ok(GraphSession {
            session_id: session_id.to_string(),
            label: label.map(|s| s.to_string()),
            active_agent_name: active_agent_name.to_string(),
            created_at: now.clone(),
            updated_at: now,
            project_target_branch: Some(project_target_branch.to_string()),
        })
    }

    pub(super) fn open_session_impl(&self, session_id: &str) -> Result<GraphSession> {
        let conn = self.conn()?;
        let session = conn
            .query_row(
                "SELECT session_id, label, active_agent_name, created_at, updated_at, project_target_branch
                 FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| {
                    Ok(GraphSession {
                        session_id: row.get(0)?,
                        label: row.get(1)?,
                        active_agent_name: row.get(2)?,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                        project_target_branch: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        session.ok_or_else(|| SessionError::NotFound(session_id.to_string()))
    }

    pub(super) fn status_impl(&self, session_id: &str) -> Result<SessionStatus> {
        let conn = self.conn()?;

        // Read session row.
        let (head_ref, head_commit_id, active_agent_name, project_target_branch): (
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT head_ref, head_commit_id, active_agent_name, project_target_branch FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|_| SessionError::NotFound(session_id.to_string()))?;

        // HEAD state.
        let head = match &head_ref {
            Some(name) => GraphHead::Branch { name: name.clone() },
            None => match &head_commit_id {
                Some(cid) => GraphHead::Detached {
                    commit_id: cid.clone(),
                },
                None => GraphHead::Branch {
                    name: DEFAULT_BRANCH.to_string(),
                },
            },
        };

        // All branch refs for this session.
        let mut ref_stmt = conn
            .prepare(
                "SELECT session_id, name, kind, commit_id, active_agent_name, updated_at
                 FROM graph_refs WHERE session_id=?1",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
        let branches: Vec<GraphRef> = ref_stmt
            .query_map(params![session_id], |row| {
                Ok(GraphRef {
                    session_id: row.get(0)?,
                    name: row.get(1)?,
                    kind: GraphRefKind::Branch,
                    commit_id: row.get(3)?,
                    active_agent_name: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        // Commit count.
        let commit_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE session_id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        Ok(SessionStatus {
            session_id: session_id.to_string(),
            head,
            current_commit_id: head_commit_id,
            active_agent_name,
            branches,
            commit_count: commit_count as usize,
            project_target_branch,
        })
    }

    pub(super) fn list_impl(&self) -> Result<Vec<SessionGraphSummary>> {
        let conn = self.conn()?;

        let mut stmt = conn
            .prepare(
                "SELECT s.session_id, s.label, s.active_agent_name, s.head_ref, s.head_commit_id, s.updated_at,
                        (SELECT COUNT(*) FROM graph_commits c WHERE c.session_id = s.session_id) as commit_count,
                        s.project_target_branch
                 FROM graph_sessions s",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        let summaries = stmt
            .query_map([], |row| {
                Ok(SessionGraphSummary {
                    session_id: row.get(0)?,
                    label: row.get(1)?,
                    active_agent_name: row.get(2)?,
                    current_branch: row.get(3)?,
                    head_commit_id: row.get(4)?,
                    updated_at: row.get(5)?,
                    commit_count: row.get::<_, i64>(6)? as usize,
                    project_target_branch: row.get(7)?,
                })
            })
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        Ok(summaries)
    }

    pub(super) fn set_active_agent_impl(
        &self,
        session_id: &str,
        agent_name: &str,
    ) -> Result<GraphSession> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        let head_ref: Option<String> = conn
            .query_row(
                "SELECT head_ref FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))?;

        let branch_name = head_ref.ok_or_else(|| {
            SessionError::Conflict(
                "cannot set active agent while HEAD is detached; checkout a branch first"
                    .to_string(),
            )
        })?;

        conn.execute(
            "UPDATE graph_refs SET active_agent_name=?1, updated_at=?2 WHERE session_id=?3 AND kind='branch' AND name=?4",
            params![agent_name, now, session_id, branch_name],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        conn.execute(
            "UPDATE graph_sessions SET active_agent_name=?1, updated_at=?2 WHERE session_id=?3",
            params![agent_name, now, session_id],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        drop(conn);
        self.open_session_impl(session_id)
    }
}

impl SessionGraphStore for SqliteSessionGraphStore {
    fn create_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        active_agent_name: &str,
        project_target_branch: &str,
    ) -> Result<GraphSession> {
        self.create_session_impl(session_id, label, active_agent_name, project_target_branch)
    }

    fn open_session(&self, session_id: &str) -> Result<GraphSession> {
        self.open_session_impl(session_id)
    }

    fn status(&self, session_id: &str) -> Result<SessionStatus> {
        self.status_impl(session_id)
    }

    fn list(&self) -> Result<Vec<SessionGraphSummary>> {
        self.list_impl()
    }

    fn head_transcript(&self, session_id: &str) -> Result<crate::graph::HeadTranscript> {
        self.head_transcript_impl(session_id)
    }

    fn log(&self, session_id: &str) -> Result<Vec<crate::graph::TurnLogEntry>> {
        self.log_impl(session_id)
    }

    fn commit_turn(
        &self,
        input: crate::graph::TurnCommitInput,
    ) -> Result<crate::graph::TurnCommit> {
        self.commit_turn_outer(input)
    }

    fn checkout(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        self.checkout_impl(session_id, target)
    }

    fn branch(&self, session_id: &str, name: &str, from: Option<&str>) -> Result<GraphRef> {
        self.branch_impl(session_id, name, from)
    }

    fn set_active_agent(&self, session_id: &str, agent_name: &str) -> Result<GraphSession> {
        self.set_active_agent_impl(session_id, agent_name)
    }

    fn diff(&self, session_id: &str, from: &str, to: &str) -> Result<crate::graph::GraphDiff> {
        self.diff_impl(session_id, from, to)
    }

    fn reset(&self, session_id: &str, target: &str) -> Result<crate::graph::ResetResult> {
        self.reset_impl(session_id, target)
    }

    fn commit_prepared_turn(&self, prepared: PreparedTurnCommit) -> Result<TurnCommit> {
        self.commit_prepared_turn_outer(prepared)
    }

    fn get_turn_commit(&self, session_id: &str, commit_id: &str) -> Result<Option<TurnCommit>> {
        self.get_turn_commit_impl(session_id, commit_id)
    }

    fn set_snapshot_lost(&self, commit_id: &str, lost: bool) -> Result<()> {
        self.set_snapshot_lost_impl(commit_id, lost)
    }

    fn get_snapshot_lost(&self, commit_id: &str) -> Result<Option<bool>> {
        self.get_snapshot_lost_impl(commit_id)
    }
}
