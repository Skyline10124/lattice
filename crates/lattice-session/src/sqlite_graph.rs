//! SQLite graph store for sessions, commits, messages, and refs.
//!
//! Implements `SessionGraphStore` over SQLite with a single shared
//! connection, WAL journaling, and lazy schema initialization.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::{
    graph::{
        canonical_message_id, canonical_turn_commit_id, canonical_turn_seed, GraphDiff, GraphHead,
        GraphMessage, GraphRef, GraphRefKind, GraphSession, GraphToolCall, HeadTranscript,
        ResetResult, SessionGraphStore, SessionGraphSummary, SessionStatus, TurnCommit,
        TurnCommitInput, TurnCommitStatus, TurnLogEntry, TurnUsage,
    },
    Result, SessionError,
};

const DEFAULT_BRANCH: &str = "main";

/// SQLite-backed session graph store.
///
/// Uses a single shared connection guarded by a Mutex. The connection is
/// opened lazily on first access (so `new` remains infallible) and runs
/// schema DDL plus PRAGMA journal_mode=WAL once on initialization.
pub struct SqliteSessionGraphStore {
    dir: PathBuf,
    db_path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

/// RAII handle around the shared connection. Derefs to `Connection`, so
/// existing call sites can use `self.conn()?.execute(...)` unchanged.
pub(crate) struct ConnHandle<'a> {
    guard: MutexGuard<'a, Option<Connection>>,
}

impl<'a> std::ops::Deref for ConnHandle<'a> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.guard
            .as_ref()
            .expect("connection initialized by SqliteSessionGraphStore::conn()")
    }
}

impl SqliteSessionGraphStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let db_path = dir.join("session_v11.db");
        Self {
            dir,
            db_path,
            conn: Mutex::new(None),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Acquire a locked handle to the shared connection, initializing on
    /// first call (creates the directory, opens the DB, runs schema DDL,
    /// enables WAL journaling).
    fn conn(&self) -> Result<ConnHandle<'_>> {
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            crate::create_dir_all(&self.dir)?;
            let conn =
                Connection::open(&self.db_path).map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            self.init_schema(&conn)?;
            *guard = Some(conn);
        }
        Ok(ConnHandle { guard })
    }

    /// Initialize schema, PRAGMAs, and idempotent ALTER TABLE migrations.
    fn init_schema(&self, conn: &Connection) -> Result<()> {
        // Enable WAL for concurrent readers and reduce SQLITE_BUSY under load.
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        conn.execute_batch(
            "
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS graph_sessions (
                session_id   TEXT PRIMARY KEY,
                label        TEXT,
                profile_id   TEXT,
                model        TEXT,
                provider     TEXT,
                head_ref     TEXT,
                head_commit_id TEXT,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS graph_refs (
                session_id TEXT NOT NULL,
                name       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                commit_id  TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (session_id, kind, name),
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );

            CREATE TABLE IF NOT EXISTS graph_commits (
                commit_id            TEXT PRIMARY KEY,
                session_id           TEXT NOT NULL,
                parents_json         TEXT NOT NULL,
                message_ids_json     TEXT NOT NULL,
                profile_snapshot_id  TEXT,
                profile_snapshot_json TEXT,
                model                TEXT NOT NULL,
                provider             TEXT NOT NULL,
                status               TEXT NOT NULL,
                commit_message       TEXT NOT NULL,
                usage_json           TEXT,
                terminal_reason      TEXT,
                created_at           TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );

            CREATE TABLE IF NOT EXISTS graph_messages (
                message_id     TEXT PRIMARY KEY,
                session_id     TEXT NOT NULL,
                role           TEXT NOT NULL,
                content        TEXT NOT NULL,
                reasoning_content TEXT,
                tool_call_id   TEXT,
                name           TEXT,
                tool_calls_json TEXT NOT NULL DEFAULT '[]',
                created_at     TEXT NOT NULL,
                ordinal        INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            ",
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        // Idempotent column additions for existing databases.
        // SQLite does not support IF NOT EXISTS on ALTER TABLE, so we catch
        // the "duplicate column name" error and ignore it.
        for col_sql in &[
            "ALTER TABLE graph_commits ADD COLUMN usage_json TEXT",
            "ALTER TABLE graph_commits ADD COLUMN terminal_reason TEXT",
        ] {
            match conn.execute_batch(col_sql) {
                Ok(()) => {}
                Err(e) => {
                    let msg = e.to_string();
                    if !msg.contains("duplicate column name") {
                        return Err(SessionError::Database {
                            path: self.db_path.clone(),
                            source: e,
                        });
                    }
                }
            }
        }

        Ok(())
    }
}

impl SqliteSessionGraphStore {
    /// Resolve the current HEAD commit ID.
    ///
    /// For attached HEAD (branch), reads the branch ref's commit_id.
    /// For detached HEAD, reads graph_sessions.head_commit_id directly.
    fn current_head_commit_id(
        &self,
        conn: &Connection,
        session_id: &str,
    ) -> Result<Option<String>> {
        let (head_ref, head_commit_id): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT head_ref, head_commit_id FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| {
                    let hr: Option<String> = row.get(0)?;
                    let hcid: Option<String> = row.get(1)?;
                    Ok((hr, hcid))
                },
            )
            .map_err(|_| SessionError::NotFound(session_id.to_string()))?;

        match head_ref {
            Some(branch_name) => {
                // Attached HEAD: read commit_id from the branch ref.
                let branch_row: Option<Option<String>> = conn
                    .query_row(
                        "SELECT commit_id FROM graph_refs WHERE session_id=?1 AND kind='branch' AND name=?2",
                        params![session_id, branch_name],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })?;

                match branch_row {
                    Some(commit_id) => Ok(commit_id),
                    None => Err(SessionError::NotFound(format!(
                        "branch '{branch_name}' not found for session '{session_id}'"
                    ))),
                }
            }
            None => {
                // Detached HEAD: use head_commit_id directly.
                Ok(head_commit_id)
            }
        }
    }

    /// Parse a JSON column from the SQLite row into a typed value.
    ///
    /// Returns `SessionError::Database` if the stored JSON is malformed,
    /// preventing silent data loss. Used for `parents_json` and
    /// `message_ids_json` columns where corruption would otherwise yield
    /// an empty Vec and produce subtle wrong-graph behavior.
    fn parse_json_column<T>(&self, column: &str, raw: &str) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        serde_json::from_str(raw).map_err(|err| SessionError::Database {
            path: self.db_path.clone(),
            source: rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("malformed JSON in column '{column}': {err}"),
                )),
            ),
        })
    }
}

impl SessionGraphStore for SqliteSessionGraphStore {
    fn head_transcript(&self, session_id: &str) -> crate::Result<HeadTranscript> {
        let conn = self.conn()?;

        // Resolve the current HEAD commit ID (attached or detached).
        let head_cid = self.current_head_commit_id(&conn, session_id)?;

        // No commits on current HEAD.
        let parent_commit_id = match &head_cid {
            None => {
                return Ok(HeadTranscript {
                    session_id: session_id.to_string(),
                    parent_commit_id: None,
                    messages: Vec::new(),
                });
            }
            Some(cid) => cid.clone(),
        };

        // Walk first-parent ancestry from root to HEAD to collect ordered
        // commit IDs. We reverse at the end to get root-first order.
        let mut commit_ids_rev: Vec<String> = Vec::new();
        let mut current = Some(parent_commit_id.clone());
        while let Some(cid) = current {
            commit_ids_rev.push(cid.clone());
            let parents = self.get_parent_ids(&conn, &cid)?;
            current = parents.into_iter().next();
        }
        commit_ids_rev.reverse();

        // For each commit in order, read its message_ids_json and load the
        // actual messages from graph_messages in stored ordinal order.
        let mut all_messages: Vec<GraphMessage> = Vec::new();
        for cid in &commit_ids_rev {
            let message_ids_json: String = conn
                .query_row(
                    "SELECT message_ids_json FROM graph_commits WHERE commit_id=?1",
                    params![cid],
                    |row| row.get(0),
                )
                .map_err(|_| SessionError::MissingCommit(cid.clone()))?;

            let message_ids: Vec<String> =
                self.parse_json_column("message_ids_json", &message_ids_json)?;

            for mid in &message_ids {
                let (role, content, reasoning_content, tool_call_id, name, tool_calls_json): (
                    String,
                    String,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    String,
                ) = conn
                    .query_row(
                        "SELECT role, content, reasoning_content, tool_call_id, name, tool_calls_json
                         FROM graph_messages WHERE message_id=?1",
                        params![mid],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                                row.get(5)?,
                            ))
                        },
                    )
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })?;

                let tool_calls: Option<Vec<GraphToolCall>> = serde_json::from_str(&tool_calls_json)
                    .ok()
                    .filter(|v: &Vec<GraphToolCall>| !v.is_empty());

                all_messages.push(GraphMessage {
                    role,
                    content,
                    reasoning_content,
                    tool_calls,
                    tool_call_id,
                    name,
                });
            }
        }

        // The parent commit ID is the HEAD commit (the last in the chain).
        Ok(HeadTranscript {
            session_id: session_id.to_string(),
            parent_commit_id: Some(parent_commit_id),
            messages: all_messages,
        })
    }

    fn create_session(
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

    fn open_session(&self, session_id: &str) -> Result<GraphSession> {
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

    fn status(&self, session_id: &str) -> Result<SessionStatus> {
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

    fn list(&self) -> Result<Vec<SessionGraphSummary>> {
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

    fn log(&self, session_id: &str) -> Result<Vec<TurnLogEntry>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT commit_id, parents_json, status, model, provider, commit_message, created_at, message_ids_json, usage_json, terminal_reason
                 FROM graph_commits WHERE session_id=?1 ORDER BY created_at ASC",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        // Collect raw rows with message_ids for tool count resolution.
        struct RawRow {
            commit_id: String,
            parent_ids: Vec<String>,
            status: TurnCommitStatus,
            model: String,
            provider: String,
            commit_message: String,
            created_at: String,
            message_ids: Vec<String>,
            usage: Option<TurnUsage>,
            terminal_reason: Option<String>,
        }

        let rows: Vec<RawRow> = stmt
            .query_map(params![session_id], |row| {
                let commit_id: String = row.get(0)?;
                let parents_json: String = row.get(1)?;
                let status_str: String = row.get(2)?;
                let model: String = row.get(3)?;
                let provider: String = row.get(4)?;
                let commit_message: String = row.get(5)?;
                let created_at: String = row.get(6)?;
                let message_ids_json: String = row.get(7)?;
                let usage_json: Option<String> = row.get(8)?;
                let terminal_reason: Option<String> = row.get(9)?;

                let parent_ids: Vec<String> =
                    serde_json::from_str(&parents_json).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("malformed JSON in parents_json: {err}"),
                            )),
                        )
                    })?;
                let message_ids: Vec<String> =
                    serde_json::from_str(&message_ids_json).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("malformed JSON in message_ids_json: {err}"),
                            )),
                        )
                    })?;
                let status = match status_str.as_str() {
                    "completed" => TurnCommitStatus::Completed,
                    "error" => TurnCommitStatus::Error,
                    "cancelled" => TurnCommitStatus::Cancelled,
                    _ => TurnCommitStatus::Completed,
                };
                let usage: Option<TurnUsage> =
                    usage_json.and_then(|json| serde_json::from_str(&json).ok());

                Ok(RawRow {
                    commit_id,
                    parent_ids,
                    status,
                    model,
                    provider,
                    commit_message,
                    created_at,
                    message_ids,
                    usage,
                    terminal_reason,
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

        // Collect all message IDs across all commits for a single batch query.
        let all_message_ids: Vec<&str> = rows
            .iter()
            .flat_map(|r| r.message_ids.iter().map(|s| s.as_str()))
            .collect();

        // Build a set of message_id -> role for tool count resolution.
        let mut tool_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        if !all_message_ids.is_empty() {
            let placeholders: String = std::iter::repeat_n("?", all_message_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT message_id FROM graph_messages WHERE message_id IN ({placeholders}) AND role = 'tool'"
            );
            let params: Vec<Box<dyn rusqlite::types::ToSql>> = all_message_ids
                .iter()
                .map(|s| Box::new(s.to_string()) as Box<dyn rusqlite::types::ToSql>)
                .collect();
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|p| p.as_ref()).collect();
            let mut tool_stmt = conn
                .prepare(&sql)
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            let tool_ids = tool_stmt
                .query_map(param_refs.as_slice(), |row| row.get::<_, String>(0))
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            for id in tool_ids {
                let id = id.map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
                tool_set.insert(id);
            }
        }

        // Build log entries with accurate tool counts.
        let result = rows
            .into_iter()
            .map(|r| {
                let tool_message_count = r
                    .message_ids
                    .iter()
                    .filter(|mid| tool_set.contains(mid.as_str()))
                    .count();
                TurnLogEntry {
                    commit_id: r.commit_id,
                    parent_ids: r.parent_ids,
                    status: r.status,
                    model: r.model,
                    provider: r.provider,
                    commit_message: r.commit_message,
                    message_count: r.message_ids.len(),
                    tool_message_count,
                    usage: r.usage,
                    terminal_reason: r.terminal_reason,
                    created_at: r.created_at,
                }
            })
            .collect();

        Ok(result)
    }

    fn commit_turn(&self, input: TurnCommitInput) -> Result<TurnCommit> {
        let conn = self.conn()?;

        // Begin immediate transaction for atomicity (T-08-04).
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        // Run the actual work in a closure so we can rollback on error.
        let result = self.commit_turn_inner(&conn, &input);

        match result {
            Ok(commit) => {
                conn.execute_batch("COMMIT")
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })?;
                Ok(commit)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    fn checkout(&self, session_id: &str, target: &str) -> Result<GraphHead> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        // Verify session exists.
        let _: String = conn
            .query_row(
                "SELECT session_id FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|_| SessionError::NotFound(session_id.to_string()))?;

        // Try target as a branch name first.
        // Distinguish "branch exists with NULL commit_id" from "branch missing".
        let branch_row: Option<Option<String>> = conn
            .query_row(
                "SELECT commit_id FROM graph_refs WHERE session_id=?1 AND kind='branch' AND name=?2",
                params![session_id, target],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        if let Some(commit_id) = branch_row {
            // Branch exists (commit_id may be NULL).
            conn.execute(
                "UPDATE graph_sessions SET head_ref=?1, head_commit_id=?2, updated_at=?3 WHERE session_id=?4",
                params![target, commit_id, now, session_id],
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
            return Ok(GraphHead::Branch {
                name: target.to_string(),
            });
        }

        // Try target as a commit ID.
        let commit_exists: bool =
            conn.query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                params![target, session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })? > 0;

        if commit_exists {
            // Detach HEAD at the commit.
            conn.execute(
                "UPDATE graph_sessions SET head_ref=NULL, head_commit_id=?1, updated_at=?2 WHERE session_id=?3",
                params![target, now, session_id],
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;
            return Ok(GraphHead::Detached {
                commit_id: target.to_string(),
            });
        }

        // Target is neither a branch nor a commit in this session.
        Err(SessionError::MissingCommit(target.to_string()))
    }

    fn branch(&self, session_id: &str, name: &str, from: Option<&str>) -> Result<GraphRef> {
        crate::validate_branch_name(name)?;
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        // Resolve the target commit ID.
        let commit_id = match from {
            Some(cid) => {
                // Verify the commit exists in this session.
                let exists: bool =
                    conn.query_row(
                        "SELECT COUNT(*) FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                        params![cid, session_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })? > 0;
                if !exists {
                    return Err(SessionError::MissingCommit(cid.to_string()));
                }
                cid.to_string()
            }
            None => {
                // Use current HEAD commit (attached or detached).
                self.current_head_commit_id(&conn, session_id)?
                    .ok_or_else(|| {
                        SessionError::Conflict(
                            "cannot create branch: no commits on current HEAD".to_string(),
                        )
                    })?
            }
        };

        // Insert or update the branch ref.
        conn.execute(
            "INSERT INTO graph_refs (session_id, name, kind, commit_id, updated_at)
             VALUES (?1, ?2, 'branch', ?3, ?4)
             ON CONFLICT(session_id, kind, name) DO UPDATE SET commit_id=?3, updated_at=?4",
            params![session_id, name, commit_id, now],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        // If this branch is the currently checked-out branch, also sync session head_commit_id.
        // On detached HEAD (head_ref IS NULL) or a non-current branch, this is a no-op.
        conn.execute(
            "UPDATE graph_sessions SET head_commit_id=?1, updated_at=?2 WHERE session_id=?3 AND head_ref=?4",
            params![commit_id, now, session_id, name],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        Ok(GraphRef {
            session_id: session_id.to_string(),
            name: name.to_string(),
            kind: GraphRefKind::Branch,
            commit_id: Some(commit_id),
            updated_at: now,
        })
    }

    fn diff(&self, session_id: &str, from: &str, to: &str) -> Result<GraphDiff> {
        let conn = self.conn()?;

        // Resolve both targets to commit IDs.
        let from_id = self.resolve_target_to_commit_id(&conn, session_id, from)?;
        let to_id = self.resolve_target_to_commit_id(&conn, session_id, to)?;

        // Build ancestor set for `from` (including from itself).
        let from_ancestors = self.collect_ancestors(&conn, &from_id)?;
        let to_ancestors = self.collect_ancestors(&conn, &to_id)?;

        // Find common ancestor: walk from `from` towards root, first commit
        // that appears in to_ancestors.
        let common_ancestor_id = {
            let mut current = Some(from_id.clone());
            let mut found = None;
            while let Some(cid) = current {
                if to_ancestors.contains(&cid) {
                    found = Some(cid);
                    break;
                }
                // Get parent of current.
                current = self.get_parent_ids(&conn, &cid)?.into_iter().next();
            }
            found
        };

        // Commits only in `from`: from_ancestors excluding to_ancestors,
        // but only those reachable from `from` up to the common ancestor.
        let only_in_from = self.collect_unique_entries(
            &conn,
            session_id,
            &from_id,
            common_ancestor_id.as_deref(),
            &to_ancestors,
        )?;

        let only_in_to = self.collect_unique_entries(
            &conn,
            session_id,
            &to_id,
            common_ancestor_id.as_deref(),
            &from_ancestors,
        )?;

        Ok(GraphDiff {
            from_commit_id: from_id,
            to_commit_id: to_id,
            common_ancestor_id,
            only_in_from,
            only_in_to,
        })
    }

    fn reset(&self, session_id: &str, target: &str) -> Result<ResetResult> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();

        // Get current HEAD info.
        let (head_ref, old_commit_id): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT head_ref, head_commit_id FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?
            .ok_or_else(|| SessionError::NotFound(session_id.to_string()))?;

        let branch_name = match head_ref {
            Some(name) => name,
            None => {
                return Err(SessionError::Conflict(
                    "cannot reset while HEAD is detached; checkout a branch first".to_string(),
                ));
            }
        };

        // Resolve target to a commit ID.
        let target_commit_id = self.resolve_target_to_commit_id(&conn, session_id, target)?;

        // Verify the target commit exists.
        let exists: bool =
            conn.query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                params![target_commit_id, session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })? > 0;
        if !exists {
            return Err(SessionError::MissingCommit(target_commit_id));
        }

        // Begin immediate transaction for atomicity.
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        // Update the branch ref.
        let ref_result = conn.execute(
            "UPDATE graph_refs SET commit_id=?1, updated_at=?2 WHERE session_id=?3 AND kind='branch' AND name=?4",
            params![target_commit_id, now, session_id, branch_name],
        );
        if let Err(source) = ref_result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(SessionError::Database {
                path: self.db_path.clone(),
                source,
            });
        }

        // Update session head_commit_id.
        let session_result = conn.execute(
            "UPDATE graph_sessions SET head_commit_id=?1, updated_at=?2 WHERE session_id=?3",
            params![target_commit_id, now, session_id],
        );
        if let Err(source) = session_result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(SessionError::Database {
                path: self.db_path.clone(),
                source,
            });
        }

        conn.execute_batch("COMMIT")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        Ok(ResetResult {
            session_id: session_id.to_string(),
            branch: branch_name,
            old_commit_id,
            new_commit_id: target_commit_id,
        })
    }
}

impl SqliteSessionGraphStore {
    fn commit_turn_inner(&self, conn: &Connection, input: &TurnCommitInput) -> Result<TurnCommit> {
        let now = Utc::now().to_rfc3339();

        // 1. Read current HEAD commit.
        let current_commit_id = self.current_head_commit_id(conn, &input.session_id)?;

        // 2. Validate expected parent matches current HEAD (T-08-05).
        if input.parent_ids.is_empty() {
            // First commit: current HEAD must be None.
            if let Some(ref head) = current_commit_id {
                return Err(SessionError::Conflict(format!(
                    "expected no parent (first commit) but HEAD is '{head}'"
                )));
            }
        } else {
            // Subsequent commit: expected parent must equal current HEAD.
            let expected_parent = &input.parent_ids[0];
            match &current_commit_id {
                Some(head) if head == expected_parent => { /* ok */ }
                Some(head) => {
                    return Err(SessionError::Conflict(format!(
                        "expected parent '{}' but HEAD is '{}'",
                        expected_parent, head
                    )));
                }
                None => {
                    return Err(SessionError::Conflict(format!(
                        "expected parent '{}' but HEAD is empty",
                        expected_parent
                    )));
                }
            }
        }

        // 3. Write message rows and compute message IDs.
        let turn_seed = canonical_turn_seed(
            &input.session_id,
            &input.parent_ids,
            &now,
            &input.model,
            &input.provider,
            &input.status,
            &input.commit_message,
        );
        let mut message_ids = Vec::new();
        for (i, msg) in input.messages.iter().enumerate() {
            let message_id = canonical_message_id(&input.session_id, &turn_seed, msg, i);
            let tool_calls_json = match &msg.tool_calls {
                Some(calls) => serde_json::to_string(calls).unwrap_or_else(|_| "[]".to_string()),
                None => "[]".to_string(),
            };

            conn.execute(
                "INSERT INTO graph_messages (message_id, session_id, role, content, reasoning_content, tool_call_id, name, tool_calls_json, created_at, ordinal)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    message_id,
                    input.session_id,
                    msg.role,
                    msg.content,
                    msg.reasoning_content,
                    msg.tool_call_id,
                    msg.name,
                    tool_calls_json,
                    now,
                    i as i64,
                ],
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

            message_ids.push(message_id);
        }

        // 4. Compute canonical commit ID.
        let commit_id = canonical_turn_commit_id(
            &input.session_id,
            &input.parent_ids,
            &message_ids,
            input.profile_snapshot_id.as_deref(),
            &input.model,
            &input.provider,
            &input.status,
            &input.commit_message,
            &now,
        );

        // 5. Insert immutable commit row.
        let parents_json =
            serde_json::to_string(&input.parent_ids).unwrap_or_else(|_| "[]".to_string());
        let message_ids_json =
            serde_json::to_string(&message_ids).unwrap_or_else(|_| "[]".to_string());

        let usage_json = input
            .usage
            .as_ref()
            .map(|u| serde_json::to_string(u).unwrap_or_else(|_| "{}".to_string()));

        conn.execute(
            "INSERT INTO graph_commits (commit_id, session_id, parents_json, message_ids_json, profile_snapshot_id, model, provider, status, commit_message, usage_json, terminal_reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                commit_id,
                input.session_id,
                parents_json,
                message_ids_json,
                input.profile_snapshot_id,
                input.model,
                input.provider,
                input.status.as_str(),
                input.commit_message,
                usage_json,
                input.terminal_reason,
                now,
            ],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        // 6. Update branch ref (only if HEAD is attached) and session head_commit_id.
        match self.current_branch_name(conn, &input.session_id)? {
            Some(branch_name) => {
                // Attached HEAD: update the branch ref and head_commit_id.
                conn.execute(
                    "UPDATE graph_refs SET commit_id=?1, updated_at=?2 WHERE session_id=?3 AND kind='branch' AND name=?4",
                    params![commit_id, now, input.session_id, branch_name],
                )
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
                conn.execute(
                    "UPDATE graph_sessions SET head_commit_id=?1, updated_at=?2 WHERE session_id=?3",
                    params![commit_id, now, input.session_id],
                )
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            }
            None => {
                // Detached HEAD: only update head_commit_id, preserve head_ref=NULL.
                conn.execute(
                    "UPDATE graph_sessions SET head_commit_id=?1, updated_at=?2 WHERE session_id=?3",
                    params![commit_id, now, input.session_id],
                )
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
            }
        }

        Ok(TurnCommit {
            commit_id,
            session_id: input.session_id.clone(),
            parent_ids: input.parent_ids.clone(),
            message_ids,
            profile_snapshot_id: input.profile_snapshot_id.clone(),
            model: input.model.clone(),
            provider: input.provider.clone(),
            status: input.status.clone(),
            commit_message: input.commit_message.clone(),
            usage: input.usage.clone(),
            terminal_reason: input.terminal_reason.clone(),
            created_at: now,
        })
    }

    fn current_branch_name(&self, conn: &Connection, session_id: &str) -> Result<Option<String>> {
        // head_ref is NULL for detached HEAD, so read as Option<String>.
        // .optional() wraps "no rows" in outer Option; inner Option handles NULL.
        let name: Option<Option<String>> = conn
            .query_row(
                "SELECT head_ref FROM graph_sessions WHERE session_id=?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        Ok(name.flatten())
    }

    fn resolve_target_to_commit_id(
        &self,
        conn: &Connection,
        session_id: &str,
        target: &str,
    ) -> Result<String> {
        // Try as branch name first.
        let branch_commit: Option<String> = conn
            .query_row(
                "SELECT commit_id FROM graph_refs WHERE session_id=?1 AND kind='branch' AND name=?2",
                params![session_id, target],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?
            .flatten();

        if let Some(cid) = branch_commit {
            return Ok(cid);
        }

        // Try as commit ID.
        let exists: bool =
            conn.query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                params![target, session_id],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })? > 0;

        if exists {
            return Ok(target.to_string());
        }

        Err(SessionError::MissingCommit(target.to_string()))
    }

    fn get_parent_ids(&self, conn: &Connection, commit_id: &str) -> Result<Vec<String>> {
        let parents_json: String = conn
            .query_row(
                "SELECT parents_json FROM graph_commits WHERE commit_id=?1",
                params![commit_id],
                |row| row.get(0),
            )
            .map_err(|_| SessionError::MissingCommit(commit_id.to_string()))?;

        self.parse_json_column("parents_json", &parents_json)
    }

    fn collect_ancestors(
        &self,
        conn: &Connection,
        start: &str,
    ) -> Result<std::collections::HashSet<String>> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(start.to_string());
        visited.insert(start.to_string());

        while let Some(current) = queue.pop_front() {
            let parents = self.get_parent_ids(conn, &current)?;
            for parent in parents {
                if visited.insert(parent.clone()) {
                    queue.push_back(parent);
                }
            }
        }

        Ok(visited)
    }

    fn collect_unique_entries(
        &self,
        conn: &Connection,
        session_id: &str,
        start: &str,
        stop_at: Option<&str>,
        other_set: &std::collections::HashSet<String>,
    ) -> Result<Vec<TurnLogEntry>> {
        let mut entries = Vec::new();
        let mut current = Some(start.to_string());

        while let Some(cid) = current {
            // Stop if we've reached the common ancestor.
            if stop_at == Some(cid.as_str()) {
                break;
            }

            // Only include commits not in the other side's ancestors.
            if !other_set.contains(&cid) {
                // Read commit data.
                let (parents_json, status_str, model, provider, commit_message, created_at, message_ids_json): (
                    String, String, String, String, String, String, String,
                ) = conn
                    .query_row(
                        "SELECT parents_json, status, model, provider, commit_message, created_at, message_ids_json
                         FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                        params![cid, session_id],
                        |row| Ok((
                            row.get(0)?, row.get(1)?, row.get(2)?,
                            row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?,
                        )),
                    )
                    .map_err(|_| SessionError::MissingCommit(cid.clone()))?;

                let status = match status_str.as_str() {
                    "completed" => TurnCommitStatus::Completed,
                    "error" => TurnCommitStatus::Error,
                    "cancelled" => TurnCommitStatus::Cancelled,
                    _ => TurnCommitStatus::Completed,
                };
                let parent_ids: Vec<String> =
                    self.parse_json_column("parents_json", &parents_json)?;
                let message_ids: Vec<String> =
                    self.parse_json_column("message_ids_json", &message_ids_json)?;

                entries.push(TurnLogEntry {
                    commit_id: cid,
                    parent_ids: parent_ids.clone(),
                    status,
                    model,
                    provider,
                    commit_message,
                    message_count: message_ids.len(),
                    tool_message_count: 0, // simplified for diff; not counting tools here
                    usage: None,
                    terminal_reason: None,
                    created_at,
                });

                // Move to first parent.
                current = parent_ids.into_iter().next();
            } else {
                break;
            }
        }

        Ok(entries)
    }
}

#[cfg(test)]
mod sqlite_graph_commit_turn {
    use super::*;
    use crate::graph::{derive_commit_message, GraphMessage};
    use std::env;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir()
            .join("lattice-session-sqlite-graph-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn user_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "user".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn assistant_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "assistant".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    #[test]
    fn store_creates_v11_tables_on_open() {
        let dir = temp_dir("creates_tables");
        let store = SqliteSessionGraphStore::new(dir.clone());

        // Trigger lazy connection init (creates tables).
        drop(store.conn().expect("init"));

        // Verify the four v1.1 tables exist via a raw connection.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        for table in &[
            "graph_sessions",
            "graph_refs",
            "graph_commits",
            "graph_messages",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("query should succeed");
            assert_eq!(count, 1, "table '{table}' should exist");
        }

        // Verify legacy tables do NOT exist.
        for table in &["objects", "commits", "refs", "meta"] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("query should succeed");
            assert_eq!(count, 0, "legacy table '{table}' should NOT exist");
        }
    }

    #[test]
    fn graph_refs_has_commit_id_column() {
        let dir = temp_dir("refs_has_commit_id");
        let store = SqliteSessionGraphStore::new(dir);
        drop(store.conn().expect("init"));
        let conn = Connection::open(store.db_path()).expect("raw conn");

        // PRAGMA table_info returns columns for graph_refs.
        let columns: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(graph_refs)")
                .expect("pragma should succeed");
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .expect("query_map should succeed");
            rows.filter_map(|r| r.ok()).collect()
        };

        assert!(
            columns.contains(&"commit_id".to_string()),
            "graph_refs must have commit_id column, found: {columns:?}"
        );
    }

    fn tool_msg(text: &str, tool_call_id: &str, name: &str) -> GraphMessage {
        GraphMessage {
            role: "tool".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: Some(name.to_string()),
        }
    }

    fn commit_input(
        session_id: &str,
        parent_ids: Vec<String>,
        messages: Vec<GraphMessage>,
        status: TurnCommitStatus,
    ) -> TurnCommitInput {
        TurnCommitInput {
            session_id: session_id.to_string(),
            parent_ids,
            messages: messages.clone(),
            profile_snapshot_id: Some("test-profile".to_string()),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            status,
            commit_message: derive_commit_message(&messages),
            usage: None,
            terminal_reason: None,
        }
    }

    #[test]
    fn graph_refs_does_not_have_session_id_as_branch_pointer_value() {
        let dir = temp_dir("refs_no_session_id_pointer");
        let store = SqliteSessionGraphStore::new(dir);
        drop(store.conn().expect("init"));
        let conn = Connection::open(store.db_path()).expect("raw conn");

        // The commit_id column exists (tested above). There should be no
        // separate "target_session_id" or similar column used as the branch
        // pointer value. The branch pointer IS commit_id.
        let columns: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(graph_refs)")
                .expect("pragma should succeed");
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .expect("query_map should succeed");
            rows.filter_map(|r| r.ok()).collect()
        };

        // Ensure no column named "target_session_id" or "target" exists.
        assert!(
            !columns
                .iter()
                .any(|c| c == "target_session_id" || c == "target"),
            "graph_refs should not have a session-id-based pointer column, found: {columns:?}"
        );
    }

    #[test]
    fn create_session_does_not_create_commit() {
        let dir = temp_dir("create_no_commit");
        let store = SqliteSessionGraphStore::new(dir);

        let session = store
            .create_session(
                "sess-1",
                Some("test"),
                None,
                Some("sonnet"),
                Some("anthropic"),
            )
            .expect("create_session should succeed");

        assert_eq!(session.session_id, "sess-1");
        assert_eq!(session.label, Some("test".to_string()));
        assert_eq!(session.model, Some("sonnet".to_string()));

        // graph_commits count must be 0 immediately after create.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let commit_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM graph_commits", [], |row| row.get(0))
            .expect("count query");
        assert_eq!(commit_count, 0, "create_session must not create any commit");
    }

    #[test]
    fn open_session_returns_existing_session() {
        let dir = temp_dir("open_existing");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session(
                "sess-1",
                Some("test"),
                None,
                Some("sonnet"),
                Some("anthropic"),
            )
            .expect("create");

        let opened = store.open_session("sess-1").expect("open_session");
        assert_eq!(opened.session_id, "sess-1");
        assert_eq!(opened.label, Some("test".to_string()));
    }

    #[test]
    fn open_session_returns_not_found_for_missing() {
        let dir = temp_dir("open_not_found");
        let store = SqliteSessionGraphStore::new(dir);

        let result = store.open_session("nonexistent");
        assert!(
            result.is_err(),
            "open_session should fail for missing session"
        );
    }

    #[test]
    fn commit_turn_writes_messages_commit_and_branch_ref_atomically() {
        let dir = temp_dir("commit_atomic");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        let messages = vec![user_msg("Hello"), assistant_msg("Hi there!")];
        let input = commit_input("sess-1", vec![], messages, TurnCommitStatus::Completed);

        let commit = store.commit_turn(input).expect("commit_turn");

        // Commit ID is a 64-char hex string.
        assert_eq!(commit.commit_id.len(), 64);
        assert!(commit.commit_id.chars().all(|c| c.is_ascii_hexdigit()));

        // Message IDs are populated.
        assert_eq!(commit.message_ids.len(), 2);

        let conn = Connection::open(store.db_path()).expect("raw conn");

        // Verify message rows exist.
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_messages WHERE session_id='sess-1'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(msg_count, 2, "should have 2 message rows");

        // Verify commit row exists.
        let commit_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(commit_count, 1, "should have 1 commit row");

        // Verify branch ref points to new commit.
        let ref_commit_id: Option<String> = conn
            .query_row(
                "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='main'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(
            ref_commit_id,
            Some(commit.commit_id.clone()),
            "branch ref should point to new commit"
        );

        // Verify session head_commit_id points to new commit.
        let head_commit_id: Option<String> = conn
            .query_row(
                "SELECT head_commit_id FROM graph_sessions WHERE session_id='sess-1'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(
            head_commit_id,
            Some(commit.commit_id.clone()),
            "session head should point to new commit"
        );
    }

    #[test]
    fn commit_turn_rejects_stale_parent() {
        let dir = temp_dir("commit_stale_parent");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // First commit succeeds.
        let messages1 = vec![user_msg("First")];
        let input1 = commit_input("sess-1", vec![], messages1, TurnCommitStatus::Completed);
        store.commit_turn(input1).expect("first commit");

        // Second commit with a stale parent (should be first commit's ID).
        let messages2 = vec![user_msg("Second")];
        let mut input2 = commit_input(
            "sess-1",
            vec!["stale-parent-id".to_string()],
            messages2,
            TurnCommitStatus::Completed,
        );
        // Override commit_message to be unique.
        input2.commit_message = "Second".to_string();

        let result = store.commit_turn(input2);
        assert!(result.is_err(), "stale parent should be rejected");

        // Verify commit count is still 1 (the failed write didn't persist).
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let commit_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(commit_count, 1, "failed commit should not persist");
    }

    #[test]
    fn commit_turn_round_trips_tool_message() {
        let dir = temp_dir("commit_tool_msg");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        let messages = vec![
            user_msg("run ls"),
            assistant_msg("calling tool"),
            tool_msg("file1.txt\nfile2.txt", "call_123", "shell"),
        ];
        let input = commit_input("sess-1", vec![], messages, TurnCommitStatus::Completed);

        let commit = store.commit_turn(input).expect("commit_turn");
        assert_eq!(commit.message_ids.len(), 3);

        let conn = Connection::open(store.db_path()).expect("raw conn");

        // Verify tool message persisted with tool_call_id and name.
        let (role, tool_call_id, name): (String, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT role, tool_call_id, name FROM graph_messages WHERE role='tool' AND session_id='sess-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("tool message should exist");

        assert_eq!(role, "tool");
        assert_eq!(tool_call_id, Some("call_123".to_string()));
        assert_eq!(name, Some("shell".to_string()));
    }

    #[test]
    fn commit_turn_accepts_completed_error_and_cancelled_statuses() {
        let dir = temp_dir("commit_all_statuses");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        for (i, status) in [
            TurnCommitStatus::Completed,
            TurnCommitStatus::Error,
            TurnCommitStatus::Cancelled,
        ]
        .iter()
        .enumerate()
        {
            let messages = vec![user_msg(&format!("Turn {i}"))];
            let parent_ids = if i == 0 {
                vec![]
            } else {
                // Use the previous commit as parent.
                vec![store
                    .log("sess-1")
                    .expect("log")
                    .last()
                    .unwrap()
                    .commit_id
                    .clone()]
            };
            let input = TurnCommitInput {
                session_id: "sess-1".to_string(),
                parent_ids,
                messages: messages.clone(),
                profile_snapshot_id: None,
                model: "test-model".to_string(),
                provider: "test-provider".to_string(),
                status: status.clone(),
                commit_message: format!("Turn {i}"),
                usage: None,
                terminal_reason: None,
            };

            let commit = store
                .commit_turn(input)
                .unwrap_or_else(|_| panic!("commit_turn should accept status {:?}", status));
            assert_eq!(commit.status, *status);
        }

        let conn = Connection::open(store.db_path()).expect("raw conn");
        let commit_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(commit_count, 3, "should have 3 commits for 3 statuses");
    }

    #[test]
    fn commit_turn_first_commit_has_empty_parent_ids() {
        let dir = temp_dir("commit_first_empty_parents");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        let messages = vec![user_msg("First turn")];
        let input = commit_input("sess-1", vec![], messages, TurnCommitStatus::Completed);

        let commit = store.commit_turn(input).expect("commit_turn");
        assert!(
            commit.parent_ids.is_empty(),
            "first commit should have empty parent_ids"
        );
    }

    #[test]
    fn commit_turn_second_commit_has_first_as_parent() {
        let dir = temp_dir("commit_chain");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // First commit.
        let messages1 = vec![user_msg("First")];
        let input1 = commit_input("sess-1", vec![], messages1, TurnCommitStatus::Completed);
        let commit1 = store.commit_turn(input1).expect("first commit");

        // Second commit with first as parent.
        let messages2 = vec![user_msg("Second")];
        let input2 = commit_input(
            "sess-1",
            vec![commit1.commit_id.clone()],
            messages2,
            TurnCommitStatus::Completed,
        );
        let commit2 = store.commit_turn(input2).expect("second commit");

        assert_eq!(commit2.parent_ids, vec![commit1.commit_id]);
    }
}

// Head transcript and usage metadata tests

#[cfg(test)]
mod sqlite_graph_head_transcript {
    use super::*;
    use crate::graph::{derive_commit_message, GraphMessage, TurnUsage};
    use std::env;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir()
            .join("lattice-session-head-transcript-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn user_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "user".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn assistant_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "assistant".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn tool_msg(text: &str, tool_call_id: &str, name: &str) -> GraphMessage {
        GraphMessage {
            role: "tool".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: Some(name.to_string()),
        }
    }

    fn commit_input_with_usage(
        session_id: &str,
        parent_ids: Vec<String>,
        messages: Vec<GraphMessage>,
        status: TurnCommitStatus,
        usage: Option<TurnUsage>,
        terminal_reason: Option<String>,
    ) -> TurnCommitInput {
        TurnCommitInput {
            session_id: session_id.to_string(),
            parent_ids,
            messages: messages.clone(),
            profile_snapshot_id: Some("test-profile".to_string()),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            status,
            commit_message: derive_commit_message(&messages),
            usage,
            terminal_reason,
        }
    }

    #[test]
    fn head_transcript_empty_session_has_no_parent_and_no_messages() {
        let dir = temp_dir("empty_session");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        let transcript = store.head_transcript("sess-1").expect("head_transcript");

        assert_eq!(transcript.session_id, "sess-1");
        assert!(
            transcript.parent_commit_id.is_none(),
            "empty session should have no parent commit"
        );
        assert!(
            transcript.messages.is_empty(),
            "empty session should have no messages"
        );
    }

    #[test]
    fn head_transcript_reads_current_branch_history_in_commit_order() {
        let dir = temp_dir("history_order");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // First commit: user + assistant.
        let messages1 = vec![user_msg("Turn 0"), assistant_msg("Response 0")];
        let input1 = commit_input_with_usage(
            "sess-1",
            vec![],
            messages1,
            TurnCommitStatus::Completed,
            None,
            None,
        );
        let commit1 = store.commit_turn(input1).expect("first commit");

        // Second commit: user + assistant + tool.
        let messages2 = vec![
            user_msg("Turn 1"),
            assistant_msg("Calling tool"),
            tool_msg("tool output", "call_1", "shell"),
        ];
        let input2 = commit_input_with_usage(
            "sess-1",
            vec![commit1.commit_id.clone()],
            messages2,
            TurnCommitStatus::Completed,
            None,
            None,
        );
        store.commit_turn(input2).expect("second commit");

        // Third commit: user + assistant.
        let messages3 = vec![user_msg("Turn 2"), assistant_msg("Response 2")];
        let input3 = commit_input_with_usage(
            "sess-1",
            vec![store
                .log("sess-1")
                .expect("log")
                .last()
                .unwrap()
                .commit_id
                .clone()],
            messages3,
            TurnCommitStatus::Completed,
            None,
            None,
        );
        store.commit_turn(input3).expect("third commit");

        // Read transcript.
        let transcript = store.head_transcript("sess-1").expect("head_transcript");

        // Should have all 7 messages from 3 commits in order.
        assert_eq!(
            transcript.messages.len(),
            7,
            "expected 7 messages from 3 commits"
        );

        // Verify chronological order: commit 0 messages, then commit 1, then commit 2.
        assert_eq!(transcript.messages[0].content, "Turn 0");
        assert_eq!(transcript.messages[1].content, "Response 0");
        assert_eq!(transcript.messages[2].content, "Turn 1");
        assert_eq!(transcript.messages[3].content, "Calling tool");
        assert_eq!(transcript.messages[4].content, "tool output");
        assert_eq!(transcript.messages[4].role, "tool");
        assert_eq!(transcript.messages[5].content, "Turn 2");
        assert_eq!(transcript.messages[6].content, "Response 2");

        // Parent should be the last commit's ID.
        assert!(transcript.parent_commit_id.is_some());
    }

    #[test]
    fn head_transcript_respects_checkout_or_branch_head() {
        let dir = temp_dir("checkout_transcript");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // First commit on main.
        let messages1 = vec![user_msg("Main turn"), assistant_msg("Main response")];
        let input1 = commit_input_with_usage(
            "sess-1",
            vec![],
            messages1,
            TurnCommitStatus::Completed,
            None,
            None,
        );
        let commit1 = store.commit_turn(input1).expect("first commit");

        // Create feature branch from first commit.
        store
            .branch("sess-1", "feature", Some(&commit1.commit_id))
            .expect("branch");

        // Checkout feature, commit one more turn.
        store.checkout("sess-1", "feature").expect("checkout");

        let messages2 = vec![user_msg("Feature turn"), assistant_msg("Feature response")];
        let input2 = commit_input_with_usage(
            "sess-1",
            vec![commit1.commit_id.clone()],
            messages2,
            TurnCommitStatus::Completed,
            None,
            None,
        );
        store.commit_turn(input2).expect("feature commit");

        // Transcript on feature branch should have 4 messages.
        let feature_transcript = store.head_transcript("sess-1").expect("head_transcript");
        assert_eq!(
            feature_transcript.messages.len(),
            4,
            "feature branch should have 4 messages"
        );

        // Reset main to first commit, checkout main.
        store.reset("sess-1", &commit1.commit_id).expect("reset");
        store.checkout("sess-1", "main").expect("checkout");

        // Transcript on main should have only 2 messages.
        let main_transcript = store.head_transcript("sess-1").expect("head_transcript");
        assert_eq!(
            main_transcript.messages.len(),
            2,
            "main branch should have 2 messages after reset"
        );
        assert_eq!(main_transcript.messages[0].content, "Main turn");
        assert_eq!(main_transcript.messages[1].content, "Main response");
    }

    #[test]
    fn commit_turn_persists_usage_and_terminal_reason() {
        let dir = temp_dir("usage_persist");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // Completed turn with usage.
        let messages1 = vec![user_msg("Hello"), assistant_msg("Hi!")];
        let usage1 = TurnUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };
        let input1 = commit_input_with_usage(
            "sess-1",
            vec![],
            messages1,
            TurnCommitStatus::Completed,
            Some(usage1.clone()),
            Some("stop".to_string()),
        );
        let commit1 = store.commit_turn(input1).expect("commit completed");

        // Error turn with usage.
        let messages2 = vec![user_msg("Fail"), assistant_msg("Error occurred")];
        let usage2 = TurnUsage {
            prompt_tokens: 200,
            completion_tokens: 0,
            total_tokens: 200,
        };
        let input2 = commit_input_with_usage(
            "sess-1",
            vec![commit1.commit_id.clone()],
            messages2,
            TurnCommitStatus::Error,
            Some(usage2.clone()),
            Some("error: provider timeout".to_string()),
        );
        let commit2 = store.commit_turn(input2).expect("commit error");

        // Cancelled turn without usage.
        let messages3 = vec![user_msg("Cancel me")];
        let input3 = commit_input_with_usage(
            "sess-1",
            vec![commit2.commit_id.clone()],
            messages3,
            TurnCommitStatus::Cancelled,
            None,
            Some("cancelled by user".to_string()),
        );
        let commit3 = store.commit_turn(input3).expect("commit cancelled");

        // Verify usage round-trips through log.
        let log = store.log("sess-1").expect("log");
        assert_eq!(log.len(), 3);

        // Completed turn.
        assert_eq!(log[0].usage, Some(usage1));
        assert_eq!(log[0].terminal_reason, Some("stop".to_string()));

        // Error turn.
        assert_eq!(log[1].usage, Some(usage2));
        assert_eq!(
            log[1].terminal_reason,
            Some("error: provider timeout".to_string())
        );

        // Cancelled turn.
        assert!(log[2].usage.is_none());
        assert_eq!(
            log[2].terminal_reason,
            Some("cancelled by user".to_string())
        );

        // Verify usage round-trips through commit output.
        assert_eq!(
            commit1.usage,
            Some(TurnUsage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150
            })
        );
        assert_eq!(commit1.terminal_reason, Some("stop".to_string()));
        assert_eq!(
            commit2.usage,
            Some(TurnUsage {
                prompt_tokens: 200,
                completion_tokens: 0,
                total_tokens: 200
            })
        );
        assert_eq!(
            commit2.terminal_reason,
            Some("error: provider timeout".to_string())
        );
        assert!(commit3.usage.is_none());
        assert_eq!(
            commit3.terminal_reason,
            Some("cancelled by user".to_string())
        );
    }

    #[test]
    fn head_transcript_uses_detached_head_commit_id() {
        let dir = temp_dir("detached_transcript");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // Commit first turn on main.
        let messages1 = vec![user_msg("Hello"), assistant_msg("Hi there!")];
        let input1 = commit_input_with_usage(
            "sess-1",
            vec![],
            messages1,
            TurnCommitStatus::Completed,
            None,
            None,
        );
        let commit1 = store.commit_turn(input1).expect("first commit");

        // Checkout the commit ID to enter detached HEAD.
        let head = store
            .checkout("sess-1", &commit1.commit_id)
            .expect("checkout to detached");
        assert!(
            matches!(head, GraphHead::Detached { .. }),
            "should be detached HEAD"
        );

        // head_transcript should still return the messages from the detached commit.
        let transcript = store.head_transcript("sess-1").expect("head_transcript");

        assert_eq!(
            transcript.parent_commit_id,
            Some(commit1.commit_id.clone()),
            "transcript parent should be the detached commit"
        );
        assert_eq!(transcript.messages.len(), 2);
        assert_eq!(transcript.messages[0].content, "Hello");
        assert_eq!(transcript.messages[0].role, "user");
        assert_eq!(transcript.messages[1].content, "Hi there!");
        assert_eq!(transcript.messages[1].role, "assistant");
    }
}

// Pointer operations tests

#[cfg(test)]
mod session_graph_pointer_ops {
    use super::*;
    use crate::graph::{derive_commit_message, GraphMessage};
    use std::env;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir()
            .join("lattice-session-pointer-ops-tests")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn user_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "user".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn assistant_msg(text: &str) -> GraphMessage {
        GraphMessage {
            role: "assistant".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    fn tool_msg(text: &str, tool_call_id: &str, name: &str) -> GraphMessage {
        GraphMessage {
            role: "tool".to_string(),
            content: text.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some(tool_call_id.to_string()),
            name: Some(name.to_string()),
        }
    }

    fn commit_input(
        session_id: &str,
        parent_ids: Vec<String>,
        messages: Vec<GraphMessage>,
        status: TurnCommitStatus,
    ) -> TurnCommitInput {
        TurnCommitInput {
            session_id: session_id.to_string(),
            parent_ids,
            messages: messages.clone(),
            profile_snapshot_id: Some("test-profile".to_string()),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            status,
            commit_message: derive_commit_message(&messages),
            usage: None,
            terminal_reason: None,
        }
    }

    /// Helper: create a session and commit N turns, returning commit IDs.
    fn setup_session_with_commits(
        store: &SqliteSessionGraphStore,
        session_id: &str,
        n: usize,
        label: Option<&str>,
    ) -> Vec<TurnCommit> {
        store
            .create_session(session_id, label, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        let mut commits: Vec<TurnCommit> = Vec::new();
        for i in 0..n {
            let parent_ids = if i == 0 {
                vec![]
            } else {
                vec![commits.last().unwrap().commit_id.clone()]
            };
            let messages = vec![
                user_msg(&format!("{session_id} turn {i}")),
                assistant_msg(&format!("{session_id} response {i}")),
            ];
            let input = TurnCommitInput {
                session_id: session_id.to_string(),
                parent_ids,
                messages: messages.clone(),
                profile_snapshot_id: Some("test-profile".to_string()),
                model: "sonnet".to_string(),
                provider: "anthropic".to_string(),
                status: TurnCommitStatus::Completed,
                commit_message: format!("Turn {i}"),
                usage: None,
                terminal_reason: None,
            };
            let commit = store.commit_turn(input).expect("commit_turn");
            commits.push(commit);
        }
        commits
    }

    #[test]
    fn status_reports_pointer_state() {
        let dir = temp_dir("status_pointer");
        let store = SqliteSessionGraphStore::new(dir);

        let commits = setup_session_with_commits(&store, "sess-1", 3, Some("test session"));

        let status = store.status("sess-1").expect("status");

        // Session ID matches.
        assert_eq!(status.session_id, "sess-1");

        // HEAD is on the "main" branch.
        match &status.head {
            GraphHead::Branch { name } => assert_eq!(name, "main"),
            other => panic!("expected Branch HEAD, got {other:?}"),
        }

        // Current commit ID is the last commit.
        assert_eq!(
            status.current_commit_id,
            Some(commits.last().unwrap().commit_id.clone())
        );

        // Branch refs include at least "main".
        assert!(
            status.branches.iter().any(|r| r.name == "main"),
            "should have main branch ref, got {:?}",
            status.branches
        );

        // Commit count is 3.
        assert_eq!(status.commit_count, 3);
    }

    #[test]
    fn list_sessions_reports_head_commit_and_commit_count() {
        let dir = temp_dir("list_sessions");
        let store = SqliteSessionGraphStore::new(dir);

        let commits_a = setup_session_with_commits(&store, "sess-a", 2, Some("Session A"));
        setup_session_with_commits(&store, "sess-b", 1, Some("Session B"));

        let list = store.list().expect("list");

        // Should have at least 2 sessions.
        assert!(
            list.len() >= 2,
            "expected at least 2 sessions, got {}",
            list.len()
        );

        let a = list
            .iter()
            .find(|s| s.session_id == "sess-a")
            .expect("sess-a");
        assert_eq!(a.label, Some("Session A".to_string()));
        assert_eq!(a.current_branch, Some("main".to_string()));
        assert_eq!(
            a.head_commit_id,
            Some(commits_a.last().unwrap().commit_id.clone())
        );
        assert_eq!(a.commit_count, 2);

        let b = list
            .iter()
            .find(|s| s.session_id == "sess-b")
            .expect("sess-b");
        assert_eq!(b.label, Some("Session B".to_string()));
        assert_eq!(b.commit_count, 1);
    }

    #[test]
    fn log_is_one_entry_per_turn_commit() {
        let dir = temp_dir("log_entries");
        let store = SqliteSessionGraphStore::new(dir);

        // Create session with tool messages too.
        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // First turn: user + assistant.
        let messages1 = vec![user_msg("Hello"), assistant_msg("Hi!")];
        let input1 = commit_input("sess-1", vec![], messages1, TurnCommitStatus::Completed);
        let commit1 = store.commit_turn(input1).expect("first commit");

        // Second turn: user + assistant + tool message.
        let messages2 = vec![
            user_msg("run ls"),
            assistant_msg("calling tool"),
            tool_msg("file1.txt", "call_1", "shell"),
        ];
        let input2 = commit_input(
            "sess-1",
            vec![commit1.commit_id.clone()],
            messages2,
            TurnCommitStatus::Completed,
        );
        let commit2 = store.commit_turn(input2).expect("second commit");

        let log = store.log("sess-1").expect("log");

        // Two committed turns -> two log entries.
        assert_eq!(log.len(), 2, "expected 2 log entries, got {}", log.len());

        // First entry.
        let entry1 = &log[0];
        assert_eq!(entry1.commit_id, commit1.commit_id);
        assert!(entry1.parent_ids.is_empty());
        assert_eq!(entry1.status, TurnCommitStatus::Completed);
        assert_eq!(entry1.model, "test-model");
        assert_eq!(entry1.provider, "test-provider");
        assert_eq!(entry1.message_count, 2);
        assert_eq!(entry1.tool_message_count, 0);

        // Second entry.
        let entry2 = &log[1];
        assert_eq!(entry2.commit_id, commit2.commit_id);
        assert_eq!(entry2.parent_ids, vec![commit1.commit_id]);
        assert_eq!(entry2.status, TurnCommitStatus::Completed);
        assert_eq!(entry2.message_count, 3);
        assert_eq!(entry2.tool_message_count, 1);
    }

    #[test]
    fn branch_checkout_and_reset_do_not_create_commits() {
        let dir = temp_dir("pointer_no_commit");
        let store = SqliteSessionGraphStore::new(dir);

        let commits = setup_session_with_commits(&store, "sess-1", 2, None);
        let first_commit_id = commits[0].commit_id.clone();

        // Record commit count before pointer operations.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let count_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
                [],
                |row| row.get(0),
            )
            .expect("count");

        // Create a branch from first commit.
        store
            .branch("sess-1", "feature", Some(&first_commit_id))
            .expect("branch");

        // Checkout the feature branch.
        store.checkout("sess-1", "feature").expect("checkout");

        // Reset main to first commit.
        store.reset("sess-1", &first_commit_id).expect("reset");

        // Checkout back to main.
        store.checkout("sess-1", "main").expect("checkout");

        // Count must be unchanged.
        let count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
                [],
                |row| row.get(0),
            )
            .expect("count");

        assert_eq!(
            count_before, count_after,
            "pointer operations must not create commits"
        );
    }

    #[test]
    fn branch_refs_point_to_commit_ids() {
        let dir = temp_dir("branch_commit_ids");
        let store = SqliteSessionGraphStore::new(dir);

        let commits = setup_session_with_commits(&store, "sess-1", 2, None);
        let first_commit_id = commits[0].commit_id.clone();

        // Create a branch from first commit.
        let branch_ref = store
            .branch("sess-1", "feature", Some(&first_commit_id))
            .expect("branch");

        // The branch ref must point to a commit ID.
        assert_eq!(
            branch_ref.commit_id,
            Some(first_commit_id.clone()),
            "branch ref must store commit ID, not session ID"
        );

        // Verify in the database directly.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let stored_commit_id: Option<String> = conn
            .query_row(
                "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='feature'",
                [],
                |row| row.get(0),
            )
            .expect("query");

        assert_eq!(stored_commit_id, Some(first_commit_id));
    }

    #[test]
    fn reset_returns_old_head_and_preserves_commits() {
        let dir = temp_dir("reset_preserves");
        let store = SqliteSessionGraphStore::new(dir);

        let commits = setup_session_with_commits(&store, "sess-1", 3, None);
        let first_id = commits[0].commit_id.clone();
        let third_id = commits[2].commit_id.clone();

        // Reset to first commit.
        let result = store.reset("sess-1", &first_id).expect("reset");

        // Old HEAD was the third commit.
        assert_eq!(result.old_commit_id, Some(third_id.clone()));
        // New HEAD is the first commit.
        assert_eq!(result.new_commit_id, first_id);
        // Branch is main.
        assert_eq!(result.branch, "main");

        // All commits are still accessible.
        let log = store.log("sess-1").expect("log");
        assert_eq!(log.len(), 3, "all 3 commits must remain after reset");
        assert!(log.iter().any(|e| e.commit_id == third_id));
    }

    #[test]
    fn diff_reports_unique_commit_paths() {
        let dir = temp_dir("diff_paths");
        let store = SqliteSessionGraphStore::new(dir);

        // Create session with 2 commits on main.
        let commits = setup_session_with_commits(&store, "sess-1", 2, None);
        let base_id = commits[0].commit_id.clone();
        let main_head_id = commits[1].commit_id.clone();

        // Create a feature branch from the base commit.
        store
            .branch("sess-1", "feature", Some(&base_id))
            .expect("branch");

        // Checkout feature, commit one more turn on it.
        store.checkout("sess-1", "feature").expect("checkout");
        let feature_messages = vec![user_msg("Feature work"), assistant_msg("Done")];
        let feature_input = TurnCommitInput {
            session_id: "sess-1".to_string(),
            parent_ids: vec![base_id.clone()],
            messages: feature_messages.clone(),
            profile_snapshot_id: Some("test-profile".to_string()),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            commit_message: derive_commit_message(&feature_messages),
            usage: None,
            terminal_reason: None,
        };
        let feature_commit = store.commit_turn(feature_input).expect("commit feature");

        // Diff between main head and feature head.
        let diff = store
            .diff("sess-1", &main_head_id, &feature_commit.commit_id)
            .expect("diff");

        // Common ancestor should be the base commit.
        assert_eq!(
            diff.common_ancestor_id,
            Some(base_id.clone()),
            "common ancestor should be the base commit"
        );

        // Commits only in "from" (main): the second commit on main.
        assert_eq!(diff.only_in_from.len(), 1);
        assert_eq!(diff.only_in_from[0].commit_id, main_head_id);

        // Commits only in "to" (feature): the feature commit.
        assert_eq!(diff.only_in_to.len(), 1);
        assert_eq!(diff.only_in_to[0].commit_id, feature_commit.commit_id);

        // Verify entries have status and commit_message.
        assert_eq!(diff.only_in_from[0].status, TurnCommitStatus::Completed);
        assert!(!diff.only_in_from[0].commit_message.is_empty());
        assert_eq!(diff.only_in_to[0].status, TurnCommitStatus::Completed);
        assert!(!diff.only_in_to[0].commit_message.is_empty());
    }

    #[test]
    fn checkout_existing_empty_branch_succeeds() {
        let dir = temp_dir("checkout_empty_branch");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // main branch was created by create_session with NULL commit_id.
        // Checkout should succeed even though the branch has no commits.
        let head = store.checkout("sess-1", "main").expect("checkout main");
        assert!(
            matches!(head, GraphHead::Branch { name } if name == "main"),
            "should return Branch with name 'main'"
        );

        // Status should still show branch main with no commit.
        let status = store.status("sess-1").expect("status");
        assert!(
            matches!(status.head, GraphHead::Branch { name } if name == "main"),
            "status HEAD should be Branch main"
        );
        assert!(
            status.current_commit_id.is_none(),
            "current_commit_id should be None for empty branch"
        );
    }

    #[test]
    fn reset_rejects_detached_head() {
        let dir = temp_dir("reset_detached");
        let store = SqliteSessionGraphStore::new(dir);

        let commits = setup_session_with_commits(&store, "sess-1", 2, None);
        let first_id = commits[0].commit_id.clone();
        let second_id = commits[1].commit_id.clone();

        // Checkout first commit to enter detached HEAD.
        store
            .checkout("sess-1", &first_id)
            .expect("checkout detached");

        // Reset should be rejected while detached.
        let result = store.reset("sess-1", &second_id);
        assert!(result.is_err(), "reset should fail while detached");
        match result {
            Err(SessionError::Conflict(msg)) => {
                assert!(
                    msg.contains("detached"),
                    "error should mention detached HEAD: {msg}"
                );
            }
            other => panic!("expected Conflict error, got {other:?}"),
        }

        // Verify main branch was NOT moved — still points to second_id.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let main_commit: Option<String> = conn
            .query_row(
                "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='main'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(
            main_commit,
            Some(second_id.clone()),
            "main branch should still point to second commit"
        );
    }

    #[test]
    fn commit_turn_while_detached_does_not_move_main_branch() {
        let dir = temp_dir("commit_detached");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // Commit A on main.
        let messages_a = vec![user_msg("A"), assistant_msg("Response A")];
        let input_a = commit_input("sess-1", vec![], messages_a, TurnCommitStatus::Completed);
        let commit_a = store.commit_turn(input_a).expect("commit A");

        // Checkout A to enter detached HEAD.
        store
            .checkout("sess-1", &commit_a.commit_id)
            .expect("checkout detached");

        // Commit B with parent A (while detached).
        let messages_b = vec![user_msg("B"), assistant_msg("Response B")];
        let input_b = commit_input(
            "sess-1",
            vec![commit_a.commit_id.clone()],
            messages_b,
            TurnCommitStatus::Completed,
        );
        let commit_b = store.commit_turn(input_b).expect("commit B while detached");

        // Status should show detached at B.
        let status = store.status("sess-1").expect("status");
        assert!(
            matches!(&status.head, GraphHead::Detached { commit_id } if *commit_id == commit_b.commit_id),
            "HEAD should be detached at commit B, got {:?}",
            status.head
        );

        // main branch should still point to A.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let main_commit: Option<String> = conn
            .query_row(
                "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='main'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(
            main_commit,
            Some(commit_a.commit_id.clone()),
            "main branch should still point to commit A"
        );

        // head_transcript should include messages from both A and B.
        let transcript = store.head_transcript("sess-1").expect("head_transcript");
        assert_eq!(transcript.messages.len(), 4, "should have A+B messages");
        assert_eq!(transcript.messages[0].content, "A");
        assert_eq!(transcript.messages[1].content, "Response A");
        assert_eq!(transcript.messages[2].content, "B");
        assert_eq!(transcript.messages[3].content, "Response B");
        assert_eq!(
            transcript.parent_commit_id,
            Some(commit_b.commit_id),
            "transcript parent should be commit B"
        );
    }

    #[test]
    fn branch_from_detached_head_uses_detached_commit() {
        let dir = temp_dir("branch_from_detached");
        let store = SqliteSessionGraphStore::new(dir);

        let commits = setup_session_with_commits(&store, "sess-1", 2, None);
        let first_id = commits[0].commit_id.clone();

        // Checkout first commit to enter detached HEAD.
        store
            .checkout("sess-1", &first_id)
            .expect("checkout detached");

        // Create branch from detached HEAD (from=None should use current HEAD).
        let branch_ref = store
            .branch("sess-1", "detached-copy", None)
            .expect("branch from detached");

        assert_eq!(
            branch_ref.commit_id,
            Some(first_id.clone()),
            "branch should point to the detached commit"
        );
    }

    #[test]
    fn commit_turn_allows_repeated_identical_user_message_across_turns() {
        let dir = temp_dir("repeated_messages");
        let store = SqliteSessionGraphStore::new(dir);

        store
            .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
            .expect("create");

        // First turn with user "hello" and assistant "hi".
        let messages1 = vec![user_msg("hello"), assistant_msg("hi")];
        let input1 = commit_input("sess-1", vec![], messages1, TurnCommitStatus::Completed);
        let commit1 = store.commit_turn(input1).expect("first commit");

        // Second turn with identical user "hello" and assistant "hi".
        let messages2 = vec![user_msg("hello"), assistant_msg("hi")];
        let input2 = commit_input(
            "sess-1",
            vec![commit1.commit_id.clone()],
            messages2,
            TurnCommitStatus::Completed,
        );
        let commit2 = store.commit_turn(input2).expect("second commit");

        // Both commits should succeed.
        assert_ne!(
            commit1.commit_id, commit2.commit_id,
            "commit IDs should differ"
        );

        // Message IDs should differ between the two commits.
        assert_ne!(
            commit1.message_ids, commit2.message_ids,
            "message IDs should differ across turns even for identical content"
        );

        // Verify all messages exist in the database.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let msg_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM graph_messages WHERE session_id='sess-1'",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(msg_count, 4, "should have 4 message rows total");
    }

    /// When `branch()` updates the currently checked-out branch, `graph_sessions.head_commit_id`
    /// must be kept in sync. Otherwise `status()` returns a stale `current_commit_id`.
    #[test]
    fn branch_update_current_branch_syncs_session_head_commit_id() {
        let dir = temp_dir("branch_syncs_head");
        let store = SqliteSessionGraphStore::new(dir);

        // 1. Create session and commit A, then commit B on main.
        let commits = setup_session_with_commits(&store, "sess-1", 2, None);
        let commit_a = commits[0].commit_id.clone();
        let commit_b = commits[1].commit_id.clone();

        // Sanity: status shows HEAD on main at commit B.
        let status = store.status("sess-1").expect("status");
        assert!(
            matches!(&status.head, GraphHead::Branch { name } if name == "main"),
            "HEAD should be on main"
        );
        assert_eq!(status.current_commit_id, Some(commit_b.clone()));

        // 2. Call branch() to move main back to commit A.
        store
            .branch("sess-1", "main", Some(&commit_a))
            .expect("branch update");

        // 3. Status must now report commit A as current.
        let status = store.status("sess-1").expect("status after branch");
        assert!(
            matches!(&status.head, GraphHead::Branch { name } if name == "main"),
            "HEAD should still be on main"
        );
        assert_eq!(
            status.current_commit_id,
            Some(commit_a.clone()),
            "status.current_commit_id must be commit A after branch moved main"
        );

        // 4. Verify main branch ref points to commit A.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let main_commit: Option<String> = conn
            .query_row(
                "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='main'",
                [],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(main_commit, Some(commit_a.clone()));

        // 5. head_transcript must also reflect the rewind (via branch ref read).
        let transcript = store.head_transcript("sess-1").expect("head_transcript");
        assert_eq!(
            transcript.messages.len(),
            2,
            "only commit A's messages visible"
        );
        assert_eq!(
            transcript.parent_commit_id,
            Some(commit_a),
            "transcript parent should be commit A"
        );
    }

    /// Updating a non-current branch via `branch()` must NOT move the session HEAD.
    #[test]
    fn branch_update_non_current_branch_does_not_move_head() {
        let dir = temp_dir("branch_non_current");
        let store = SqliteSessionGraphStore::new(dir);

        // 1. Create session, commit A, branch 'side' from A, commit B on main.
        let commits = setup_session_with_commits(&store, "sess-1", 1, None);
        let commit_a = commits[0].commit_id.clone();

        store
            .branch("sess-1", "side", Some(&commit_a))
            .expect("create side branch");

        // Commit B on main.
        let messages_b = vec![user_msg("B"), assistant_msg("Response B")];
        let input_b = commit_input(
            "sess-1",
            vec![commit_a.clone()],
            messages_b,
            TurnCommitStatus::Completed,
        );
        let commit_b = store.commit_turn(input_b).expect("commit B");

        // HEAD is still on main at B.
        let status = store.status("sess-1").expect("status");
        assert_eq!(status.current_commit_id, Some(commit_b.commit_id.clone()));
        let commit_b_id = commit_b.commit_id;

        // 2. Update 'side' branch to point to B.
        store
            .branch("sess-1", "side", Some(&commit_b_id))
            .expect("update side branch");

        // 3. HEAD must still be on main at B — not moved by side branch update.
        let status = store.status("sess-1").expect("status after side update");
        assert!(
            matches!(&status.head, GraphHead::Branch { name } if name == "main"),
            "HEAD should still be on main"
        );
        assert_eq!(
            status.current_commit_id,
            Some(commit_b_id.clone()),
            "HEAD commit should still be B"
        );

        // 4. 'side' branch ref should point to B.
        let conn = Connection::open(store.db_path()).expect("raw conn");
        let side_commit: Option<String> = conn
                        .query_row(
                            "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='side'",
                            [],
                            |row| row.get(0),
                        )
                        .expect("query");
        assert_eq!(side_commit, Some(commit_b_id));
    }
}
