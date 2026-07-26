// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Call execution tests (#5 slice C.6/C.7, #28 B3/B5/B7).
//!
//! These tests exercise the `ComponentRuntime::call` path end-to-end:
//! params validation, Component instantiation, per-operation Cell
//! authorization on the ToolHost, result CellRef validation against the
//! descriptor ceiling and the admitted set, result schema validation, and
//! Cell evidence recording.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_dispatch::{Cancellation, EventSink};
use lancet_protocol::CepErrorCode;
use lancet_protocol::cell::Trust;
use lancet_protocol::evidence::{CapabilityAdmissionPolicy, EffectSource};
use lancet_runtime::{CellEvidenceLedger, ComponentCallContext, ComponentRuntime, admit_tool};
use tokio::sync::{mpsc, watch};

/// The profile policy a turn carries: `noop.echo` enabled, nothing denied,
/// `in` boundary. `deny` lists capability names the profile subtracts.
fn policy(deny: &[&str]) -> CapabilityAdmissionPolicy {
    CapabilityAdmissionPolicy {
        enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
        denied_capabilities: deny.iter().map(|name| (*name).to_string()).collect(),
        max_boundary: Trust::In,
    }
}

/// Build a call context with fresh event/cancellation channels, admitting
/// `noop.echo` under `policy` exactly as the transport does before `call`.
/// The returned ledger clone survives the call and carries its evidence.
fn call_ctx_with(
    rt: &ComponentRuntime,
    policy: &CapabilityAdmissionPolicy,
) -> (
    ComponentCallContext,
    CellEvidenceLedger,
    mpsc::Receiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
) {
    let (tx, rx) = mpsc::channel(64);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let descriptor = rt.descriptor("noop.echo").expect("fixture tool exists");
    let admitted = admit_tool(descriptor, policy).expect("noop.echo is admitted");
    let evidence = CellEvidenceLedger::new();
    evidence.set_admission(admitted.evidence.clone());
    let ctx = ComponentCallContext {
        call_id: 1,
        turn_id: "test".into(),
        events: EventSink::new(tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        evidence: evidence.clone(),
        admitted: Arc::new(admitted),
    };
    (ctx, evidence, rx, cancel_tx)
}

/// The common case: every declared capability admitted.
fn call_ctx(
    rt: &ComponentRuntime,
) -> (
    ComponentCallContext,
    CellEvidenceLedger,
    mpsc::Receiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
) {
    call_ctx_with(rt, &policy(&[]))
}

/// Tracer bullet: calling `noop.echo` with valid params returns the echoed
/// result and records Cell evidence.
#[tokio::test]
async fn noop_call_round_trips() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _evidence, _rx, _cancel) = call_ctx(&rt);

    let result = rt
        .call("noop.echo", serde_json::json!({"message": "hello"}), ctx)
        .await
        .expect("call succeeds");

    assert_eq!(result, serde_json::json!({"echoed": "hello"}));
}

/// Calling a non-existent tool returns `method_not_found`.
#[tokio::test]
async fn call_unknown_tool_returns_method_not_found() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _evidence, _rx, _cancel) = call_ctx(&rt);

    let result = rt.call("does.not.exist", serde_json::json!({}), ctx).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    // wire_code for method_not_found is -32601
    assert_eq!(lancet_protocol::error::to_jsonrpc_error(&err).0, -32601);
}

/// Params that don't match the schema are rejected before entering Wasmtime.
#[tokio::test]
async fn call_rejects_bad_params_schema() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _evidence, _rx, _cancel) = call_ctx(&rt);

    // Missing required "message" field.
    let result = rt
        .call("noop.echo", serde_json::json!({"other": "x"}), ctx)
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, CepErrorCode::SchemaMismatch);
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
            "noop.echo",
            serde_json::json!({"message": "evidence-test"}),
            ctx,
        )
        .await
        .expect("call succeeds");
    assert_eq!(result, serde_json::json!({"echoed": "evidence-test"}));

    let (admission, effects, denied) = evidence.snapshot();
    let admission = admission.expect("admission evidence recorded");
    assert_eq!(admission.tool, "noop.echo");
    assert_eq!(
        admission.approved_capabilities,
        vec!["session.events".to_string(), "session.result".to_string()]
    );
    assert!(denied.is_empty(), "nothing was refused: {denied:?}");

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
        evidence.snapshot_keys(),
        vec![
            "mutate:none:session:in".to_string(),
            "alloc:none:session:in".to_string(),
        ]
    );
}

/// Events: the no-op guest emits a progress event via host-control.
#[tokio::test]
async fn call_forwards_guest_events() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _evidence, mut rx, _cancel) = call_ctx(&rt);

    let _result = rt
        .call(
            "noop.echo",
            serde_json::json!({"message": "event-test"}),
            ctx,
        )
        .await
        .expect("call succeeds");

    // The guest emits `{"phase":"probed"}` via host-control::emit_event.
    // The host converts it to a CEP event. Check the event was forwarded.
    let event = rx.try_recv().expect("at least one event");
    assert_eq!(event.method, "tool.event");
    // The payload should contain the phase.
    let payload = &event.params;
    assert!(payload.get("payload").is_some() || payload.get("phase").is_some());
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
        .call("noop.echo", serde_json::json!({"message": "partial"}), ctx)
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
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].seq, 0);
    assert_eq!(denied[0].cell.reference(), "mutate:none:session");
    assert!(
        denied[0].reason.contains("not covered by an admitted"),
        "{}",
        denied[0].reason
    );
    // C8: `component_cells` derives from effects only — a denial contributes
    // no cell key.
    assert_eq!(
        evidence.snapshot_keys(),
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
        .call("noop.echo", serde_json::json!({"message": "ceiling"}), ctx)
        .await
        .expect_err("an unadmitted result cell must fail the call");
    assert_eq!(err.code, CepErrorCode::EffectViolation);

    // The operation was admitted, so it really happened and is recorded.
    assert!(rx.try_recv().is_ok(), "the event was emitted");
    let (_admission, effects, denied) = evidence.snapshot();
    assert!(denied.is_empty(), "{denied:?}");
    assert_eq!(effects.effects.len(), 1, "{:?}", effects.effects);
    assert_eq!(effects.effects[0].source, EffectSource::Operation);
    assert_eq!(effects.effects[0].cell.reference(), "mutate:none:session");
}

/// A tool the profile never enabled is not callable at all (B2).
#[test]
fn disabled_tool_is_not_admitted() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let descriptor = rt.descriptor("noop.echo").expect("fixture tool exists");
    let denial = admit_tool(descriptor, &CapabilityAdmissionPolicy::default())
        .expect_err("the fail-closed default enables no tool");
    assert!(denial.evidence.approved_capabilities.is_empty());
}

/// Cancellation: if the cancellation signal is set before the call, the
/// host short-circuits with `CallCancelled` before entering Wasmtime (#48).
#[tokio::test]
async fn precancelled_call_returns_call_cancelled() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _evidence, _rx, cancel_tx) = call_ctx(&rt);

    // Set cancellation before the call.
    cancel_tx.send(true).unwrap();

    let err = rt
        .call("noop.echo", serde_json::json!({"message": "cancel"}), ctx)
        .await
        .expect_err("pre-cancelled call must fail");
    assert_eq!(err.code, CepErrorCode::CallCancelled);
}

/// `contains` reports whether a method name is in the closed catalog.
/// D.3's dispatch split uses this to branch Component vs native.
#[test]
fn contains_reports_catalog_membership() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    assert!(rt.contains("noop.echo"));
    assert!(!rt.contains("observe.workspace"));
    assert!(!rt.contains("does.not.exist"));
}
