// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Resource limits, epoch interruption, and abort tracking for Component
//! Tool execution.
//!
//! The shared engine uses epoch interruption for precise per-Store
//! cancellation/deadline: the epoch ticker increments the global epoch
//! periodically, but **cancellation does not directly bump the engine epoch**
//! (that would advance all Stores' epochs). Instead, each Store's deadline
//! callback checks its own cancellation/deadline and returns `Interrupt` only
//! for that Store; unrelated Stores continue.

use wasmtime::{ResourceLimiter, StoreLimits, StoreLimitsBuilder};

/// Why a Component call was aborted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbortReason {
    /// Caller requested cancellation via `Cancellation`.
    Cancelled,
    /// Per-call deadline elapsed.
    Deadline,
    /// Memory growth rejected by `RecordingLimits`.
    Memory,
    /// Table or instance growth rejected.
    Instances,
}

/// Wraps `StoreLimits` to record which limit rejected growth, so error
/// mapping doesn't depend on string matching.
pub(crate) struct RecordingLimits {
    inner: StoreLimits,
    pub last_rejection: Option<AbortReason>,
}

impl RecordingLimits {
    pub(crate) fn new(limits: &ExecutionLimits) -> Self {
        let inner = StoreLimitsBuilder::new()
            .memory_size(limits.memory_size)
            .instances(limits.instances)
            .memories(limits.memories)
            .tables(limits.tables)
            .table_elements(limits.table_elements)
            .build();
        Self {
            inner,
            last_rejection: None,
        }
    }
}

impl ResourceLimiter for RecordingLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        let ok = self.inner.memory_growing(current, desired, maximum)?;
        if !ok {
            self.last_rejection = Some(AbortReason::Memory);
        }
        Ok(ok)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> Result<bool, wasmtime::Error> {
        let ok = self.inner.table_growing(current, desired, maximum)?;
        if !ok {
            self.last_rejection = Some(AbortReason::Instances);
        }
        Ok(ok)
    }
}

/// Per-call resource limits. Applied to every Component call via
/// `ComponentRuntime::call`.
#[derive(Clone, Debug)]
pub struct ExecutionLimits {
    /// Wasmtime fuel (coarse CPU upper bound).
    pub fuel: u64,
    /// Maximum total linear memory size in bytes.
    pub memory_size: usize,
    /// Maximum number of component instances.
    pub instances: usize,
    /// Maximum number of memories.
    pub memories: usize,
    /// Maximum number of tables.
    pub tables: usize,
    /// Maximum table elements.
    pub table_elements: usize,
    /// Per-call deadline.
    pub deadline: std::time::Duration,
}

impl Default for ExecutionLimits {
    fn default() -> Self {
        Self {
            fuel: 1_000_000,
            memory_size: 64 * 1024 * 1024, // 64 MiB
            instances: 1,
            memories: 1,
            tables: 1,
            table_elements: 10_000,
            deadline: std::time::Duration::from_secs(30),
        }
    }
}
