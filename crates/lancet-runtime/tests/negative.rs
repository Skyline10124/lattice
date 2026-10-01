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

/// Run a limited call, check its original error expectations, then prove runtime survival.
async fn assert_resource_exhausted_then_survives(
    name: &str,
    limits: lancet_runtime::ExecutionLimits,
    initial_message: &str,
    resource: Option<&str>,
) {
    let mut rt =
        ComponentRuntime::noop_fixture().unwrap_or_else(|error| panic!("{name}: {error:?}"));
    rt.set_limits(limits);
    let (ctx, _rx, _cancel) = call_ctx(&rt);
    let result = rt
        .call(
            "noop.echo",
            serde_json::json!({"message": initial_message}),
            ctx,
        )
        .await;
    if resource.is_none() {
        assert!(result.is_err(), "{name}");
    }
    let err = result.expect_err(name);
    assert_eq!(
        err.code,
        CepErrorCode::ToolResourceExhausted,
        "{name}: {err:?}"
    );
    if let Some(resource) = resource {
        assert_eq!(
            tool_resource(&err),
            Some(resource),
            "{name}: wrong resource for {err:?}"
        );
    }
    // Restore defaults on the same runtime, with a fresh context and live cancellation sender.
    rt.set_limits(lancet_runtime::ExecutionLimits::default());
    let (ctx2, _rx, _cancel) = call_ctx(&rt);
    let result = rt
        .call(
            "noop.echo",
            serde_json::json!({"message": "survived"}),
            ctx2,
        )
        .await
        .unwrap_or_else(|error| panic!("{name}: call failed after restoring limits: {error:?}"));
    assert_eq!(result, serde_json::json!({"echoed": "survived"}), "{name}");
}

#[tokio::test]
async fn resource_limits_fail_calls_without_poisoning_the_runtime() {
    use lancet_runtime::ExecutionLimits;
    for (name, limits, initial_message, resource) in [
        // Zero fuel fails immediately; the original test did not check the resource label.
        (
            "call_fuel_exhaustion_then_survival",
            ExecutionLimits {
                fuel: 0,
                ..Default::default()
            },
            "fuel",
            None,
        ),
        // One page is below initial guest memory: memory_growing must report memory, not ToolTrap.
        (
            "memory_bytes_limit_maps_to_resource_exhausted_and_runtime_survives",
            ExecutionLimits {
                memory_size: 64 * 1024,
                ..Default::default()
            },
            "limit",
            Some("memory"),
        ),
        // table_growing must report table, not instances or ToolTrap.
        (
            "table_elements_limit_maps_to_resource_exhausted_and_runtime_survives",
            ExecutionLimits {
                table_elements: 0,
                ..Default::default()
            },
            "limit",
            Some("table"),
        ),
        // Noop needs 3 instances; count limits have no callback and use the static footprint.
        (
            "instances_limit_maps_to_resource_exhausted_and_runtime_survives",
            ExecutionLimits {
                instances: 1,
                ..Default::default()
            },
            "limit",
            Some("instances"),
        ),
        // Zero memories rejects the guest's linear memory during instantiation.
        (
            "memories_limit_maps_to_resource_exhausted_and_runtime_survives",
            ExecutionLimits {
                memories: 0,
                ..Default::default()
            },
            "limit",
            Some("memories"),
        ),
        // Noop defines 2 tables; this is distinct from the per-table element limit.
        (
            "tables_limit_maps_to_resource_exhausted_and_runtime_survives",
            ExecutionLimits {
                tables: 1,
                ..Default::default()
            },
            "limit",
            Some("tables"),
        ),
    ] {
        assert_resource_exhausted_then_survives(name, limits, initial_message, resource).await;
    }
}
