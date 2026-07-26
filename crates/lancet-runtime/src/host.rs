// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Minimal ToolHost — control-only, with per-operation Cell authorization
//! (#28 B3/D2).
//!
//! The no-op Tool imports only `host-control` (cancellation probe +
//! structured events). It imports **no** workspace/network/session domain
//! operation; #6 adds handle-per-capability domain imports, and each will
//! bind its own cell through the same [`authorize_operation`] seam.
//!
//! Host-side binding (D2): `emit-event ⇒ Cell{Mutate, None, <the tool's
//! session-channel scope>}` with concrete target `Session{turn_id}`. Guests
//! never name a cell for an operation. `cancelled` is a pure control probe —
//! no cell, never gated.

use lancet_protocol::cell::{Cell, ChannelKind, CtxOp, ExtImpact, ScopeDecl, Trust};
use lancet_protocol::evidence::{EffectSource, ObservedScope, ObservedTarget};

use crate::admission::{authorize_operation, resolve_scope};
use crate::bindings;
use crate::runtime::StoreState;

/// Scope recorded when an operation cannot be bound to exactly one scope.
/// Not a legal manifest scope name, so it can never collide with a declared
/// one.
const UNBOUND_SCOPE: &str = "<unbound>";

/// Implement the `host-control` import for `StoreState`.
///
/// - `cancelled`: delegates to the call-scoped `Cancellation`.
/// - `emit_event`: authorizes the operation's cell + concrete target against
///   the admitted capability set, then converts the guest payload to a CEP
///   event and forwards it via `EventSink::emit_sync` (best-effort: a closed
///   channel doesn't fail the handler). A denial rejects only this operation:
///   the sink is never reached and the call continues.
impl bindings::lattice::tool_host::host_control::Host for StoreState {
    fn cancelled(&mut self) -> bool {
        self.call.cancellation.is_cancelled()
    }

    fn emit_event(&mut self, payload: String) {
        let descriptor = &self.call.admitted.descriptor;
        let target = ObservedTarget::Session {
            turn_id: self.call.turn_id.clone(),
        };

        // Fail-closed binding: with zero or several session-channel scopes
        // the host cannot say which scope the event belongs to, and it does
        // not guess.
        let sessions: Vec<&ScopeDecl> = descriptor
            .scopes
            .iter()
            .filter(|scope| scope.channel == ChannelKind::Session)
            .collect();
        let [session] = sessions.as_slice() else {
            self.call.evidence.record_denied(
                Cell {
                    ctx_op: CtxOp::Mutate,
                    ext_impact: ExtImpact::None,
                    scope: UNBOUND_SCOPE.into(),
                },
                ObservedScope {
                    scope: UNBOUND_SCOPE.into(),
                    trust: Trust::Ex,
                    channel: ChannelKind::Session,
                },
                target,
                format!(
                    "emit-event has no unique session-channel scope ({} declared)",
                    sessions.len()
                ),
            );
            return;
        };

        let cell = Cell {
            ctx_op: CtxOp::Mutate,
            ext_impact: ExtImpact::None,
            scope: session.name.clone(),
        };
        let scope = resolve_scope(descriptor, &session.name, ChannelKind::Session);

        if let Err(denial) =
            authorize_operation(&self.call.admitted, &cell, &target, &self.call.turn_id)
        {
            self.call
                .evidence
                .record_denied(cell, scope, target, denial.to_string());
            return;
        }

        // The guest sends a JSON string as the event payload. Parse it and
        // forward as a CEP event. Best-effort: a closed/full client channel
        // does not fail the handler.
        let params: serde_json::Value =
            serde_json::from_str(&payload).unwrap_or(serde_json::Value::String(payload));
        self.call.events.emit_sync("tool.event", params);
        self.call.evidence.record_effect(
            cell,
            scope,
            target,
            EffectSource::Operation,
            // Only a result cell allocates context (D12).
            None,
        );
    }
}
