// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Schema initialization, PRAGMAs, and idempotent ALTER TABLE migrations.

use rusqlite::Connection;

use crate::{Result, SessionError};

use super::SqliteSessionGraphStore;

impl SqliteSessionGraphStore {
    /// Initialize schema, PRAGMAs, and idempotent ALTER TABLE migrations.
    pub(super) fn init_schema(&self, conn: &Connection) -> Result<()> {
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
                active_agent_name TEXT NOT NULL,
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
                active_agent_name TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (session_id, kind, name),
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );

            CREATE TABLE IF NOT EXISTS graph_commits (
                commit_id            TEXT PRIMARY KEY,
                session_id           TEXT NOT NULL,
                parents_json         TEXT NOT NULL,
                message_ids_json     TEXT NOT NULL,
                agent_name           TEXT NOT NULL,
                profile_snapshot_hash TEXT NOT NULL,
                profile_snapshot_json TEXT NOT NULL,
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
