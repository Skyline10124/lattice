// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `session.todo_write` — the first production Source Tool guest (#29).
//!
//! Behavior is the one the native handler had: validate a structured todo plan
//! and echo the normalized result. There is no session store behind it and this
//! migration adds none, so the Tool has exactly one effect — its result entering
//! the agent context, which the host measures.
//!
//! **All validation is host-side.** Every rule the native handler checked
//! imperatively (non-blank content, at most one `in_progress`, a bounded todo
//! count, a closed `status` vocabulary, no extra fields) lives in the manifest's
//! params schema, so the host's `params_validator` refuses a bad call with one
//! structured `SchemaMismatch` before this Component is instantiated. That is
//! why `run` below performs no checks of its own: a second, imperative copy of
//! the rules inside the guest could disagree with the descriptor, and the guest
//! is the side of the boundary whose word the host does not take.
//!
//! Normalization is likewise nothing: the schema is closed and the field order
//! is fixed, so the validated params *are* the normalized plan. `run` echoes
//! them through unchanged.
//!
//! The import surface is empty of domain authority: this Tool imports only
//! `host-control`, and does not even use its `emit-event` — a progress event
//! would be a `Mutate` session-channel effect, and this Tool has none. It probes
//! `cancelled` so a call cancelled before it started stops here rather than
//! producing a result the host would have to discard.

#![no_std]
extern crate alloc;

use alloc::string::String;

// Generated WIT bindings live in `bindings` (the convention `export_tool!`
// relies on). `wit_bindgen` is a direct dep because the generated `_rt` module
// references `wit_bindgen::rt::...` by bare crate path; the prelude re-export
// pins the version and provides the export glue. `export_tool!` is invoked
// INSIDE `bindings` because wit-bindgen's generated `export!` resolves `self`
// at the call site.
mod bindings {
    lattice_tool_prelude::wit_bindgen::generate!({
        path: "generated",
    });
    use crate::TodoWrite;
    lattice_tool_prelude::export_tool!(TodoWrite);
}

use bindings::exports::lattice::tool_host::tool_api::{Guest, ToolOutput};
use bindings::lattice::tool_host::host_control;

struct TodoWrite;

impl Guest for TodoWrite {
    fn run(params_json: String) -> Result<ToolOutput, String> {
        // Cooperative cancellation: the host also enforces an epoch deadline,
        // and its call-scoped cancellation state — not this string — is what
        // classifies the outcome.
        if host_control::cancelled() {
            return Err(String::from("cancelled"));
        }

        Ok(ToolOutput {
            // Trust-free 3-segment CellRef; the host resolves `session` in the
            // descriptor and alone appends `:in` to the audit key. A guest
            // cannot encode trust (`parse_cell_ref` rejects a 4th segment).
            result_cell: String::from("alloc:none:session"),
            // The validated params are the normalized result. Echoing the input
            // document verbatim is exactly the `TodoWriteResult { todos }` the
            // native handler returned, and it cannot drift from the result
            // schema: params and result schemas are the same shape, and the host
            // validates the value against the result schema on the way out.
            value_json: params_json,
        })
    }
}
