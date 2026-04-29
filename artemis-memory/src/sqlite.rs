use crate::{ConversationTurn, EntryKind, Memory, MemoryEntry};
use async_trait::async_trait;
use rusqlite::{params, Connection};
use std::sync::Mutex;

/// SQLite-backed persistent memory with FTS5 full-text search.
///
/// Interior mutability via `Mutex<Connection>` makes this type `Sync`.
pub struct SqliteMemory {
    conn: Mutex<Connection>,
}

impl SqliteMemory {
    /// Open (or create) a memory database at the given path.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS memory (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                session_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                summary, content, tags_str,
                content='memory', content_rowid='rowid'
            );
            ",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Fallback LIKE-based recall when FTS5 MATCH fails.
    fn recall_like(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let pattern = format!("%{}%", query);
        let sql = "SELECT id, kind, session_id, summary, content, tags, created_at
                   FROM memory
                   WHERE summary LIKE ?1 OR content LIKE ?1
                   LIMIT ?2";
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match stmt.query_map(params![pattern, limit as i64], Self::row_to_entry) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    fn row_to_entry(row: &rusqlite::Row) -> rusqlite::Result<MemoryEntry> {
        let tags_str: String = row.get(5)?;
        let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
        let kind_str: String = row.get(1)?;
        let kind = match kind_str.as_str() {
            "session_log" => EntryKind::SessionLog,
            "fact" => EntryKind::Fact,
            "decision" => EntryKind::Decision,
            "project_context" => EntryKind::ProjectContext,
            _ => EntryKind::Fact,
        };
        Ok(MemoryEntry {
            id: row.get(0)?,
            kind,
            session_id: row.get(2)?,
            summary: row.get(3)?,
            content: row.get(4)?,
            tags,
            created_at: row.get(6)?,
        })
    }
}

#[async_trait]
impl Memory for SqliteMemory {
    async fn save_entry(&self, entry: MemoryEntry) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return,
        };
        let tags_json = serde_json::to_string(&entry.tags).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "INSERT OR REPLACE INTO memory (id, kind, session_id, summary, content, tags, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.id,
                entry.kind_str(),
                entry.session_id,
                entry.summary,
                entry.content,
                tags_json,
                entry.created_at
            ],
        )
        .ok();
        // Sync FTS index
        conn.execute(
            "INSERT INTO memory_fts(rowid, summary, content, tags_str)
             SELECT rowid, summary, content, tags FROM memory WHERE id = ?1",
            params![entry.id],
        )
        .ok();
    }

    async fn recall(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        // Try FTS5 MATCH first
        let sql = "SELECT m.id, m.kind, m.session_id, m.summary, m.content, m.tags, m.created_at
                   FROM memory m
                   WHERE m.rowid IN (
                       SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1
                   )
                   LIMIT ?2";
        if let Ok(mut stmt) = conn.prepare(sql) {
            if let Ok(rows) = stmt.query_map(params![query, limit as i64], Self::row_to_entry) {
                let results: Vec<_> = rows.filter_map(|r| r.ok()).collect();
                if !results.is_empty() {
                    return results;
                }
            }
        }
        // Fallback: LIKE search
        drop(conn);
        self.recall_like(query, limit)
    }

    async fn entries_by_kind(&self, kind: &EntryKind, limit: usize) -> Vec<MemoryEntry> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return vec![],
        };
        let kind_str = match kind {
            EntryKind::SessionLog => "session_log",
            EntryKind::Fact => "fact",
            EntryKind::Decision => "decision",
            EntryKind::ProjectContext => "project_context",
        };
        let sql = "SELECT id, kind, session_id, summary, content, tags, created_at
                   FROM memory WHERE kind = ?1 LIMIT ?2";
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match stmt.query_map(params![kind_str, limit as i64], Self::row_to_entry) {
            Ok(r) => r,
            Err(_) => return vec![],
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    fn reflect(&self, _session_log: &[ConversationTurn]) -> Vec<String> {
        // No LLM extraction in this backend.
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryKind, MemoryEntry};

    #[test]
    fn test_sqlite_save_and_recall() {
        let path = ":memory:";
        let mem = SqliteMemory::open(path).unwrap();
        futures::executor::block_on(mem.save_entry(MemoryEntry {
            id: "1".into(),
            kind: EntryKind::Fact,
            session_id: "s1".into(),
            summary: "Project uses Rust".into(),
            content: "artemis is written in Rust".into(),
            tags: vec!["project".into()],
            created_at: "2026-04-29".into(),
        }));
        let results = futures::executor::block_on(mem.recall("Rust", 10));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "Project uses Rust");
    }

    #[test]
    fn test_sqlite_entries_by_kind() {
        let path = ":memory:";
        let mem = SqliteMemory::open(path).unwrap();
        futures::executor::block_on(mem.save_entry(MemoryEntry {
            id: "f1".into(),
            kind: EntryKind::Fact,
            session_id: "s1".into(),
            summary: "Fact one".into(),
            content: "First fact".into(),
            tags: vec![],
            created_at: "2026-04-29".into(),
        }));
        futures::executor::block_on(mem.save_entry(MemoryEntry {
            id: "d1".into(),
            kind: EntryKind::Decision,
            session_id: "s1".into(),
            summary: "Decision one".into(),
            content: "First decision".into(),
            tags: vec![],
            created_at: "2026-04-29".into(),
        }));
        let facts = futures::executor::block_on(mem.entries_by_kind(&EntryKind::Fact, 10));
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].id, "f1");

        let decisions = futures::executor::block_on(mem.entries_by_kind(&EntryKind::Decision, 10));
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].id, "d1");
    }

    #[test]
    fn test_sqlite_recall_empty() {
        let path = ":memory:";
        let mem = SqliteMemory::open(path).unwrap();
        let results = futures::executor::block_on(mem.recall("nothing", 10));
        assert!(results.is_empty());
    }
}
