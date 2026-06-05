// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared SQL helpers used across the SqliteSessionGraphStore impls.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, OptionalExtension};

use crate::graph::{GraphMessage, GraphToolCall, TurnCommitStatus, TurnLogEntry};
use crate::{Result, SessionError};

use super::SqliteSessionGraphStore;

impl SqliteSessionGraphStore {
    /// Resolve the current HEAD commit ID.
    ///
    /// For attached HEAD (branch), reads the branch ref's commit_id.
    /// For detached HEAD, reads graph_sessions.head_commit_id directly.
    pub(super) fn current_head_commit_id(
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
    pub(super) fn parse_json_column<T>(&self, column: &str, raw: &str) -> Result<T>
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

    pub(super) fn current_branch_name(
        &self,
        conn: &Connection,
        session_id: &str,
    ) -> Result<Option<String>> {
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

    pub(super) fn resolve_target_to_commit_id(
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

    pub(super) fn commit_exists_in_session(
        &self,
        conn: &Connection,
        session_id: &str,
        commit_id: &str,
    ) -> Result<bool> {
        conn.query_row(
            "SELECT COUNT(*) FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
            params![commit_id, session_id],
            |row| Ok(row.get::<_, i64>(0)? > 0),
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })
    }

    pub(super) fn get_parent_ids(
        &self,
        conn: &Connection,
        session_id: &str,
        commit_id: &str,
    ) -> Result<Vec<String>> {
        let parents_json: String = conn
            .query_row(
                "SELECT parents_json FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                params![commit_id, session_id],
                |row| row.get(0),
            )
            .map_err(|_| SessionError::MissingCommit(commit_id.to_string()))?;

        self.parse_json_column("parents_json", &parents_json)
    }

    fn graph_cycle_error(&self, commit_id: &str) -> SessionError {
        SessionError::Database {
            path: self.db_path.clone(),
            source: rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("cycle detected in commit graph at {commit_id}"),
                )),
            ),
        }
    }

    /// Collect all same-session commits reachable from `head_commit_id` in stable
    /// topological order (every parent before its child).
    ///
    /// Parent visit order follows `parents_json` array order: `parent_ids[0]`
    /// (first parent / mainline) is traversed before additional merge parents.
    /// Duplicate commits (e.g. diamond merge common ancestor) appear once.
    pub(super) fn collect_reachable_commits_topological(
        &self,
        conn: &Connection,
        session_id: &str,
        head_commit_id: &str,
    ) -> Result<Vec<String>> {
        enum Frame {
            Enter(String),
            Exit(String),
        }

        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        let mut ordered = Vec::new();
        let mut stack = vec![Frame::Enter(head_commit_id.to_string())];

        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Enter(cid) => {
                    if visited.contains(&cid) {
                        continue;
                    }
                    if !visiting.insert(cid.clone()) {
                        return Err(self.graph_cycle_error(&cid));
                    }
                    stack.push(Frame::Exit(cid.clone()));
                    let parents = self.get_parent_ids(conn, session_id, &cid)?;
                    for parent in parents.into_iter().rev() {
                        if !visited.contains(&parent) {
                            stack.push(Frame::Enter(parent));
                        }
                    }
                }
                Frame::Exit(cid) => {
                    visiting.remove(&cid);
                    if visited.insert(cid.clone()) {
                        ordered.push(cid);
                    }
                }
            }
        }

        Ok(ordered)
    }

    /// Load `message_ids_json` for many commits, chunking the `IN` clause
    /// to stay within SQLite's bind-parameter limit.
    pub(super) fn commit_message_ids_batch(
        &self,
        conn: &Connection,
        session_id: &str,
        commit_ids: &[String],
    ) -> Result<HashMap<String, Vec<String>>> {
        if commit_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut out = HashMap::with_capacity(commit_ids.len());
        for chunk in commit_ids.chunks(super::SQLITE_BIND_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT commit_id, message_ids_json FROM graph_commits WHERE session_id=? AND commit_id IN ({placeholders})"
            );
            let mut param_refs: Vec<&dyn rusqlite::types::ToSql> =
                Vec::with_capacity(chunk.len() + 1);
            param_refs.push(&session_id);
            param_refs.extend(chunk.iter().map(|s| s as &dyn rusqlite::types::ToSql));

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;

            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;

            for row in rows {
                let (commit_id, message_ids_json) =
                    row.map_err(|source| SessionError::Database {
                        path: self.db_path.clone(),
                        source,
                    })?;
                let message_ids: Vec<String> =
                    self.parse_json_column("message_ids_json", &message_ids_json)?;
                out.insert(commit_id, message_ids);
            }
        }
        Ok(out)
    }

    /// Load graph messages by ID, chunking the `IN` clause to stay within
    /// SQLite's bind-parameter limit.
    pub(super) fn load_graph_messages_by_id(
        &self,
        conn: &Connection,
        session_id: &str,
        message_ids: &[&str],
    ) -> Result<HashMap<String, GraphMessage>> {
        if message_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut out = HashMap::with_capacity(message_ids.len());
        for chunk in message_ids.chunks(super::SQLITE_BIND_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT message_id, role, content, reasoning_content, tool_call_id, name, tool_calls_json
                 FROM graph_messages WHERE session_id=? AND message_id IN ({placeholders})"
            );
            let param_values: Vec<String> = chunk.iter().map(|s| (*s).to_string()).collect();
            let mut param_refs: Vec<&dyn rusqlite::types::ToSql> =
                Vec::with_capacity(param_values.len() + 1);
            param_refs.push(&session_id);
            param_refs.extend(
                param_values
                    .iter()
                    .map(|s| s as &dyn rusqlite::types::ToSql),
            );

            let mut stmt = conn
                .prepare(&sql)
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;

            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    let message_id: String = row.get(0)?;
                    let role: String = row.get(1)?;
                    let content: String = row.get(2)?;
                    let reasoning_content: Option<String> = row.get(3)?;
                    let tool_call_id: Option<String> = row.get(4)?;
                    let name: Option<String> = row.get(5)?;
                    let tool_calls_json: String = row.get(6)?;
                    Ok((
                        message_id,
                        role,
                        content,
                        reasoning_content,
                        tool_call_id,
                        name,
                        tool_calls_json,
                    ))
                })
                .map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;

            for row in rows {
                let (
                    message_id,
                    role,
                    content,
                    reasoning_content,
                    tool_call_id,
                    name,
                    tool_calls_json,
                ) = row.map_err(|source| SessionError::Database {
                    path: self.db_path.clone(),
                    source,
                })?;
                let tool_calls: Option<Vec<GraphToolCall>> = serde_json::from_str(&tool_calls_json)
                    .ok()
                    .filter(|v: &Vec<GraphToolCall>| !v.is_empty());
                out.insert(
                    message_id,
                    GraphMessage {
                        role,
                        content,
                        reasoning_content,
                        tool_calls,
                        tool_call_id,
                        name,
                    },
                );
            }
        }
        Ok(out)
    }

    pub(super) fn collect_ancestors(
        &self,
        conn: &Connection,
        session_id: &str,
        start: &str,
    ) -> Result<std::collections::HashSet<String>> {
        let mut visited = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(start.to_string());
        visited.insert(start.to_string());

        while let Some(current) = queue.pop_front() {
            let parents = self.get_parent_ids(conn, session_id, &current)?;
            for parent in parents {
                if visited.insert(parent.clone()) {
                    queue.push_back(parent);
                }
            }
        }

        Ok(visited)
    }

    pub(super) fn collect_unique_entries(
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
                let (
                    parents_json,
                    status_str,
                    agent_name,
                    profile_snapshot_hash,
                    model,
                    provider,
                    commit_message,
                    created_at,
                    message_ids_json,
                ): (
                    String, String, String, String, String, String, String, String, String,
                ) = conn
                    .query_row(
                        "SELECT parents_json, status, agent_name, profile_snapshot_hash, model, provider, commit_message, created_at, message_ids_json
                         FROM graph_commits WHERE commit_id=?1 AND session_id=?2",
                        params![cid, session_id],
                        |row| Ok((
                            row.get(0)?, row.get(1)?, row.get(2)?,
                            row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?,
                            row.get(7)?, row.get(8)?,
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
                    agent_name,
                    profile_snapshot_hash,
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
