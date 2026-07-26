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

/// Why a Component call was aborted, as recorded by the epoch callback or the
/// `RecordingLimits` growth callbacks.
///
/// Count limits (instances/memories/tables) are **not** represented here: they
/// have no wasmtime rejection callback, so a count-limit failure is classified
/// after the fact from the component's static [`ResourceFootprint`] (see
/// `error_map::map_component_error`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AbortReason {
    /// Caller requested cancellation via `Cancellation`.
    Cancelled,
    /// Per-call deadline elapsed.
    Deadline,
    /// Linear-memory byte growth rejected by `RecordingLimits` (the
    /// `memory_size` limit).
    MemoryBytes,
    /// Table element growth rejected by `RecordingLimits` (the
    /// `table_elements` limit).
    TableElements,
}

/// Static per-component resource footprint: how many core instances, linear
/// memories, and tables the component allocates when instantiated.
///
/// Computed once at composition from the component binary and compared against
/// the per-call count limits to classify a count-limit instantiation failure.
/// wasmtime enforces these counts but exposes no rejection callback, so this is
/// the only string-free evidence of *which* count was exceeded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ResourceFootprint {
    /// Core module instantiations (`bump_resource_counts` increments one per
    /// instantiated core module).
    pub instances: usize,
    /// Defined linear memories summed across instantiated core modules.
    pub memories: usize,
    /// Defined tables summed across instantiated core modules.
    pub tables: usize,
}

impl ResourceFootprint {
    /// The first count whose requirement exceeds `limits`, in wasmtime's own
    /// enforcement order (instances, then memories, then tables). Returns the
    /// resource label for the structured error, or `None` when every count
    /// fits. The order must match wasmtime so the reported resource is the one
    /// wasmtime actually rejected on.
    pub fn first_exceeded(&self, limits: &ResourceFootprint) -> Option<&'static str> {
        if self.instances > limits.instances {
            Some("instances")
        } else if self.memories > limits.memories {
            Some("memories")
        } else if self.tables > limits.tables {
            Some("tables")
        } else {
            None
        }
    }
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
            self.last_rejection = Some(AbortReason::MemoryBytes);
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
            self.last_rejection = Some(AbortReason::TableElements);
        }
        Ok(ok)
    }

    // Count limits have no rejection callback, so forward them to the inner
    // `StoreLimits` (built from the configured `ExecutionLimits`). Without this
    // the trait defaults (10_000 each) would silently override the configured
    // instance/table/memory counts.
    fn instances(&self) -> usize {
        ResourceLimiter::instances(&self.inner)
    }

    fn tables(&self) -> usize {
        ResourceLimiter::tables(&self.inner)
    }

    fn memories(&self) -> usize {
        ResourceLimiter::memories(&self.inner)
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
    /// Maximum number of core module instantiations. A componentized tool
    /// expands into several core instances (guest module + wit-component
    /// shim/fixup), so this is not 1 per tool.
    pub instances: usize,
    /// Maximum number of linear memories (summed across core instances).
    pub memories: usize,
    /// Maximum number of tables (summed across core instances).
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
            // Componentization overhead: a wit-bindgen guest expands into the
            // main core instance plus wit-component shim/fixup instances and
            // their tables. The noop tracer alone needs 3 instances / 2 tables,
            // so these carry headroom for tools that import more host
            // interfaces while still bounding a runaway component.
            instances: 16,
            memories: 8,
            tables: 16,
            table_elements: 10_000,
            deadline: std::time::Duration::from_secs(30),
        }
    }
}
