// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Negative call tests: schema mismatch, fuel exhaustion, deadline, and
//! survival proof (#5 slice C.7).
//!
//! After each failure, a normal no-op call proves the engine/server survives.
//! Custom-component trap/effect-violation tests require a WIT-matching
//! guest and are deferred to a dedicated test tool crate.

use lancet_dispatch::{Cancellation, EventSink};
use lancet_protocol::CepErrorCode;
use lancet_runtime::{CellEvidenceLedger, ComponentCallContext, ComponentRuntime};
use tokio::sync::{mpsc, watch};

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

/// Survival proof: after a method_not_found error (which doesn't enter
/// Wasmtime), a normal no-op call still works.
#[tokio::test]
async fn survival_after_method_not_found() {
    let mut rt = ComponentRuntime::noop_fixture().expect("fixture composes");

    // Call a non-existent tool.
    let (ctx_bad, _rx, _cancel) = call_ctx();
    let bad = rt.call("no.such.tool", serde_json::json!({}), ctx_bad).await;
    assert!(bad.is_err());

    // A normal no-op call still works.
    let (ctx_ok, _rx, _cancel) = call_ctx();
    let result = rt
        .call("noop.echo", serde_json::json!({"message": "survived"}), ctx_ok)
        .await
        .expect("call succeeds after error");
    assert_eq!(result, serde_json::json!({"echoed": "survived"}));
}

/// Survival proof: after a params schema mismatch, a normal call works.
#[tokio::test]
async fn survival_after_schema_mismatch() {
    let mut rt = ComponentRuntime::noop_fixture().expect("fixture composes");

    // Bad params.
    let (ctx_bad, _rx, _cancel) = call_ctx();
    let bad = rt
        .call("noop.echo", serde_json::json!({"wrong": "field"}), ctx_bad)
        .await;
    assert!(bad.is_err());

    // Normal call.
    let (ctx_ok, _rx, _cancel) = call_ctx();
    let result = rt
        .call("noop.echo", serde_json::json!({"message": "ok"}), ctx_ok)
        .await
        .expect("call succeeds");
    assert_eq!(result, serde_json::json!({"echoed": "ok"}));
}

/// Survival proof: after a guest error (cancelled), a normal call works.
#[tokio::test]
async fn survival_after_guest_error() {
    let mut rt = ComponentRuntime::noop_fixture().expect("fixture composes");

    // Pre-cancelled call → guest error.
    let (ctx_bad, _rx, cancel_tx) = call_ctx();
    cancel_tx.send(true).unwrap();
    let bad = rt
        .call("noop.echo", serde_json::json!({"message": "x"}), ctx_bad)
        .await;
    assert!(bad.is_err());

    // Normal call.
    let (ctx_ok, _rx, _cancel) = call_ctx();
    let result = rt
        .call("noop.echo", serde_json::json!({"message": "ok"}), ctx_ok)
        .await
        .expect("call succeeds");
    assert_eq!(result, serde_json::json!({"echoed": "ok"}));
}

/// Params schema mismatch: extra/missing fields are rejected before Wasmtime.
#[tokio::test]
async fn call_rejects_extra_params() {
    let mut rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    let (ctx, _rx, _cancel) = call_ctx();

    // The schema has additionalProperties: false and requires "message".
    // Extra field should be rejected.
    let result = rt
        .call(
            "noop.echo",
            serde_json::json!({"message": "ok", "extra": true}),
            ctx,
        )
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, CepErrorCode::SchemaMismatch);
}

/// Fuel exhaustion: with fuel=0, the call fails with ToolResourceExhausted
/// (resource=fuel). The engine survives for subsequent calls with normal fuel.
#[tokio::test]
async fn call_fuel_exhaustion_then_survival() {
    let mut rt = ComponentRuntime::noop_fixture().expect("fixture composes");

    // Set fuel to 0 — the call should fail immediately.
    rt.set_limits(lancet_runtime::ExecutionLimits {
        fuel: 0,
        ..Default::default()
    });

    let (ctx, _rx, _cancel) = call_ctx();
    let result = rt
        .call("noop.echo", serde_json::json!({"message": "fuel"}), ctx)
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, CepErrorCode::ToolResourceExhausted);

    // Restore normal limits and call again.
    rt.set_limits(lancet_runtime::ExecutionLimits::default());
    let (ctx2, _rx, _cancel) = call_ctx();
    let result = rt
        .call("noop.echo", serde_json::json!({"message": "survived"}), ctx2)
        .await
        .expect("call succeeds after fuel restoration");
    assert_eq!(result, serde_json::json!({"echoed": "survived"}));
}

/// Deadline: the noop component completes too fast for the epoch ticker
/// (5ms interval) to interrupt it. This test documents that the epoch-based
/// timeout works correctly for the *callback* path: with a past deadline,
/// the callback returns `Interrupt` on the first epoch tick. For a call
/// that completes before the first tick, the timeout doesn't fire — this
/// is correct behavior (the call finished in time).
///
/// To exercise the timeout path, a long-running component is needed
/// (future: a looping guest). The fuel exhaustion test above covers the
/// synchronous interruption path.
#[tokio::test]
async fn call_with_past_deadline_does_not_crash() {
    let mut rt = ComponentRuntime::noop_fixture().expect("fixture composes");

    // Set a very short deadline. The noop call is fast enough to complete
    // before the epoch ticker fires, so this should succeed (the deadline
    // hasn't been reached when the call finishes).
    rt.set_limits(lancet_runtime::ExecutionLimits {
        deadline: std::time::Duration::from_millis(1),
        ..Default::default()
    });

    let (ctx, _rx, _cancel) = call_ctx();
    // The call should complete normally — the noop is faster than 1ms.
    let result = rt
        .call("noop.echo", serde_json::json!({"message": "fast"}), ctx)
        .await;
    // Either it succeeds (fast enough) or it times out — both are correct.
    // The important thing is the engine survives.
    if let Err(ref err) = result {
        assert!(
            err.code == CepErrorCode::ToolTimeout
                || err.code == CepErrorCode::ToolResourceExhausted,
            "unexpected error code: {err:?}"
        );
    }

    // Restore normal limits and call again — engine must survive.
    rt.set_limits(lancet_runtime::ExecutionLimits::default());
    let (ctx2, _rx, _cancel) = call_ctx();
    let result = rt
        .call("noop.echo", serde_json::json!({"message": "survived"}), ctx2)
        .await
        .expect("call succeeds after short deadline");
    assert_eq!(result, serde_json::json!({"echoed": "survived"}));
}
