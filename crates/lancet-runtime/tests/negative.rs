// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Negative call tests: schema mismatch, fuel exhaustion, deadline, and
//! survival proof (#27 slice C.7, Codeberg #5).
//!
//! After each failure, a normal no-op call proves the shared runtime remains
//! usable. Custom-component trap/capability-violation cases require a
//! WIT-matching guest and belong in a dedicated test Tool.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_protocol::cell::Trust;
use lancet_protocol::evidence::CapabilityAdmissionPolicy;
use lancet_protocol::{CepErrorCode, InvocationId};
use lancet_runtime::{Cancellation, EventSink};
use lancet_runtime::{CellEvidenceLedger, ComponentCallContext, ComponentRuntime, admit_tool};
use tokio::sync::{mpsc, watch};

/// A call context with every declared `noop.echo` capability admitted, built
/// the way the transport builds it: admission runs per call, before `call`.
fn call_ctx(
    rt: &ComponentRuntime,
) -> (
    ComponentCallContext,
    mpsc::UnboundedReceiver<lancet_protocol::ToolEvent>,
    watch::Sender<bool>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let descriptor = rt.descriptor("noop.echo").expect("fixture tool exists");
    let admitted = admit_tool(
        descriptor,
        &CapabilityAdmissionPolicy {
            enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
            denied_capabilities: BTreeSet::new(),
            max_boundary: Trust::In,
        },
    )
    .expect("noop.echo is admitted");
    let invocation_id = InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
    let ctx = ComponentCallContext {
        invocation_id,
        turn_id: "test".into(),
        events: EventSink::new(tx, invocation_id),
        cancellation: Cancellation::new(cancel_rx),
        evidence: CellEvidenceLedger::new(),
        admitted: Arc::new(admitted),
        // These fixtures exercise control-only Tools; a call with no workspace
        // authority refuses every observation operation (#30).
        workspace: None,
        mutate: None,
        network: None,
    };
    (ctx, rx, cancel_tx)
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

    let (ctx, _rx, _cancel) = call_ctx(&rt);
    let result = rt
        .call("noop.echo", serde_json::json!({"message": "fuel"}), ctx)
        .await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, CepErrorCode::ToolResourceExhausted);

    // Restore normal limits and call again.
    rt.set_limits(lancet_runtime::ExecutionLimits::default());
    let (ctx2, _rx, _cancel) = call_ctx(&rt);
    let result = rt
        .call(
            "noop.echo",
            serde_json::json!({"message": "survived"}),
            ctx2,
        )
        .await
        .expect("call succeeds after fuel restoration");
    assert_eq!(result, serde_json::json!({"echoed": "survived"}));
}

// ---------------------------------------------------------------------------
// Resource-limit enforcement (#47): each configured limit must reject a real
// Component instantiation/call with `ToolResourceExhausted` and an accurate,
// non-leaking `resource` field, and the runtime must remain usable afterwards.
// drive the enforcement + mapping end-to-end through the real noop Component,
// not just the mapper helper.
// ---------------------------------------------------------------------------

/// Extract the structured `resource` label from a tool-execution error.
fn tool_resource(err: &lancet_protocol::CepError) -> Option<&str> {
    match &err.data {
        lancet_protocol::CepErrorData::ToolExecution(d) => d.resource.as_deref(),
        _ => None,
    }
}

/// Run one call under `limits`, asserting it fails with `ToolResourceExhausted`
/// carrying `resource`, then prove the runtime remains usable with a normal call.
async fn assert_resource_exhausted_then_survives(
    limits: lancet_runtime::ExecutionLimits,
    resource: &str,
) {
    let mut rt = ComponentRuntime::noop_fixture().expect("fixture composes");
    rt.set_limits(limits);

    let (ctx, _rx, _cancel) = call_ctx(&rt);
    let err = rt
        .call("noop.echo", serde_json::json!({"message": "limit"}), ctx)
        .await
        .expect_err("limit must reject");
    assert_eq!(
        err.code,
        CepErrorCode::ToolResourceExhausted,
        "expected resource-exhausted for {resource}, got {err:?}"
    );
    assert_eq!(
        tool_resource(&err),
        Some(resource),
        "wrong resource for {err:?}"
    );

    // The engine must survive: a normal call with default limits still works.
    rt.set_limits(lancet_runtime::ExecutionLimits::default());
    let (ctx2, _rx, _cancel) = call_ctx(&rt);
    let result = rt
        .call(
            "noop.echo",
            serde_json::json!({"message": "survived"}),
            ctx2,
        )
        .await
        .expect("call succeeds after resource-limit rejection");
    assert_eq!(result, serde_json::json!({"echoed": "survived"}));
}

/// Memory-byte limit: a `memory_size` below the guest's initial linear memory
/// is rejected by the limiter's `memory_growing` callback and maps to
/// `resource = "memory"` (previously fell through to `ToolTrap`).
#[tokio::test]
async fn memory_bytes_limit_maps_to_resource_exhausted_and_runtime_survives() {
    assert_resource_exhausted_then_survives(
        lancet_runtime::ExecutionLimits {
            // One page — well below the guest module's initial memory.
            memory_size: 64 * 1024,
            ..Default::default()
        },
        "memory",
    )
    .await;
}

/// Table-element limit: a `table_elements` of 0 is rejected by the limiter's
/// `table_growing` callback and maps to `resource = "table"` (previously
/// misclassified as `"instances"`, and it fell through to `ToolTrap`).
#[tokio::test]
async fn table_elements_limit_maps_to_resource_exhausted_and_runtime_survives() {
    assert_resource_exhausted_then_survives(
        lancet_runtime::ExecutionLimits {
            table_elements: 0,
            ..Default::default()
        },
        "table",
    )
    .await;
}

/// Instance-count limit: the noop Component needs 3 core instances, so a limit
/// of 1 is exceeded during instantiation. Count limits have no rejection
/// callback, so this is classified from the static footprint as
/// `resource = "instances"` — proving the configured count now takes effect.
#[tokio::test]
async fn instances_limit_maps_to_resource_exhausted_and_runtime_survives() {
    assert_resource_exhausted_then_survives(
        lancet_runtime::ExecutionLimits {
            instances: 1,
            ..Default::default()
        },
        "instances",
    )
    .await;
}

/// Memory-count limit: `memories = 0` rejects the guest module's linear memory
/// during instantiation and is classified as `resource = "memories"`.
#[tokio::test]
async fn memories_limit_maps_to_resource_exhausted_and_runtime_survives() {
    assert_resource_exhausted_then_survives(
        lancet_runtime::ExecutionLimits {
            memories: 0,
            ..Default::default()
        },
        "memories",
    )
    .await;
}

/// Table-count limit: the noop Component defines 2 tables, so `tables = 1` is
/// exceeded during instantiation and is classified as `resource = "tables"`
/// (distinct from the per-table `table_elements` limit above).
#[tokio::test]
async fn tables_limit_maps_to_resource_exhausted_and_runtime_survives() {
    assert_resource_exhausted_then_survives(
        lancet_runtime::ExecutionLimits {
            tables: 1,
            ..Default::default()
        },
        "tables",
    )
    .await;
}
