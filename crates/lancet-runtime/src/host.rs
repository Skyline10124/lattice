// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Minimal ToolHost — control-only (#5 slice C).
//!
//! The no-op Tool imports only `host-control` (cancellation probe +
//! structured events). It imports **no** workspace/network/session domain
//! operation: #6 unifies capability admission, and admitting a domain
//! operation before the admission gate exists would create a
//! declare-then-authorize bypass.

use crate::bindings;
use crate::runtime::StoreState;

/// Implement the `host-control` import for `StoreState`.
///
/// - `cancelled`: delegates to the call-scoped `Cancellation`.
/// - `emit_event`: converts the guest payload to a CEP event and forwards
///   it via `EventSink::emit_sync` (best-effort: a closed channel doesn't
///   fail the handler).
impl bindings::lattice::tool_host::host_control::Host for StoreState {
    fn cancelled(&mut self) -> bool {
        self.call.cancellation.is_cancelled()
    }

    fn emit_event(&mut self, payload: String) {
        // The guest sends a JSON string as the event payload. Parse it and
        // forward as a CEP event. Best-effort: a closed/full client channel
        // does not fail the handler.
        let params: serde_json::Value =
            serde_json::from_str(&payload).unwrap_or(serde_json::Value::String(payload));
        self.call.events.emit_sync("tool.event", params);
    }
}
