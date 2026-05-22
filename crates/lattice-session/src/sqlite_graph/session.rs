//! Session lifecycle and read-only metadata operations.

use chrono::Utc;
use rusqlite::{params, OptionalExtension};

use crate::graph::{
    GraphHead, GraphRef, GraphRefKind, GraphSession, SessionGraphStore, SessionGraphSummary,
    SessionStatus,
};
use crate::{Result, SessionError};

use super::{SqliteSessionGraphStore, DEFAULT_BRANCH};

impl SqliteSessionGraphStore {
    pub(super) fn create_session_impl(
        &self,
        session_id: &str,
        label: Option<&str>,
        profile_id: Option<&str>,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<GraphSession> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        conn.execute(
            "INSERT INTO graph_sessions (session_id, label, profile_id, model, provider, head_ref, head_commit_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8)",
            params![session_id, label, profile_id, model, provider, DEFAULT_BRANCH, now, now],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        // Insert initial main branch ref with no commit yet.
        conn.execute(
            "INSERT INTO graph_refs (session_id, name, kind, commit_id, updated_at)
             VALUES (?1, ?2, 'branch', NULL, ?3)",
            params![session_id, DEFAULT_BRANCH, now],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        Ok(GraphSession {
            session_id: session_id.to_string(),
            label: label.map(|s| s.to_string()),
            profile_id: profile_id.map(|s| s.to_string()),
            model: model.map(|s| s.to_string()),
            provider: provider.map(|s| s.to_string()),
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub(super) fn open_session_impl(&self, session_id: &str) -> Result<GraphSession> {
        let conn = self.conn()?;
        let session = conn
            .query_row(
                "SELECT session_id, label, profile_id, model, provider, created_at, updated_at
                 FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| {
                    Ok(GraphSession {
                        session_id: row.get(0)?,
                        label: row.get(1)?,
                        profile_id: row.get(2)?,
                        model: row.get(3)?,
                        provider: row.get(4)?,
                        created_at: row.get(5)?,
                        updated_at: row.get(6)?,
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
        let (head_ref, head_commit_id): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT head_ref, head_commit_id FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
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
                "SELECT session_id, name, kind, commit_id, updated_at
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
                    updated_at: row.get(4)?,
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
            branches,
            commit_count: commit_count as usize,
        })
    }

    pub(super) fn list_impl(&self) -> Result<Vec<SessionGraphSummary>> {
        let conn = self.conn()?;

        let mut stmt = conn
            .prepare(
                "SELECT s.session_id, s.label, s.head_ref, s.head_commit_id, s.updated_at,
                        (SELECT COUNT(*) FROM graph_commits c WHERE c.session_id = s.session_id) as commit_count
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
                    current_branch: row.get(2)?,
                    head_commit_id: row.get(3)?,
                    updated_at: row.get(4)?,
                    commit_count: row.get::<_, i64>(5)? as usize,
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
}

impl SessionGraphStore for SqliteSessionGraphStore {
    fn create_session(
        &self,
        session_id: &str,
        label: Option<&str>,
        profile_id: Option<&str>,
        model: Option<&str>,
        provider: Option<&str>,
    ) -> Result<GraphSession> {
        self.create_session_impl(session_id, label, profile_id, model, provider)
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

    fn branch(
        &self,
        session_id: &str,
        name: &str,
        from: Option<&str>,
    ) -> Result<GraphRef> {
        self.branch_impl(session_id, name, from)
    }

    fn diff(
        &self,
        session_id: &str,
        from: &str,
        to: &str,
    ) -> Result<crate::graph::GraphDiff> {
        self.diff_impl(session_id, from, to)
    }

    fn reset(&self, session_id: &str, target: &str) -> Result<crate::graph::ResetResult> {
        self.reset_impl(session_id, target)
    }
}
