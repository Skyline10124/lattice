use crate::{
    EntryKind, InMemoryMemory, Memory, MemoryEntry, MemoryError, PartitionAccess, SharedMemory,
    SharedPartition,
};
use rusqlite::{params, Connection};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

/// SQLite-backed persistent memory with FTS5 full-text search.
/// Implements both Memory (private) and SharedMemory (cross-agent partitioned).
pub struct SqliteMemory {
    conn: Mutex<Connection>,
    db_path: String,
}

impl SqliteMemory {
    /// Open (or create) a memory database at the given path.
    /// Creates parent directories if they don't exist.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        if path != ":memory:" {
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent).ok();
            }
        }
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
                created_at TEXT NOT NULL,
                partition TEXT NOT NULL DEFAULT 'private'
            );
            CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
                summary, content, tags_str,
                content='memory', content_rowid='rowid'
            );
            ",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
            db_path: path.to_string(),
        })
    }

    fn escape_like(query: &str) -> String {
        query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    }

    /// Escape a query string for safe use in FTS5 MATCH expressions.
    /// FTS5 treats AND, OR, NOT, NEAR as operators and *, ^, (, ) as
    /// special characters. Wrapping in double-quotes makes the query a
    /// phrase match, but internal quotes must be removed and other
    /// special characters escaped to prevent semantic injection.
    fn escape_fts5(query: &str) -> String {
        // Remove internal double-quotes (would break phrase syntax)
        let cleaned = query.replace('"', "");
        // Remove FTS5 operators that could change query semantics
        let cleaned = cleaned
            .replace(" AND ", " ")
            .replace(" OR ", " ")
            .replace(" NOT ", " ")
            .replace(" NEAR ", " ");
        // Remove FTS5 special characters: * (prefix), ^ (initial), ( ) (grouping)
        let cleaned: String = cleaned
            .chars()
            .filter(|c| !matches!(c, '*' | '^' | '(' | ')'))
            .collect();
        format!("\"{}\"", cleaned)
    }

    fn recall_like(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("SqliteMemory: poisoned lock in recall_like: {:?}", e);
                return vec![];
            }
        };
        let pattern = format!("%{}%", Self::escape_like(query));
        let sql = "SELECT id, kind, session_id, summary, content, tags, created_at
                   FROM memory
                   WHERE summary LIKE ?1 ESCAPE '\\' OR content LIKE ?1 ESCAPE '\\'
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

    fn partition_str(partition: &SharedPartition) -> String {
        match partition {
            SharedPartition::Named(name) => name.clone(),
            SharedPartition::All => "_all".to_string(),
        }
    }
}

impl Memory for SqliteMemory {
    fn save_entry(&self, entry: MemoryEntry) {
        if let Err(err) = self.try_save_entry(entry) {
            tracing::warn!("SqliteMemory: failed to save memory entry: {err}");
        }
    }

    fn recall(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("SqliteMemory: poisoned lock: {:?}", e);
                return vec![];
            }
        };
        let fts_query = Self::escape_fts5(query);
        let sql = "SELECT m.id, m.kind, m.session_id, m.summary, m.content, m.tags, m.created_at
                   FROM memory m
                   WHERE m.rowid IN (
                       SELECT rowid FROM memory_fts WHERE memory_fts MATCH ?1
                   )
                   LIMIT ?2";
        if let Ok(mut stmt) = conn.prepare(sql) {
            if let Ok(rows) = stmt.query_map(params![fts_query, limit as i64], Self::row_to_entry) {
                let results: Vec<_> = rows.filter_map(|r| r.ok()).collect();
                if !results.is_empty() {
                    return results;
                }
            }
        }
        drop(conn);
        self.recall_like(query, limit)
    }

    fn entries_by_kind(&self, kind: &EntryKind, limit: usize) -> Vec<MemoryEntry> {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("SqliteMemory: poisoned lock: {:?}", e);
                return vec![];
            }
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

    fn clone_box(&self) -> Box<dyn Memory> {
        self.try_clone_box().expect(
            "SqliteMemory::clone_box: reopen failed; use Arc<dyn Memory> for shared ownership",
        )
    }
}

impl SharedMemory for SqliteMemory {
    fn save_shared<'a>(
        &'a self,
        entry: MemoryEntry,
        partition: SharedPartition,
        access: &'a PartitionAccess,
    ) -> Pin<Box<dyn Future<Output = Result<(), MemoryError>> + Send + 'a>> {
        Box::pin(async move { self.save_shared_sync(entry, partition, access) })
    }

    fn read_shared<'a>(
        &'a self,
        query: &'a str,
        partition: SharedPartition,
        access: &'a PartitionAccess,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<MemoryEntry>, MemoryError>> + Send + 'a>> {
        Box::pin(async move { self.read_shared_sync(query, partition, access, limit) })
    }
}

impl SqliteMemory {
    pub fn try_clone_box(&self) -> Result<Box<dyn Memory>, String> {
        SqliteMemory::open(&self.db_path)
            .map(|clone| Box::new(clone) as Box<dyn Memory>)
            .map_err(|e| format!("failed to reopen '{}': {}", self.db_path, e))
    }

    fn try_save_entry(&self, entry: MemoryEntry) -> rusqlite::Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        Self::insert_entry(&conn, &entry, None)
    }

    fn insert_entry(
        conn: &Connection,
        entry: &MemoryEntry,
        partition: Option<&str>,
    ) -> rusqlite::Result<()> {
        let tags_json = serde_json::to_string(&entry.tags).unwrap_or_else(|_| "[]".to_string());
        conn.execute(
            "DELETE FROM memory_fts WHERE rowid = (SELECT rowid FROM memory WHERE id = ?1)",
            params![entry.id],
        )?;

        match partition {
            Some(partition) => {
                conn.execute(
                    "INSERT OR REPLACE INTO memory (id, kind, session_id, summary, content, tags, created_at, partition)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        entry.id,
                        entry.kind_str(),
                        entry.session_id,
                        entry.summary,
                        entry.content,
                        tags_json,
                        entry.created_at,
                        partition,
                    ],
                )?;
            }
            None => {
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
                        entry.created_at,
                    ],
                )?;
            }
        }

        conn.execute(
            "INSERT INTO memory_fts(rowid, summary, content, tags_str)
             SELECT rowid, summary, content, tags FROM memory WHERE id = ?1",
            params![entry.id],
        )?;

        Ok(())
    }

    fn save_shared_sync(
        &self,
        entry: MemoryEntry,
        partition: SharedPartition,
        access: &PartitionAccess,
    ) -> Result<(), MemoryError> {
        if !access.can_write(&partition) {
            return Err(MemoryError::AccessDenied(partition));
        }

        let conn = self
            .conn
            .lock()
            .map_err(|_| MemoryError::StorageError("lock poisoned".into()))?;
        let partition_str = Self::partition_str(&partition);
        Self::insert_entry(&conn, &entry, Some(&partition_str))
            .map_err(|e| MemoryError::StorageError(e.to_string()))
    }

    fn read_shared_sync(
        &self,
        query: &str,
        partition: SharedPartition,
        access: &PartitionAccess,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        if !access.can_read(&partition) {
            return Err(MemoryError::AccessDenied(partition));
        }

        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return Err(MemoryError::StorageError("lock poisoned".into())),
        };
        let partition_str = Self::partition_str(&partition);
        let sql = "SELECT id, kind, session_id, summary, content, tags, created_at
                   FROM memory WHERE partition = ?1 AND (summary LIKE ?2 ESCAPE '\\' OR content LIKE ?2 ESCAPE '\\')
                   LIMIT ?3";
        let pattern = format!("%{}%", Self::escape_like(query));
        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| MemoryError::StorageError(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![partition_str, pattern, limit as i64],
                Self::row_to_entry,
            )
            .map_err(|e| MemoryError::StorageError(e.to_string()))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryKind, MemoryEntry, PartitionAccess, SharedPartition};

    #[test]
    fn test_sqlite_save_and_recall() {
        let mem = SqliteMemory::open(":memory:").unwrap();
        mem.save_entry(MemoryEntry {
            id: "1".into(),
            kind: EntryKind::Fact,
            session_id: "s1".into(),
            summary: "Project uses Rust".into(),
            content: "lattice is written in Rust".into(),
            tags: vec!["project".into()],
            created_at: "2026-04-29".into(),
        });
        let results = mem.recall("Rust", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "Project uses Rust");
    }

    #[test]
    fn test_sqlite_entries_by_kind() {
        let mem = SqliteMemory::open(":memory:").unwrap();
        mem.save_entry(MemoryEntry {
            id: "f1".into(),
            kind: EntryKind::Fact,
            session_id: "s1".into(),
            summary: "Fact one".into(),
            content: "First fact".into(),
            tags: vec![],
            created_at: "2026-04-29".into(),
        });
        mem.save_entry(MemoryEntry {
            id: "d1".into(),
            kind: EntryKind::Decision,
            session_id: "s1".into(),
            summary: "Decision one".into(),
            content: "First decision".into(),
            tags: vec![],
            created_at: "2026-04-29".into(),
        });
        let facts = mem.entries_by_kind(&EntryKind::Fact, 10);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].id, "f1");

        let decisions = mem.entries_by_kind(&EntryKind::Decision, 10);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].id, "d1");
    }

    #[test]
    fn test_sqlite_recall_empty() {
        let mem = SqliteMemory::open(":memory:").unwrap();
        let results = mem.recall("nothing", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_shared_write_and_read_with_access() {
        let mem = SqliteMemory::open(":memory:").unwrap();
        let access = PartitionAccess::new(
            vec![SharedPartition::Named("review-results".into())],
            vec![SharedPartition::Named("review-results".into())],
        );

        futures::executor::block_on(mem.save_shared(
            MemoryEntry {
                id: "sr1".into(),
                kind: EntryKind::Decision,
                session_id: "reviewer".into(),
                summary: "Approved change".into(),
                content: "Code looks good".into(),
                tags: vec!["review".into()],
                created_at: "2026-05-01".into(),
            },
            SharedPartition::Named("review-results".into()),
            &access,
        ))
        .unwrap();

        let results = futures::executor::block_on(mem.read_shared(
            "Approved",
            SharedPartition::Named("review-results".into()),
            &access,
            10,
        ))
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "Approved change");
    }

    #[test]
    fn test_shared_write_access_denied() {
        let mem = SqliteMemory::open(":memory:").unwrap();
        let access = PartitionAccess::new(
            vec![SharedPartition::Named("review-results".into())],
            vec![],
        );

        let result = futures::executor::block_on(mem.save_shared(
            MemoryEntry {
                id: "sr2".into(),
                kind: EntryKind::Fact,
                session_id: "observer".into(),
                summary: "Trying to write".into(),
                content: "Should be denied".into(),
                tags: vec![],
                created_at: "2026-05-01".into(),
            },
            SharedPartition::Named("review-results".into()),
            &access,
        ));
        assert!(matches!(result, Err(MemoryError::AccessDenied(_))));
    }

    #[test]
    fn test_shared_read_access_denied() {
        let mem = SqliteMemory::open(":memory:").unwrap();
        let write_only = PartitionAccess::new(
            vec![],
            vec![SharedPartition::Named("security-findings".into())],
        );

        futures::executor::block_on(mem.save_shared(
            MemoryEntry {
                id: "sf1".into(),
                kind: EntryKind::Fact,
                session_id: "scanner".into(),
                summary: "SQL injection found".into(),
                content: "Vulnerable endpoint".into(),
                tags: vec!["security".into()],
                created_at: "2026-05-01".into(),
            },
            SharedPartition::Named("security-findings".into()),
            &write_only,
        ))
        .unwrap();

        let result = futures::executor::block_on(mem.read_shared(
            "SQL",
            SharedPartition::Named("security-findings".into()),
            &write_only,
            10,
        ));
        assert!(matches!(result, Err(MemoryError::AccessDenied(_))));
    }

    #[test]
    fn test_shared_partition_all_access() {
        let mem = SqliteMemory::open(":memory:").unwrap();
        let super_access =
            PartitionAccess::new(vec![SharedPartition::All], vec![SharedPartition::All]);

        futures::executor::block_on(mem.save_shared(
            MemoryEntry {
                id: "sa1".into(),
                kind: EntryKind::Fact,
                session_id: "admin".into(),
                summary: "Global fact".into(),
                content: "Everyone can see this".into(),
                tags: vec![],
                created_at: "2026-05-01".into(),
            },
            SharedPartition::Named("any-topic".into()),
            &super_access,
        ))
        .unwrap();

        let results = futures::executor::block_on(mem.read_shared(
            "Global",
            SharedPartition::Named("any-topic".into()),
            &super_access,
            10,
        ))
        .unwrap();
        assert_eq!(results.len(), 1);
    }
}
