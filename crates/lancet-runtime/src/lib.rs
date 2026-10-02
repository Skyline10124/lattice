// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lancet-runtime` — ADR 0004 Component engine, closed composition gate,
//! and minimal ToolHost for LATTICE Source Tools.
//!
//! This crate is the Component execution substrate (#27 slice C, Codeberg #5).
//! It owns:
//!
//! - **Embedded tool registration** — `EmbeddedTool` (name + bytes) collected
//!   via `inventory`. A Tool crate cannot self-approve; only the trusted host
//!   composition step may pin identity.
//! - **Closed composition gate** — `ClosedCatalog::compose` verifies every
//!   registration against a host-pinned `ExpectedToolIdentity` (descriptor
//!   digest, artifact digest, ABI, imports) at startup, before any call is
//!   served. The catalog is immutable after composition.
//! - **Component engine** — wasmtime with component-model, no ambient WASI
//!   (no `wasmtime-wasi`/`wasi-common` dependency). Only versioned Tool Host
//!   WIT imports are linked.
//! - **Cell capability admission** — the per-call admission stages (enabled
//!   tool → profile deny → boundary predicate) and the per-operation
//!   authorizer are the only authorization authority for Component Tools.
//! - **ToolHost** — `host-control` (cancellation + structured events) plus, since
//!   GitHub #30, the `workspace-observe` domain interface, with every effectful
//!   import gated by its host-bound Cell and concrete target. A Tool reaches the
//!   filesystem only through these imports; there is no ambient authority and no
//!   path a guest can name outside them.
//! - **Call execution** — params validation, Component instantiation, result
//!   CellRef validation against the descriptor ceiling and the admitted set,
//!   result schema validation, host-computed context measure, and Cell
//!   evidence recording. Errors map to structured `CepError` codes
//!   (`ToolTrap`/`ToolTimeout`/`ToolResourceExhausted`/`CallCancelled`).
//! - **Private workspace history** — [`history::WorkspaceHistory`] appends
//!   every successfully installed Atomic Local Mutation to a project-scoped
//!   `lattice-store` kernel repository that lives outside the Local Workspace
//!   and the user's project `.git` (GitHub #63, migrated in #143).
//! - **The concrete Local Workspace** — [`workspace::LocalWorkspace`] and the
//!   [`workspace::VisibilityEngine`] moved down here in GitHub #30, because the
//!   `workspace-observe` and `workspace-mutate` Tool Host imports execute all
//!   filesystem operations here. Secure path resolution, System Path
//!   Exclusion, visibility, bounded reads, atomic installation and mutation history
//!   share this one concrete module.
//!
//! `lancet-runtime` does **not** depend on `lancet-api` or any LATTICE
//! session/agent/profile crate. The `lancet-authority` edge points downward.
pub mod admission;
pub mod bindings;
pub mod catalog;
pub mod composition;
pub mod control;
pub mod error_map;
pub mod evidence;
pub mod execution;
pub mod generated;
pub mod history;
pub mod host;
pub mod host_mutate;
pub mod host_network;
pub mod host_workspace;
pub mod limits;
pub mod runtime;
pub mod workspace;

#[cfg(feature = "test-utils")]
pub mod test_helpers;

pub use admission::{
    AdmissionDenial, AdmissionDenialReason, AdmittedTool, OperationDenial, PolicyValidationError,
    admit_tool, authorize_operation, validate_policy_against_catalog,
};
pub use catalog::{CatalogSelectionError, ToolDefinition, enabled_tool_definitions};
pub use composition::{ClosedCatalog, CompositionError, EmbeddedTool, ExpectedToolIdentity};
pub use control::{Cancellation, CancellationSender, EventSink, ExecutionReceipt};
pub use evidence::CellEvidenceLedger;
pub use execution::RuntimeExecution;
pub use history::{
    HistorySnapshot, MutationRecord, RecordedMutation, WorkspaceHistory, WorkspaceHistoryBacking,
    WorkspaceHistoryError,
};
/// Lock and addressing vocabulary from the durability kernel, re-exported so
/// callers holding a runtime handle need no direct kernel dependency
/// (ADR 0005 §5).
pub use lattice_store::{Blocking, FlockGuard, LockMode, ProjectStoreKey, StateLayout};

pub use host_mutate::{
    CreateInstalled, FileBytes, MutateError, MutationCallContext, ReplaceInstalled,
    WorkspaceMutationHost, mutate_cep_error,
};
pub use host_network::{BudgetRemaining, NetworkFetchHost};
pub use host_workspace::{
    BudgetCharge, BudgetRefusal, ObservationCallContext, ObservationError,
    WorkspaceObservationHost, observation_cep_error,
};
pub use limits::ExecutionLimits;
pub use runtime::{ComponentCallContext, ComponentRuntime};
pub use workspace::{
    CreatedFile, DirectoryEntry, FindRequest, GrepRequest, InstalledFile, LocalWorkspace,
    LocalWorkspaceError, MAX_WORKSPACE_WRITE_BYTES, PathFacts, PathIntent, PathKind,
    VisibilityEngine, WorkspacePath, content_hash,
};
