// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Generated wasmtime Component bindings for the Tool Host world.
//!
//! The WIT world `lattice:tool-host@0.1.0` defines two interfaces:
//!
//! - `host-control` (imported by the guest): cooperative cancellation probe
//!   + structured progress events. The host implements `Host` on `StoreState`.
//! - `tool-api` (exported by the guest): `run(params-json) ->
//!   result<tool-output, string>`. The host calls this to execute a Tool.
//!
//! No `wasi:*` imports are linked — the no-ambient-WASI gate is structural.

wasmtime::component::bindgen!({
    path: "../../tools/noop/generated/tool-host.wit",
    world: "tool",
});
