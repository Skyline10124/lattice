use std::sync::{Arc, Mutex, MutexGuard};

pub use lattice_core::memory::{
    now_ms, EntryKind, MemoryEntry, MemoryError, PartitionAccess, SharedMemory, SharedPartition,
};

// ---------------------------------------------------------------------------
// Memory trait — persistent, searchable memory
// ---------------------------------------------------------------------------

/// Cross-session persistent memory. Supports both full-text and semantic search.
pub trait Memory: Send + Sync {
    fn save_entry(&self, entry: MemoryEntry);
    fn recall(&self, query: &str, limit: usize) -> Vec<MemoryEntry>;
    fn entries_by_kind(&self, kind: &EntryKind, limit: usize) -> Vec<MemoryEntry>;
    /// Clone the memory backend.
    ///
    /// For in-memory backends (`InMemoryMemory`) this shares the underlying
    /// store via `Arc`.  For persistent backends (e.g. `SqliteMemory`) this
    /// opens a new connection to the same database.
    ///
    fn clone_box(&self) -> Box<dyn Memory>;
}

// ---------------------------------------------------------------------------
// InMemoryMemory — HashMap-based, not persisted. Default implementation.
// ---------------------------------------------------------------------------

pub struct InMemoryMemory {
    store: Arc<Mutex<Vec<MemoryEntry>>>,
}

impl InMemoryMemory {
    pub fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn clear(&self) {
        self.entries().clear();
    }

    fn entries(&self) -> MutexGuard<'_, Vec<MemoryEntry>> {
        self.store.lock().unwrap_or_else(|err| err.into_inner())
    }
}

impl Default for InMemoryMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl Memory for InMemoryMemory {
    fn save_entry(&self, entry: MemoryEntry) {
        let mut entries = self.entries();
        let idx = entries
            .partition_point(|existing| existing.created_at.as_str() >= entry.created_at.as_str());
        entries.insert(idx, entry);
    }

    fn recall(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        self.entries()
            .iter()
            .filter(|e| e.summary.contains(query) || e.content.contains(query))
            .take(limit)
            .cloned()
            .collect()
    }

    fn entries_by_kind(&self, kind: &EntryKind, limit: usize) -> Vec<MemoryEntry> {
        self.entries()
            .iter()
            .filter(|e| {
                matches!(
                    (&e.kind, kind),
                    (EntryKind::SessionLog, EntryKind::SessionLog)
                        | (EntryKind::Fact, EntryKind::Fact)
                        | (EntryKind::Decision, EntryKind::Decision)
                        | (EntryKind::ProjectContext, EntryKind::ProjectContext)
                )
            })
            .take(limit)
            .cloned()
            .collect()
    }

    fn clone_box(&self) -> Box<dyn Memory> {
        Box::new(InMemoryMemory {
            store: self.store.clone(),
        })
    }
}

#[cfg(feature = "sqlite-memory")]
pub mod sqlite;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_and_recall_inmemory() {
        let mem = InMemoryMemory::new();
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
    fn test_recall_sorts_matching_entries_by_created_at_desc() {
        let mem = InMemoryMemory::new();
        for (id, created_at) in [
            ("oldest", "2026-05-01T00:00:00Z"),
            ("newest", "2026-05-03T00:00:00Z"),
            ("middle", "2026-05-02T00:00:00Z"),
        ] {
            mem.save_entry(MemoryEntry {
                id: id.into(),
                kind: EntryKind::Fact,
                session_id: "s1".into(),
                summary: format!("Rust fact {id}"),
                content: "lattice uses Rust".into(),
                tags: vec![],
                created_at: created_at.into(),
            });
        }

        let results = mem.recall("Rust", 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "newest");
        assert_eq!(results[1].id, "middle");
    }

    #[test]
    fn test_entries_by_kind_sorts_by_created_at_desc() {
        let mem = InMemoryMemory::new();
        for (id, kind, created_at) in [
            ("old_fact", EntryKind::Fact, "2026-05-01T00:00:00Z"),
            ("decision", EntryKind::Decision, "2026-05-04T00:00:00Z"),
            ("new_fact", EntryKind::Fact, "2026-05-03T00:00:00Z"),
            ("mid_fact", EntryKind::Fact, "2026-05-02T00:00:00Z"),
        ] {
            mem.save_entry(MemoryEntry {
                id: id.into(),
                kind,
                session_id: "s1".into(),
                summary: id.into(),
                content: id.into(),
                tags: vec![],
                created_at: created_at.into(),
            });
        }

        let results = mem.entries_by_kind(&EntryKind::Fact, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "new_fact");
        assert_eq!(results[1].id, "mid_fact");
    }

    #[test]
    fn test_recall_empty() {
        let mem = InMemoryMemory::new();
        let results = mem.recall("nothing", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_entries_by_kind_inmemory() {
        let mem = InMemoryMemory::new();
        mem.save_entry(MemoryEntry {
            id: "kind-fact".into(),
            kind: EntryKind::Fact,
            session_id: "s1".into(),
            summary: "Fact one".into(),
            content: "First fact content".into(),
            tags: vec![],
            created_at: "2026-04-29".into(),
        });
        mem.save_entry(MemoryEntry {
            id: "kind-decision".into(),
            kind: EntryKind::Decision,
            session_id: "s1".into(),
            summary: "Decision one".into(),
            content: "First decision content".into(),
            tags: vec![],
            created_at: "2026-04-29".into(),
        });
        let facts = mem.entries_by_kind(&EntryKind::Fact, 10);
        assert!(facts.iter().any(|e| e.id == "kind-fact"));
        assert!(!facts.iter().any(|e| e.id == "kind-decision"));

        let decisions = mem.entries_by_kind(&EntryKind::Decision, 10);
        assert!(decisions.iter().any(|e| e.id == "kind-decision"));
        assert!(!decisions.iter().any(|e| e.id == "kind-fact"));
    }

    #[test]
    fn test_can_read_named_in_list() {
        let access = PartitionAccess::new(vec![SharedPartition::Named("results".into())], vec![]);
        assert!(access.can_read(&SharedPartition::Named("results".into())));
    }

    #[test]
    fn test_can_read_named_not_in_list() {
        let access = PartitionAccess::new(vec![SharedPartition::Named("results".into())], vec![]);
        assert!(!access.can_read(&SharedPartition::Named("other".into())));
    }

    #[test]
    fn test_can_read_shared_all() {
        let access = PartitionAccess::new(vec![SharedPartition::All], vec![]);
        assert!(access.can_read(&SharedPartition::Named("anything".into())));
    }

    #[test]
    fn test_can_write_named_in_list() {
        let access = PartitionAccess::new(vec![], vec![SharedPartition::Named("results".into())]);
        assert!(access.can_write(&SharedPartition::Named("results".into())));
    }

    #[test]
    fn test_can_write_empty_list() {
        let access = PartitionAccess::new(vec![], vec![]);
        assert!(!access.can_write(&SharedPartition::Named("results".into())));
    }

    #[test]
    fn test_can_read_empty_list() {
        let access = PartitionAccess::new(vec![], vec![]);
        assert!(!access.can_read(&SharedPartition::Named("results".into())));
    }
}
