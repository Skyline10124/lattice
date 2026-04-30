use async_trait::async_trait;
use std::sync::LazyLock;

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
// InMemoryMemory — HashMap-based, not persisted. Default implementation.
// ---------------------------------------------------------------------------

pub struct InMemoryMemory;

impl InMemoryMemory {
    pub fn new() -> Self {
        Self
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
        // InMemoryMemory is not Sync (it uses RefCell internally).
        // Using a static LazyLock<Mutex<…>> for cross-thread safety.
        // For now, just delegate to a global store.
        GLOBAL_STORE.lock().unwrap().entries.push(entry);
    }

    async fn recall(&self, query: &str, limit: usize) -> Vec<MemoryEntry> {
        let store = GLOBAL_STORE.lock().unwrap();
        store
            .entries
            .iter()
            .filter(|e| e.summary.contains(query) || e.content.contains(query))
            .take(limit)
            .cloned()
            .collect()
    }

    async fn entries_by_kind(&self, kind: &EntryKind, limit: usize) -> Vec<MemoryEntry> {
        let store = GLOBAL_STORE.lock().unwrap();
        store
            .entries
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
        // Default: no-op reflection. Override in SqliteMemory or use Reflect
        // to get real LLM-based extraction.
        vec![]
    }
}

use std::sync::Mutex;

struct GlobalStore {
    entries: Vec<MemoryEntry>,
}

static GLOBAL_STORE: LazyLock<Mutex<GlobalStore>> =
    LazyLock::new(|| Mutex::new(GlobalStore { entries: vec![] }));

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
}
