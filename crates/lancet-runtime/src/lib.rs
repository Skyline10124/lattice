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
//! - **Minimal ToolHost** — `host-control` only (cancellation + structured
//!   events), with every effectful import gated by its host-bound Cell and
//!   concrete target. No workspace/network/session domain operation before
//!   #6.
//! - **Call execution** — params validation, Component instantiation, result
//!   CellRef validation against the descriptor ceiling and the admitted set,
//!   result schema validation, host-computed context measure, and Cell
//!   evidence recording. Errors map to structured `CepError` codes
//!   (`ToolTrap`/`ToolTimeout`/`ToolResourceExhausted`/`CallCancelled`).
//! - **Private workspace history** — [`history::WorkspaceHistory`] appends
//!   every successfully installed Atomic Local Mutation to a private bare
//!   `gix` repository that lives outside the Local Workspace and the user's
//!   project `.git` (GitHub #63).
//!
//! `lancet-runtime` does **not** depend on `lancet-native`, `lancet-api`, or
//! any LATTICE session/agent/profile crate. The dependency graph has no cycle.

pub mod admission;
pub mod bindings;
pub mod composition;
pub mod error_map;
pub mod evidence;
pub mod generated;
pub mod history;
pub mod host;
pub mod limits;
pub mod runtime;

#[cfg(feature = "test-utils")]
pub mod test_helpers;

pub use admission::{
    AdmissionDenial, AdmissionDenialReason, AdmittedTool, OperationDenial, PolicyValidationError,
    admit_tool, authorize_operation, validate_policy_against_catalog,
};
pub use composition::{ClosedCatalog, CompositionError, EmbeddedTool, ExpectedToolIdentity};
pub use evidence::CellEvidenceLedger;
pub use history::{
    HistorySnapshot, MutationRecord, RecordedMutation, WorkspaceHistory, WorkspaceHistoryBacking,
    WorkspaceHistoryError,
};
pub use limits::ExecutionLimits;
pub use runtime::{ComponentCallContext, ComponentRuntime};
