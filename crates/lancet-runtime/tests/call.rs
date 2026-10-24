// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Call execution tests (#27 (Codeberg #5) slice C.6/C.7, #28 (Codeberg #6)
//! B3/B5/B7).
//!
//! These tests exercise the `ComponentRuntime::call` path end-to-end:
//! params validation, Component instantiation, per-operation Cell
//! authorization on the ToolHost, result CellRef validation against the
//! descriptor ceiling and the admitted set, result schema validation, and
//! Cell evidence recording.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_protocol::cell::Trust;
use lancet_protocol::evidence::{CapabilityAdmissionPolicy, EffectSource};
use lancet_protocol::{CepErrorCode, InvocationId};
use lancet_runtime::{Cancellation, EventSink};
use lancet_runtime::{CellEvidenceLedger, ComponentCallContext, ComponentRuntime, admit_tool};
use tokio::sync::{mpsc, watch};

/// The profile policy a turn carries: `noop__echo` enabled, nothing denied,
/// `in` boundary. `deny` lists capability names the profile subtracts.
fn policy(deny: &[&str]) -> CapabilityAdmissionPolicy {
    CapabilityAdmissionPolicy {
        enabled_tools: BTreeSet::from(["noop__echo".to_string()]),
        denied_capabilities: deny.iter().map(|name| (*name).to_string()).collect(),
        max_boundary: Trust::In,
    }
}

/// Build a call context with fresh event/cancellation channels, admitting
/// `noop__echo` under `policy` exactly as the transport does before `call`.
/// The returned ledger clone survives the call and carries its evidence.
fn call_ctx_with(
    rt: &ComponentRuntime,
    policy: &CapabilityAdmissionPolicy,
) -> (
    ComponentCallContext,
    CellEvidenceLedger,
    mpsc::UnboundedReceiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let descriptor = rt.descriptor("noop__echo").expect("fixture tool exists");
    let admitted = admit_tool(descriptor, policy).expect("noop__echo is admitted");
    let evidence = CellEvidenceLedger::new();
    evidence.set_admission(admitted.evidence.clone());
    let invocation_id = InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
    let ctx = ComponentCallContext {
        invocation_id,
        turn_id: "test".into(),
        events: EventSink::new(tx, invocation_id),
        cancellation: Cancellation::new(cancel_rx),
        evidence: evidence.clone(),
        admitted: Arc::new(admitted),
        // These fixtures exercise control-only Tools; a call with no workspace
        // authority refuses every observation operation (#30).
        workspace: None,
        mutate: None,
        network: None,
    };
    (ctx, evidence, rx, cancel_tx)
}

/// The common case: every declared capability admitted.
fn call_ctx(
    rt: &ComponentRuntime,
) -> (
    ComponentCallContext,
    CellEvidenceLedger,
    mpsc::UnboundedReceiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
) {
    call_ctx_with(rt, &policy(&[]))
}

/// Evidence: a successful call records two effects — the `emit-event`
/// operation and the result — with host-derived trust, host-computed measure,
/// and the admission evidence that authorized them.
#[tokio::test]
async fn call_records_operation_and_result_evidence() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, evidence, _rx, _cancel) = call_ctx(&rt);

    let result = rt
        .call(
            "noop__echo",
            serde_json::json!({"message": "evidence-test"}),
            ctx,
        )
        .await
        .expect("call succeeds");
    assert_eq!(result, serde_json::json!({"echoed": "evidence-test"}));

    let (admission, effects, denied) = evidence.snapshot();
    let admission = admission.expect("admission evidence recorded");
    assert_eq!(admission.tool, "noop__echo");
    assert_eq!(
        admission.approved_capabilities,
        vec!["session.events".to_string(), "session.result".to_string()]
    );
    assert!(denied.denied.is_empty(), "nothing was refused: {denied:?}");

    // C8: one call, two effects, kept in `seq` order and never collapsed.
    assert_eq!(
        effects
            .effects
            .iter()
            .map(|effect| (effect.seq, effect.source, effect.cell.reference()))
            .collect::<Vec<_>>(),
        vec![
            (
                0,
                EffectSource::Operation,
                "mutate:none:session".to_string()
            ),
            (1, EffectSource::Result, "alloc:none:session".to_string()),
        ]
    );
    // D12: the measure rides only on the Alloc result, host-computed from the
    // canonical result JSON (`{"echoed":"evidence-test"}` — 26 bytes).
    assert_eq!(effects.effects[0].context_measure, None);
    assert_eq!(effects.effects[1].context_measure, Some(26));
    // The operation's target is the call's own turn.
    assert_eq!(
        effects.effects[0].target,
        lancet_protocol::evidence::ObservedTarget::Session {
            turn_id: "test".into()
        }
    );

    // The audit keys carry the host-derived trust segment, in seq order.
    assert_eq!(
        effects
            .effects
            .iter()
            .map(|effect| effect.cell.audit_key(effect.scope.trust))
            .collect::<Vec<_>>(),
        vec![
            "mutate:none:session:in".to_string(),
            "alloc:none:session:in".to_string(),
        ]
    );
}

/// Partial capability admission (B3) and the uncovered-operation case in
/// one: denying `session.events` leaves the tool callable but admits no
/// `mutate:none:session` cell, so `emit-event` is refused — the sink is
/// never invoked and the refusal is recorded as evidence, not as an effect.
#[tokio::test]
async fn partial_deny_refuses_the_operation_but_not_the_call() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, evidence, mut rx, _cancel) = call_ctx_with(&rt, &policy(&["session.events"]));

    let result = rt
        .call("noop__echo", serde_json::json!({"message": "partial"}), ctx)
        .await
        .expect("the call itself still succeeds");
    assert_eq!(result, serde_json::json!({"echoed": "partial"}));

    assert!(
        rx.try_recv().is_err(),
        "the host facility must never be invoked for a denied operation"
    );

    let (admission, effects, denied) = evidence.snapshot();
    let admission = admission.expect("admission evidence recorded");
    assert_eq!(
        admission.approved_capabilities,
        vec!["session.result".to_string()]
    );
    assert_eq!(
        admission.denied_capabilities,
        vec!["session.events".to_string()]
    );

    // Only the result effect happened; the operation is a denial record.
    assert_eq!(effects.effects.len(), 1);
    assert_eq!(effects.effects[0].source, EffectSource::Result);
    assert_eq!(denied.denied.len(), 1);
    assert_eq!(denied.denied[0].seq, 0);
    assert_eq!(denied.denied[0].cell.reference(), "mutate:none:session");
    assert!(
        denied.denied[0]
            .reason
            .contains("not covered by an admitted"),
        "{}",
        denied.denied[0].reason
    );
    // Only observed effects carry host-derived trust; a denial contributes no effect.
    assert_eq!(
        effects
            .effects
            .iter()
            .map(|effect| effect.cell.audit_key(effect.scope.trust))
            .collect::<Vec<_>>(),
        vec!["alloc:none:session:in".to_string()]
    );
}

/// Effect ceiling (D3): a declared-but-not-admitted result cell fails the
/// call closed. The result effect is never recorded, while the operation
/// effect already in the ledger survives into audit (B5).
#[tokio::test]
async fn result_cell_outside_the_admitted_set_fails_closed() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, evidence, mut rx, _cancel) = call_ctx_with(&rt, &policy(&["session.result"]));

    let err = rt
        .call("noop__echo", serde_json::json!({"message": "ceiling"}), ctx)
        .await
        .expect_err("an unadmitted result cell must fail the call");
    assert_eq!(err.code, CepErrorCode::CapabilityViolation);

    // The operation was admitted, so it really happened and is recorded.
    assert!(rx.try_recv().is_ok(), "the event was emitted");
    let (_admission, effects, denied) = evidence.snapshot();
    assert!(denied.denied.is_empty(), "{denied:?}");
    assert_eq!(effects.effects.len(), 1, "{:?}", effects.effects);
    assert_eq!(effects.effects[0].source, EffectSource::Operation);
    assert_eq!(effects.effects[0].cell.reference(), "mutate:none:session");
}
