// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Generated wasmtime Component bindings for the Tool Host world.
//!
//! The WIT world `lattice:tool-host@0.1.0` defines:
//!
//! - `host-control` (imported by the guest): cooperative cancellation probe
//!   + structured progress events. The host implements `Host` on `StoreState`.
//! - `workspace-observe` (imported by the guest, since #30): Policy-Visible
//!   Workspace read / list / grep / find. The host implements `Host` on
//!   `StoreState` in [`crate::host_workspace`], where all path safety, visibility,
//!   bounds, and evidence live.
//! - `tool-api` (exported by the guest): `run(params-json) ->
//!   result<tool-output, string>`. The host calls this to execute a Tool.
//!
//! No `wasi:*` imports are linked — the no-ambient-WASI gate is structural.
//!
//! # Why this reads `observe-workspace`'s generated world
//!
//! Each Tool's `generated/tool-host.wit` describes *that Tool's* world: the
//! interfaces it imports, and no others. The host has to be able to serve every
//! interface any built-in Tool may import, so its bindings must come from the
//! **widest** committed world — which since #30 is `observe.workspace`'s, the only
//! one declaring a domain import. A narrower source would compile, and then fail
//! at composition with a missing linker binding, which is a build-order accident
//! rather than a decision.
//!
//! `cargo xtask tool-codegen` renders every world from one generator, so a Tool
//! that declares a new domain interface changes this file's source of truth. When
//! a second domain import lands (#31/#32), the widest world is whichever Tool
//! declares the superset, and this path follows it.

wasmtime::component::bindgen!({
    path: "../../tools/observe-workspace/generated/tool-host.wit",
    world: "tool",
});
