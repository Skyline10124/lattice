use async_trait::async_trait;
use std::sync::Arc;
use std::sync::Mutex;

/// Current time as milliseconds since UNIX epoch.
/// Used for MemoryEntry IDs and created_at timestamps.
pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

// ---------------------------------------------------------------------------
// MemoryEntry — a single unit of stored information
// ---------------------------------------------------------------------------

/// Kinds of memory entries.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    /// Raw conversation log (input → output).
    SessionLog,
    /// A factual statement extracted from conversation ("the project path is X").
    Fact,
    /// A decision or design choice made during the session.
    Decision,
    /// Project-level context (CLAUDE.md rules, repo structure).
    ProjectContext,
}

/// A single memory entry, akin to hindsight's memory_item.
#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub id: String,
    pub kind: EntryKind,
    pub session_id: String,
    pub summary: String,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: String,
}

impl MemoryEntry {
    /// Convert the entry kind to a static string for storage.
    fn kind_str(&self) -> &str {
        match self.kind {
            EntryKind::SessionLog => "session_log",
            EntryKind::Fact => "fact",
            EntryKind::Decision => "decision",
            EntryKind::ProjectContext => "project_context",
        }
    }
}

// ---------------------------------------------------------------------------
// Memory trait — persistent, searchable memory
// ---------------------------------------------------------------------------

/// Cross-session persistent memory. Supports both full-text and semantic search.
#[async_trait]
pub trait Memory: Send + Sync {
    /// Store a memory entry.
    async fn save_entry(&self, entry: MemoryEntry);

    /// Recall entries matching a natural-language query.
    async fn recall(&self, query: &str, limit: usize) -> Vec<MemoryEntry>;

    /// List all entries of a given kind.
    async fn entries_by_kind(&self, kind: &EntryKind, limit: usize) -> Vec<MemoryEntry>;

    /// Reflect on a conversation and extract memories.
    /// Returns summaries of what should be remembered.
    fn reflect(&self, _session_log: &[ConversationTurn]) -> Vec<String>;

    /// Clone this memory as a trait object.
    /// Default implementation creates a fresh InMemoryMemory.
    fn clone_box(&self) -> Box<dyn Memory> {
        Box::new(InMemoryMemory::new())
    }
}

/// A single turn in a conversation (used as input for reflection).
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
}

// ---------------------------------------------------------------------------
// SharedMemory — cross-agent shared memory with partition access control (D7)
// ---------------------------------------------------------------------------

/// Shared partition identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SharedPartition {
    Named(String),
    All,
}

/// Agent's read/write access to shared partitions.
/// Advisory-only: the caller self-declares access. In single-process deployments
/// this is sufficient (agents share the same binary). For multi-process/multi-team
/// deployments, enforce partitions at the bus/harness layer using agent identity.
#[derive(Debug, Clone, Default)]
pub struct PartitionAccess {
    pub read: Vec<SharedPartition>,
    pub write: Vec<SharedPartition>,
}

impl PartitionAccess {
    pub fn new(read: Vec<SharedPartition>, write: Vec<SharedPartition>) -> Self {
        Self { read, write }
    }

    fn can_read(&self, partition: &SharedPartition) -> bool {
        self.read
            .iter()
            .any(|p| *p == SharedPartition::All || p == partition)
    }

    fn can_write(&self, partition: &SharedPartition) -> bool {
        self.write
            .iter()
            .any(|p| *p == SharedPartition::All || p == partition)
    }
}

/// Memory operation errors.
#[derive(Debug)]
pub enum MemoryError {
    AccessDenied(SharedPartition),
    StorageError(String),
}

/// Cross-agent shared memory with partition-based access control.
/// Separate trait from Memory — agents use Memory for private state,
/// SharedMemory for cross-agent collaboration.
#[async_trait]
pub trait SharedMemory: Send + Sync {
    /// Write to a shared partition. Caller declares access; denied if no write permission.
    async fn save_shared(
        &self,
        entry: MemoryEntry,
        partition: SharedPartition,
        access: &PartitionAccess,
    ) -> Result<(), MemoryError>;

    /// Read from a shared partition. Caller declares access; denied if no read permission.
    async fn read_shared(
        &self,
        query: &str,
        partition: SharedPartition,
        access: &PartitionAccess,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError>;
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
}

impl Default for InMemoryMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Memory for InMemoryMemory {
    async fn save_entry(&self, entry: MemoryEntry) {
        self.store.lock().unwrap().push(entry);
    }

    async fn recall(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        self.store
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.summary.contains(query) || e.content.contains(query))
            .take(limit)
            .cloned()
            .collect()
    }

    async fn entries_by_kind(&self, kind: &EntryKind, limit: usize) -> Vec<MemoryEntry> {
        self.store
            .lock()
            .unwrap()
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

    fn reflect(&self, _session_log: &[ConversationTurn]) -> Vec<String> {
        vec![]
    }

    fn clone_box(&self) -> Box<dyn Memory> {
        Box::new(InMemoryMemory {
            store: self.store.clone(),
        })
    }
}

pub mod reflect;
pub mod sqlite;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_save_and_recall_inmemory() {
        let mem = InMemoryMemory::new();
        futures::executor::block_on(mem.save_entry(MemoryEntry {
            id: "1".into(),
            kind: EntryKind::Fact,
            session_id: "s1".into(),
            summary: "Project uses Rust".into(),
            content: "lattice is written in Rust".into(),
            tags: vec!["project".into()],
            created_at: "2026-04-29".into(),
        }));
        let results = futures::executor::block_on(mem.recall("Rust", 10));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "Project uses Rust");
    }

    #[test]
    fn test_recall_empty() {
        let mem = InMemoryMemory::new();
        let results = futures::executor::block_on(mem.recall("nothing", 10));
        assert!(results.is_empty());
    }

    #[test]
    fn test_entries_by_kind_inmemory() {
        let mem = InMemoryMemory::new();
        futures::executor::block_on(mem.save_entry(MemoryEntry {
            id: "kind-fact".into(),
            kind: EntryKind::Fact,
            session_id: "s1".into(),
            summary: "Fact one".into(),
            content: "First fact content".into(),
            tags: vec![],
            created_at: "2026-04-29".into(),
        }));
        futures::executor::block_on(mem.save_entry(MemoryEntry {
            id: "kind-decision".into(),
            kind: EntryKind::Decision,
            session_id: "s1".into(),
            summary: "Decision one".into(),
            content: "First decision content".into(),
            tags: vec![],
            created_at: "2026-04-29".into(),
        }));
        let facts = futures::executor::block_on(mem.entries_by_kind(&EntryKind::Fact, 10));
        assert!(facts.iter().any(|e| e.id == "kind-fact"));
        assert!(!facts.iter().any(|e| e.id == "kind-decision"));

        let decisions = futures::executor::block_on(mem.entries_by_kind(&EntryKind::Decision, 10));
        assert!(decisions.iter().any(|e| e.id == "kind-decision"));
        assert!(!decisions.iter().any(|e| e.id == "kind-fact"));
    }

    // --- PartitionAccess::can_read / can_write tests ---

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
