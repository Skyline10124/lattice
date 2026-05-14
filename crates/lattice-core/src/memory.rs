use std::future::Future;
use std::pin::Pin;

/// Current time as milliseconds since UNIX epoch.
pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

/// Kinds of memory entries.
#[derive(Debug, Clone, PartialEq)]
pub enum EntryKind {
    SessionLog,
    Fact,
    Decision,
    ProjectContext,
}

/// A single memory entry shared by private and partitioned memory backends.
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
    pub fn kind_str(&self) -> &str {
        match self.kind {
            EntryKind::SessionLog => "session_log",
            EntryKind::Fact => "fact",
            EntryKind::Decision => "decision",
            EntryKind::ProjectContext => "project_context",
        }
    }
}

/// Shared partition identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SharedPartition {
    Named(String),
    All,
}

/// Agent read/write access to shared memory partitions.
#[derive(Debug, Clone, Default)]
pub struct PartitionAccess {
    pub read: Vec<SharedPartition>,
    pub write: Vec<SharedPartition>,
}

impl PartitionAccess {
    pub fn new(read: Vec<SharedPartition>, write: Vec<SharedPartition>) -> Self {
        Self { read, write }
    }

    pub fn can_read(&self, partition: &SharedPartition) -> bool {
        self.read
            .iter()
            .any(|p| *p == SharedPartition::All || p == partition)
    }

    pub fn can_write(&self, partition: &SharedPartition) -> bool {
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
pub trait SharedMemory: Send + Sync {
    fn save_shared<'a>(
        &'a self,
        entry: MemoryEntry,
        partition: SharedPartition,
        access: &'a PartitionAccess,
    ) -> Pin<Box<dyn Future<Output = Result<(), MemoryError>> + Send + 'a>>;

    fn read_shared<'a>(
        &'a self,
        query: &'a str,
        partition: SharedPartition,
        access: &'a PartitionAccess,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<MemoryEntry>, MemoryError>> + Send + 'a>>;
}
