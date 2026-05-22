//! Commit creation, log read, and head transcript reconstruction.

use chrono::Utc;
use rusqlite::{params, Connection};

use crate::graph::{
    canonical_message_id, canonical_turn_commit_id, canonical_turn_seed, GraphMessage,
    GraphToolCall, HeadTranscript, TurnCommit, TurnCommitInput, TurnCommitStatus, TurnLogEntry,
    TurnUsage,
};
use crate::{Result, SessionError};

use super::SqliteSessionGraphStore;

impl SqliteSessionGraphStore {
    pub(super) fn head_transcript_impl(&self, session_id: &str) -> Result<HeadTranscript> {
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

    pub(super) fn log_impl(&self, session_id: &str) -> Result<Vec<TurnLogEntry>> {
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

    pub(super) fn commit_turn_outer(&self, input: TurnCommitInput) -> Result<TurnCommit> {
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
}
