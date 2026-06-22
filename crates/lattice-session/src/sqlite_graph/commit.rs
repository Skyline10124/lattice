// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Commit creation, log read, head transcript reconstruction, and prepared commit persistence.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};

use crate::graph::{
    GraphMessage, HeadTranscript, PreparedTurnCommit, TurnCommit, TurnCommitInput,
    TurnCommitStatus, TurnLifecycleEvidence, TurnLogEntry, TurnUsage, WorkspaceDisposition,
};
use crate::{Result, SessionError};

use super::SqliteSessionGraphStore;

/// Strict decode a persisted status string, failing closed on unknown values.
pub(super) fn decode_strict_status(raw: &str) -> Result<TurnCommitStatus> {
    TurnCommitStatus::parse(raw)
}

/// Strict decode a persisted disposition string, failing closed on unknown values.
pub(super) fn decode_strict_disposition(raw: &str) -> Result<WorkspaceDisposition> {
    WorkspaceDisposition::parse(raw)
}

/// Decode lifecycle JSON from a persisted string, failing closed on malformed data.
///
/// Returns `None` when the stored value is the empty object `{}` (no lifecycle evidence).
pub(super) fn decode_lifecycle_json(
    db_path: &std::path::Path,
    raw: &str,
) -> Result<Option<TurnLifecycleEvidence>> {
    if raw == "{}" {
        return Ok(None);
    }
    serde_json::from_str(raw).map_err(|err| SessionError::Database {
        path: db_path.to_path_buf(),
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("malformed JSON in lifecycle_json: {err}"),
            )),
        ),
    })
}

/// Encode lifecycle evidence to JSON for persistence.
pub(super) fn encode_lifecycle_json(lifecycle: &Option<TurnLifecycleEvidence>) -> String {
    match lifecycle {
        Some(lc) => serde_json::to_string(lc).unwrap_or_else(|_| "{}".to_string()),
        None => "{}".to_string(),
    }
}

impl SqliteSessionGraphStore {
    pub(super) fn head_transcript_impl(&self, session_id: &str) -> Result<HeadTranscript> {
        let conn = self.conn()?;
        let head_cid = self.current_head_commit_id(&conn, session_id)?;

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

        let commit_ids =
            self.collect_reachable_commits_topological(&conn, session_id, &parent_commit_id)?;
        let commit_message_ids = self.commit_message_ids_batch(&conn, session_id, &commit_ids)?;
        let mut ordered_message_ids: Vec<String> = Vec::new();
        for cid in &commit_ids {
            let message_ids = commit_message_ids
                .get(cid)
                .ok_or_else(|| SessionError::MissingCommit(cid.clone()))?;
            ordered_message_ids.extend(message_ids.iter().cloned());
        }

        let ordered_refs: Vec<&str> = ordered_message_ids.iter().map(String::as_str).collect();
        let messages_by_id = self.load_graph_messages_by_id(&conn, session_id, &ordered_refs)?;

        let mut all_messages: Vec<GraphMessage> = Vec::with_capacity(ordered_message_ids.len());
        for mid in &ordered_message_ids {
            let message = messages_by_id
                .get(mid)
                .ok_or_else(|| SessionError::Database {
                    path: self.db_path.clone(),
                    source: rusqlite::Error::QueryReturnedNoRows,
                })?;
            all_messages.push(message.clone());
        }

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
                "SELECT commit_id, parents_json, status, agent_name, profile_snapshot_hash, model, provider, commit_message, created_at, message_ids_json, usage_json, terminal_reason, disposition, lifecycle_json
                 FROM graph_commits WHERE session_id=?1 ORDER BY created_at ASC",
            )
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        struct RawRow {
            commit_id: String,
            parent_ids: Vec<String>,
            status: TurnCommitStatus,
            disposition: WorkspaceDisposition,
            agent_name: String,
            profile_snapshot_hash: String,
            model: String,
            provider: String,
            commit_message: String,
            created_at: String,
            message_ids: Vec<String>,
            usage: Option<TurnUsage>,
            terminal_reason: Option<String>,
        }

        let db_path = self.db_path.clone();
        let rows: Vec<RawRow> = stmt
            .query_map(params![session_id], |row| {
                let commit_id: String = row.get(0)?;
                let parents_json: String = row.get(1)?;
                let status_str: String = row.get(2)?;
                let agent_name: String = row.get(3)?;
                let profile_snapshot_hash: String = row.get(4)?;
                let model: String = row.get(5)?;
                let provider: String = row.get(6)?;
                let commit_message: String = row.get(7)?;
                let created_at: String = row.get(8)?;
                let message_ids_json: String = row.get(9)?;
                let usage_json: Option<String> = row.get(10)?;
                let terminal_reason: Option<String> = row.get(11)?;
                let disposition_str: String = row.get(12)?;
                let lifecycle_json_str: String = row.get(13)?;

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
                let status = decode_strict_status(&status_str).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("strict decode status: {e}"),
                        )),
                    )
                })?;
                let disposition = decode_strict_disposition(&disposition_str).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        12,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("strict decode disposition: {e}"),
                        )),
                    )
                })?;
                let lifecycle =
                    decode_lifecycle_json(&db_path, &lifecycle_json_str).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            13,
                            rusqlite::types::Type::Text,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("strict decode lifecycle_json: {e}"),
                            )),
                        )
                    })?;
                let _ = lifecycle;
                let usage: Option<TurnUsage> =
                    usage_json.and_then(|json| serde_json::from_str(&json).ok());

                Ok(RawRow {
                    commit_id,
                    parent_ids,
                    status,
                    disposition,
                    agent_name,
                    profile_snapshot_hash,
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

        let all_message_ids: Vec<&str> = rows
            .iter()
            .flat_map(|r| r.message_ids.iter().map(|s| s.as_str()))
            .collect();

        let mut tool_set: std::collections::HashSet<String> = std::collections::HashSet::new();
        for chunk in all_message_ids.chunks(super::SQLITE_BIND_CHUNK_SIZE) {
            let placeholders: String = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "SELECT message_id FROM graph_messages WHERE message_id IN ({placeholders}) AND role = 'tool'"
            );
            let params: Vec<Box<dyn rusqlite::types::ToSql>> = chunk
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
                    disposition: r.disposition,
                    agent_name: r.agent_name,
                    profile_snapshot_hash: r.profile_snapshot_hash,
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

    /// Commit a turn from raw input — convenience that prepares the commit and delegates.
    pub(super) fn commit_turn_outer(&self, input: TurnCommitInput) -> Result<TurnCommit> {
        let now = Utc::now().to_rfc3339();
        let prepared = crate::graph::prepare_turn_commit(&input, &now);
        self.commit_prepared_turn_outer(prepared)
    }

    /// Persist a precomputed turn commit with prepared identity (T-36-06).
    ///
    /// Wraps `commit_prepared_inner` in a `BEGIN IMMEDIATE` transaction so the
    /// HEAD CAS check and all writes are atomic.
    pub(super) fn commit_prepared_turn_outer(
        &self,
        prepared: PreparedTurnCommit,
    ) -> Result<TurnCommit> {
        let conn = self.conn()?;

        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        let result = self.commit_prepared_inner(&conn, &prepared);

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

    fn commit_prepared_inner(
        &self,
        conn: &Connection,
        prepared: &PreparedTurnCommit,
    ) -> Result<TurnCommit> {
        let commit = &prepared.commit;
        let now = &commit.created_at;

        // 1. Check if commit already exists (idempotent retry) — before CAS.
        let existing: Option<String> = conn
            .query_row(
                "SELECT status FROM graph_commits WHERE commit_id = ?1",
                params![commit.commit_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        if existing.is_some() {
            // Same-ID retry: return the prepared commit unchanged.
            // The prepared identity is immutable so the stored row is authoritative.
            return Ok(commit.clone());
        }

        // 2. Read current HEAD commit.
        let current_commit_id = self.current_head_commit_id(conn, &commit.session_id)?;

        // 3. Validate expected parent matches current HEAD (CAS).
        if commit.parent_ids.is_empty() {
            if let Some(ref head) = current_commit_id {
                return Err(SessionError::Conflict(format!(
                    "expected no parent (first commit) but HEAD is '{head}'"
                )));
            }
        } else {
            let expected_parent = &commit.parent_ids[0];
            match &current_commit_id {
                Some(head) if head == expected_parent => {}
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

        for parent_id in &commit.parent_ids {
            if !self.commit_exists_in_session(conn, &commit.session_id, parent_id)? {
                return Err(SessionError::MissingCommit(parent_id.clone()));
            }
        }

        // 4. Write message rows using prepared message IDs and carried messages.
        for (i, msg) in prepared.messages.iter().enumerate() {
            let message_id = &prepared.message_ids[i];
            let tool_calls_json = match &msg.tool_calls {
                Some(calls) => serde_json::to_string(calls).unwrap_or_else(|_| "[]".to_string()),
                None => "[]".to_string(),
            };
            conn.execute(
                "INSERT INTO graph_messages (message_id, session_id, role, content, reasoning_content, tool_call_id, name, tool_calls_json, created_at, ordinal)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    message_id, commit.session_id, msg.role, msg.content,
                    msg.reasoning_content, msg.tool_call_id, msg.name,
                    tool_calls_json, now, i as i64,
                ],
            )
            .map_err(|source| SessionError::Database { path: self.db_path.clone(), source })?;
        }

        // 5. Insert immutable commit row with disposition and lifecycle evidence.
        let parents_json =
            serde_json::to_string(&commit.parent_ids).unwrap_or_else(|_| "[]".to_string());
        let message_ids_json =
            serde_json::to_string(&commit.message_ids).unwrap_or_else(|_| "[]".to_string());
        let usage_json = commit
            .usage
            .as_ref()
            .map(|u| serde_json::to_string(u).unwrap_or_else(|_| "{}".to_string()));
        let lifecycle_json = encode_lifecycle_json(&commit.lifecycle);

        conn.execute(
            "INSERT INTO graph_commits (commit_id, session_id, parents_json, message_ids_json, agent_name, profile_snapshot_hash, profile_snapshot_json, model, provider, status, commit_message, usage_json, terminal_reason, disposition, lifecycle_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                commit.commit_id, commit.session_id, parents_json, message_ids_json,
                commit.agent_name, commit.profile_snapshot_hash, commit.profile_snapshot_json,
                commit.model, commit.provider, commit.status.as_str(), commit.commit_message,
                usage_json, commit.terminal_reason, commit.disposition.as_str(),
                lifecycle_json, now,
            ],
        )
        .map_err(|source| SessionError::Database { path: self.db_path.clone(), source })?;

        // 6. Update branch ref and session head.
        self.update_refs_after_commit(conn, commit)?;

        Ok(commit.clone())
    }

    /// Look up a single turn commit by session and commit ID.
    pub(super) fn get_turn_commit_impl(
        &self,
        session_id: &str,
        commit_id: &str,
    ) -> Result<Option<TurnCommit>> {
        let conn = self.conn()?;

        let row = conn
            .query_row(
                "SELECT commit_id, session_id, parents_json, message_ids_json, agent_name, profile_snapshot_hash, profile_snapshot_json, model, provider, status, commit_message, usage_json, terminal_reason, disposition, lifecycle_json, created_at
                 FROM graph_commits WHERE commit_id = ?1 AND session_id = ?2",
                params![commit_id, session_id],
                |row| {
                    let commit_id: String = row.get(0)?;
                    let session_id: String = row.get(1)?;
                    let parents_json: String = row.get(2)?;
                    let message_ids_json: String = row.get(3)?;
                    let agent_name: String = row.get(4)?;
                    let profile_snapshot_hash: String = row.get(5)?;
                    let profile_snapshot_json: String = row.get(6)?;
                    let model: String = row.get(7)?;
                    let provider: String = row.get(8)?;
                    let status_str: String = row.get(9)?;
                    let commit_message: String = row.get(10)?;
                    let usage_json: Option<String> = row.get(11)?;
                    let terminal_reason: Option<String> = row.get(12)?;
                    let disposition_str: String = row.get(13)?;
                    let lifecycle_json_str: String = row.get(14)?;
                    let created_at: String = row.get(15)?;
                    Ok((
                        commit_id, session_id, parents_json, message_ids_json, agent_name,
                        profile_snapshot_hash, profile_snapshot_json, model, provider,
                        status_str, commit_message, usage_json, terminal_reason,
                        disposition_str, lifecycle_json_str, created_at,
                    ))
                },
            )
            .optional()
            .map_err(|source| SessionError::Database { path: self.db_path.clone(), source })?;

        match row {
            None => Ok(None),
            Some((
                commit_id,
                session_id,
                parents_json,
                message_ids_json,
                agent_name,
                profile_snapshot_hash,
                profile_snapshot_json,
                model,
                provider,
                status_str,
                commit_message,
                usage_json,
                terminal_reason,
                disposition_str,
                lifecycle_json_str,
                created_at,
            )) => {
                let parent_ids = self.parse_json_column("parents_json", &parents_json)?;
                let message_ids = self.parse_json_column("message_ids_json", &message_ids_json)?;
                let status = decode_strict_status(&status_str)?;
                let disposition = decode_strict_disposition(&disposition_str)?;
                let lifecycle = decode_lifecycle_json(&self.db_path, &lifecycle_json_str)?;
                let usage: Option<TurnUsage> =
                    usage_json.and_then(|j| serde_json::from_str(&j).ok());

                Ok(Some(TurnCommit {
                    commit_id,
                    session_id,
                    parent_ids,
                    message_ids,
                    agent_name,
                    profile_snapshot_hash,
                    profile_snapshot_json,
                    model,
                    provider,
                    status,
                    disposition,
                    lifecycle,
                    commit_message,
                    usage,
                    terminal_reason,
                    created_at,
                }))
            }
        }
    }

    /// Set snapshot-lost operational state (T-36-07).
    pub(super) fn set_snapshot_lost_impl(&self, commit_id: &str, lost: bool) -> Result<()> {
        let conn = self.conn()?;
        let now = Utc::now().to_rfc3339();
        let lost_int = if lost { 1 } else { 0 };

        conn.execute(
            "INSERT INTO graph_snapshot_state (commit_id, snapshot_lost, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(commit_id) DO UPDATE SET snapshot_lost = ?2, updated_at = ?3",
            params![commit_id, lost_int, now],
        )
        .map_err(|source| SessionError::Database {
            path: self.db_path.clone(),
            source,
        })?;

        Ok(())
    }

    /// Get snapshot-lost operational state.
    pub(super) fn get_snapshot_lost_impl(&self, commit_id: &str) -> Result<Option<bool>> {
        let conn = self.conn()?;

        let result = conn
            .query_row(
                "SELECT snapshot_lost FROM graph_snapshot_state WHERE commit_id = ?1",
                params![commit_id],
                |row| row.get::<_, i32>(0),
            )
            .optional()
            .map_err(|source| SessionError::Database {
                path: self.db_path.clone(),
                source,
            })?;

        Ok(result.map(|v| v != 0))
    }

    /// Helper: update branch ref and session head after a commit.
    fn update_refs_after_commit(&self, conn: &Connection, commit: &TurnCommit) -> Result<()> {
        let now = &commit.created_at;
        match self.current_branch_name(conn, &commit.session_id)? {
            Some(branch_name) => {
                conn.execute(
                    "UPDATE graph_refs SET commit_id=?1, updated_at=?2 WHERE session_id=?3 AND kind='branch' AND name=?4",
                    params![commit.commit_id, now, commit.session_id, branch_name],
                )
                .map_err(|source| SessionError::Database { path: self.db_path.clone(), source })?;
                conn.execute(
                    "UPDATE graph_sessions SET head_commit_id=?1, updated_at=?2 WHERE session_id=?3",
                    params![commit.commit_id, now, commit.session_id],
                )
                .map_err(|source| SessionError::Database { path: self.db_path.clone(), source })?;
            }
            None => {
                conn.execute(
                    "UPDATE graph_sessions SET head_commit_id=?1, updated_at=?2 WHERE session_id=?3",
                    params![commit.commit_id, now, commit.session_id],
                )
                .map_err(|source| SessionError::Database { path: self.db_path.clone(), source })?;
            }
        }
        Ok(())
    }
}
