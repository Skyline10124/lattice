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
//! - `workspace-mutate` (imported by the guest, since #31): Structured Workspace
//!   Mutation create / edit installs. The host implements `Host` on `StoreState`
//!   in [`crate::host_mutate`], where precondition, atomic install, history, and
//!   evidence live.
//! - `tool-api` (exported by the guest): `run(params-json) ->
//!   result<tool-output, string>`. The host calls this to execute a Tool.
//!
//! No `wasi:*` imports are linked — the no-ambient-WASI gate is structural.
//!
//! # Why this reads the host-widest world
//!
//! Each Tool's `generated/tool-host.wit` describes *that Tool's* world: the
//! interfaces it imports, and no others. The host has to be able to serve every
//! interface any built-in Tool may import, so its bindings come from the
//! **widest** committed world under `crates/lancet-runtime/wit/tool-host.wit`
//! — the union of every `DomainImport` the runtime links. A narrower source
//! would compile, and then fail at composition with a missing linker binding.

wasmtime::component::bindgen!({
    path: "wit/tool-host.wit",
    world: "tool",
});
