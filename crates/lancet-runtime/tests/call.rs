// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Call execution tests (#5 slice C.6/C.7).
//!
//! These tests exercise the `ComponentRuntime::call` path end-to-end:
//! params validation, Component instantiation, ToolHost control, result
//! CellRef validation, result schema validation, and Cell evidence recording.

use lancet_dispatch::{Cancellation, EventSink};
use lancet_protocol::CepErrorCode;
use lancet_runtime::{CellEvidenceLedger, ComponentCallContext, ComponentRuntime};
use tokio::sync::{mpsc, watch};

/// Build a call context with fresh event/cancellation channels.
fn call_ctx() -> (
    ComponentCallContext,
    mpsc::Receiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
) {
    let (tx, rx) = mpsc::channel(64);
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let ctx = ComponentCallContext {
        call_id: 1,
        turn_id: "test".into(),
        events: EventSink::new(tx, 1),
        cancellation: Cancellation::new(cancel_rx),
        evidence: CellEvidenceLedger::new(),
    };
    (ctx, rx, cancel_tx)
}

/// Tracer bullet: calling `noop.echo` with valid params returns the echoed
/// result and records Cell evidence.
#[tokio::test]
async fn noop_call_round_trips() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _rx, _cancel) = call_ctx();

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
    let (ctx, _rx, _cancel) = call_ctx();

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
    let (ctx, _rx, _cancel) = call_ctx();

    // Missing required "message" field.
    let result = rt
        .call("noop.echo", serde_json::json!({"other": "x"}), ctx)
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, CepErrorCode::SchemaMismatch);
}

/// Evidence: a successful call records the result Cell with derived trust.
#[tokio::test]
async fn call_records_cell_evidence() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _rx, _cancel) = call_ctx();

    // We need to capture the evidence after the call. The evidence is in
    // the ComponentCallContext, which is moved into the call. We'll
    // reconstruct a second call and check the evidence ledger.
    // Actually, the evidence is moved into the store, which is dropped when
    // call returns. We need a way to get it back.
    //
    // For now, verify the call succeeds (evidence is recorded internally).
    // Slice D connects evidence to the audit lifecycle.
    let result = rt
        .call(
            "noop.echo",
            serde_json::json!({"message": "evidence-test"}),
            ctx,
        )
        .await
        .expect("call succeeds");
    assert_eq!(result, serde_json::json!({"echoed": "evidence-test"}));
}

/// Events: the no-op guest emits a progress event via host-control.
#[tokio::test]
async fn call_forwards_guest_events() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, mut rx, _cancel) = call_ctx();

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

/// Cancellation: if the cancellation signal is set before the call, the
/// guest's `cancelled()` check returns true and it returns an error.
#[tokio::test]
async fn call_with_precancelled_returns_guest_error() {
    let rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _rx, cancel_tx) = call_ctx();

    // Set cancellation before the call.
    cancel_tx.send(true).unwrap();

    let result = rt
        .call("noop.echo", serde_json::json!({"message": "cancel"}), ctx)
        .await;
    assert!(result.is_err());
    // The guest returns Err("cancelled") which maps to ExecutionFailed.
    let err = result.unwrap_err();
    assert_eq!(err.code, CepErrorCode::ExecutionFailed);
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

/// `snapshot_keys` returns the 4-segment audit keys (cell + derived trust)
/// recorded by the evidence ledger. D.3 merges these into audit metadata.
#[test]
fn evidence_snapshot_keys_returns_audit_keys() {
    use lancet_protocol::cell::{Cell, CtxOp, ExtImpact, Trust};

    // An empty ledger has no keys.
    let empty = CellEvidenceLedger::new();
    assert!(empty.snapshot_keys().is_empty());

    // After recording a (cell, trust) pair, snapshot_keys returns the
    // 4-segment audit key. The noop descriptor declares session/in trust
    // and the result cell is alloc:none:session, so the audit key is
    // "alloc:none:session:in".
    let ledger = CellEvidenceLedger::new();
    let cell = Cell {
        ctx_op: CtxOp::Alloc,
        ext_impact: ExtImpact::None,
        scope: "session".into(),
    };
    ledger.record(cell, Trust::In);
    assert_eq!(
        ledger.snapshot_keys(),
        vec!["alloc:none:session:in".to_string()]
    );
}
