use async_trait::async_trait;
pub use lattice_agent::memory::{
    now_ms, ConversationTurn, EntryKind, InMemoryMemory, Memory, MemoryEntry, MemoryError,
    PartitionAccess, SharedPartition,
};

/// Cross-agent shared memory with partition-based access control.
/// Separate trait from Memory — agents use Memory for private state,
/// SharedMemory for cross-agent collaboration.
#[async_trait]
pub trait SharedMemory: Send + Sync {
    async fn save_shared(
        &self,
        entry: MemoryEntry,
        partition: SharedPartition,
        access: &PartitionAccess,
    ) -> Result<(), MemoryError>;

    async fn read_shared(
        &self,
        query: &str,
        partition: SharedPartition,
        access: &PartitionAccess,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError>;
}

pub mod sqlite;
