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
            // An operation returns no value, so there is nothing to measure
            // (D12).
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use lancet_dispatch::{Cancellation, EventSink};
    use lancet_protocol::ToolEvent;
    use lancet_protocol::cell::{ArtifactIdentity, Capability, ToolDescriptor};
    use lancet_protocol::evidence::CapabilityAdmissionPolicy;
    use tokio::sync::{mpsc, watch};

    use crate::admission::admit_tool;
    use crate::bindings::lattice::tool_host::host_control::Host;
    use crate::evidence::CellEvidenceLedger;
    use crate::limits::{ExecutionLimits, RecordingLimits, ResourceFootprint};
    use crate::runtime::{ComponentCallContext, StoreState};

    fn scope_decl(name: &str, channel: ChannelKind) -> ScopeDecl {
        ScopeDecl {
            name: name.into(),
            trust: Trust::In,
            channel,
        }
    }

    /// A descriptor with exactly `scopes` declared and one admissible
    /// `mutate:none:<cell_scope>` capability, so admission never masks the
    /// binding under test.
    fn descriptor(scopes: Vec<ScopeDecl>, cell_scope: &str) -> Arc<ToolDescriptor> {
        Arc::new(ToolDescriptor {
            name: "synthetic.tool".into(),
            version: "0.1.0".into(),
            description: "operation-binding fixture".into(),
            params_schema: serde_json::Value::Null,
            result_schema: serde_json::Value::Null,
            scopes,
            capabilities: vec![Capability {
                name: "session.events".into(),
                cell: Cell {
                    ctx_op: CtxOp::Mutate,
                    ext_impact: ExtImpact::None,
                    scope: cell_scope.into(),
                },
            }],
            artifact: ArtifactIdentity {
                sha256: "0".repeat(64),
                toolchain: "test".into(),
            },
            abi_version: 1,
        })
    }

    /// Drive one `emit_event` against a store built from `descriptor`, and
    /// return the call's evidence plus the client's event receiver.
    fn emit_event_on(
        descriptor: Arc<ToolDescriptor>,
    ) -> (CellEvidenceLedger, mpsc::Receiver<ToolEvent>) {
        let admitted = admit_tool(
            descriptor,
            &CapabilityAdmissionPolicy {
                enabled_tools: BTreeSet::from(["synthetic.tool".to_string()]),
                denied_capabilities: BTreeSet::new(),
                max_boundary: Trust::In,
            },
        )
        .expect("the fixture capability is admitted");
        let (tx, rx) = mpsc::channel(4);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let evidence = CellEvidenceLedger::new();
        let mut state = StoreState {
            call: ComponentCallContext {
                call_id: 1,
                turn_id: "turn-1".into(),
                events: EventSink::new(tx, 1),
                cancellation: Cancellation::new(cancel_rx),
                evidence: evidence.clone(),
                admitted: Arc::new(admitted),
            },
            limits: RecordingLimits::new(&ExecutionLimits::default()),
            deadline: Instant::now() + Duration::from_secs(60),
            abort: None,
            count_limits: ResourceFootprint::default(),
        };
        state.emit_event(r#"{"phase":"probed"}"#.into());
        (evidence, rx)
    }

    fn assert_denied_unbound(
        evidence: &CellEvidenceLedger,
        rx: &mut mpsc::Receiver<ToolEvent>,
        declared: usize,
    ) {
        assert!(
            rx.try_recv().is_err(),
            "the host facility must never be reached for an unbound operation"
        );
        let (_, effects, denied) = evidence.snapshot();
        assert!(effects.effects.is_empty(), "{:?}", effects.effects);
        assert_eq!(denied.denied.len(), 1);
        assert_eq!(denied.denied[0].cell.scope, UNBOUND_SCOPE);
        assert_eq!(denied.denied[0].scope.scope, UNBOUND_SCOPE);
        assert_eq!(denied.denied[0].scope.trust, Trust::Ex);
        assert_eq!(
            denied.denied[0].target,
            ObservedTarget::Session {
                turn_id: "turn-1".into()
            }
        );
        assert_eq!(
            denied.denied[0].reason,
            format!("emit-event has no unique session-channel scope ({declared} declared)")
        );
    }

    /// D2 fail-closed binding: with no session-channel scope the host cannot
    /// name the event's scope, so the operation is denied under the sentinel.
    #[test]
    fn emit_event_without_a_session_scope_is_denied_unbound() {
        let (evidence, mut rx) = emit_event_on(descriptor(
            vec![scope_decl("workspace", ChannelKind::Workspace)],
            "workspace",
        ));
        assert_denied_unbound(&evidence, &mut rx, 0);
    }

    /// The same binding refuses to guess between two session-channel scopes.
    #[test]
    fn emit_event_with_two_session_scopes_is_denied_unbound() {
        let (evidence, mut rx) = emit_event_on(descriptor(
            vec![
                scope_decl("session", ChannelKind::Session),
                scope_decl("chat", ChannelKind::Session),
            ],
            "session",
        ));
        assert_denied_unbound(&evidence, &mut rx, 2);
    }

    /// The bound case, for contrast: exactly one session-channel scope binds
    /// the operation to it, the sink is reached, and the effect is recorded.
    #[test]
    fn emit_event_with_one_session_scope_reaches_the_sink() {
        let (evidence, mut rx) = emit_event_on(descriptor(
            vec![
                scope_decl("session", ChannelKind::Session),
                scope_decl("workspace", ChannelKind::Workspace),
            ],
            "session",
        ));

        assert_eq!(
            rx.try_recv().expect("the event was forwarded").method,
            "tool.event"
        );
        let (_, effects, denied) = evidence.snapshot();
        assert!(denied.denied.is_empty(), "{denied:?}");
        assert_eq!(effects.effects.len(), 1);
        assert_eq!(effects.effects[0].cell.reference(), "mutate:none:session");
        assert_eq!(effects.effects[0].context_measure, None);
    }
}
