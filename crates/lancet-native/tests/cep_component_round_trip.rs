// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! #5 slice D.6: end-to-end Component dispatch round-trip through the
//! existing native CEP server lifecycle.
//!
//! These tests prove the D.3 dispatch split works: a no-op Component Tool
//! (`noop.echo`) round-trips through the same `CepServer`/audit lifecycle
//! as the 5 production native handlers, producing Cell-attributed audit
//! evidence. The 5 native handlers continue to work unchanged alongside
//! the Component path.
//!
//! The fixed no-op fixture comes from `lancet_runtime::ComponentRuntime::noop_fixture()`
//! (`test-utils` feature). The caller cannot inject bytes, registrations,
//! handlers, paths, or an approval set.
//!
//! #28 B11: the same lifecycle also carries the Cell admission end-to-end
//! scenarios — allow, partial deny, and full deny — with their audit evidence.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_native::{
    AuditQueryFilters, CepAuditStore, CepServerNativeExt, CepTransport, MethodRegistry,
    ServerRunConfig, register_production_method_registry,
};
use lancet_protocol::{CapabilityAdmissionPolicy, Trust};
use lancet_runtime::ComponentRuntime;

/// The noop fixture's admission policy: the tool is enabled, and `deny`
/// subtracts capability names by the profile's stage-2 rule.
fn admission(deny: &[&str]) -> CapabilityAdmissionPolicy {
    CapabilityAdmissionPolicy {
        enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
        denied_capabilities: deny.iter().map(|name| (*name).to_string()).collect(),
        max_boundary: Trust::In,
    }
}

/// Spawn a native CEP server with the no-op Component fixture composed in,
/// plus the production native method registry. Returns the client, audit
/// store, and the server task handle.
async fn spawn_native_with_noop() -> (
    lancet_native::CepClient,
    Arc<CepAuditStore>,
    tokio::task::JoinHandle<()>,
) {
    spawn_native_with_admission(admission(&[])).await
}

/// As above, under an explicit admission policy. The fail-closed default
/// admits no tool at all, so every Component scenario names its policy.
async fn spawn_native_with_admission(
    policy: CapabilityAdmissionPolicy,
) -> (
    lancet_native::CepClient,
    Arc<CepAuditStore>,
    tokio::task::JoinHandle<()>,
) {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests().with_capability_admission(policy);
    config.components = Some(components);

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).unwrap();

    let (client, server) = CepTransport::pair(64);
    let audit = Arc::clone(&config.audit);
    let task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    tokio::task::yield_now().await;
    (client, audit, task)
}

/// The single `noop.echo` audit row's metadata, parsed.
async fn noop_metadata(audit: &CepAuditStore) -> serde_json::Value {
    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters {
            method: Some("noop.echo".to_string()),
            ..Default::default()
        })
        .expect("audit query");
    assert_eq!(rows.len(), 1, "exactly one noop.echo audit row");
    let metadata = rows[0]
        .metadata_json
        .as_deref()
        .expect("a Component call always finalizes with evidence metadata");
    serde_json::from_str(metadata).expect("metadata is JSON")
}

/// D.6 tracer + #28 B11 allow path: `noop.echo` round-trips through CEP +
/// audit with both capabilities admitted, so one call produces two effects
/// (the gated `emit-event` operation and the result) and the audit row keeps
/// "why authorized" apart from "what happened".
#[tokio::test]
async fn embedded_noop_round_trips_through_cep_and_audit() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "ping" }))
        .await
        .expect("component round trip");
    assert_eq!(value, serde_json::json!({ "echoed": "ping" }));

    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters {
            method: Some("noop.echo".to_string()),
            ..Default::default()
        })
        .expect("audit query");
    assert_eq!(rows.len(), 1, "exactly one noop.echo audit row");
    let row = &rows[0];
    let result = row.result_or_error.as_deref().unwrap_or("");
    assert!(
        result.starts_with("success:"),
        "audit outcome must be success, got: {result}"
    );

    let metadata: serde_json::Value =
        serde_json::from_str(row.metadata_json.as_deref().expect("evidence metadata"))
            .expect("metadata is JSON");
    // C8: a multi-effect call keeps every cell, in seq order.
    assert_eq!(
        metadata["component_cells"],
        serde_json::json!(["mutate:none:session:in", "alloc:none:session:in"])
    );
    // B5/A2: approved capability names are admission evidence only.
    assert_eq!(
        metadata["admission_evidence"]["approved_capabilities"],
        serde_json::json!(["session.events", "session.result"])
    );
    assert_eq!(metadata["admission_evidence"]["max_boundary"], "in");
    let effects = metadata["observed_effects"]["effects"]
        .as_array()
        .expect("observed effects array");
    assert_eq!(effects.len(), 2, "emit-event operation + result");
    assert_eq!(effects[0]["source"], "operation");
    assert_eq!(effects[0]["target"]["turn_id"], "test-turn");
    assert_eq!(effects[0]["context_measure"], serde_json::Value::Null);
    assert_eq!(effects[1]["source"], "result");
    // D12: the host measures the canonical result bytes itself.
    assert_eq!(
        effects[1]["context_measure"],
        serde_json::json!(r#"{"echoed":"ping"}"#.len())
    );
    assert!(
        metadata.get("denied_operations").is_none(),
        "nothing was refused on the allow path: {metadata}"
    );
}

/// B11 partial deny: subtracting `session.events` leaves the tool callable,
/// but its `emit-event` operation is refused. The call still succeeds, one
/// effect (the result) is recorded, and the refusal is recorded as evidence.
#[tokio::test]
async fn partial_admission_denies_only_the_uncovered_operation() {
    let (client, audit, _task) = spawn_native_with_admission(admission(&["session.events"])).await;

    let events = client.events();
    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "partial" }))
        .await
        .expect("the tool stays callable with one capability");
    assert_eq!(value, serde_json::json!({ "echoed": "partial" }));

    // B3: the host facility is never reached, so no tool event is emitted.
    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        assert_ne!(
            event.method, "tool.event",
            "a denied operation must not reach the event sink"
        );
    }
    drop(guard);

    let metadata = noop_metadata(&audit).await;
    assert_eq!(
        metadata["admission_evidence"]["approved_capabilities"],
        serde_json::json!(["session.result"])
    );
    assert_eq!(
        metadata["admission_evidence"]["denied_capabilities"],
        serde_json::json!(["session.events"])
    );
    assert_eq!(
        metadata["component_cells"],
        serde_json::json!(["alloc:none:session:in"])
    );
    let denied = metadata["denied_operations"]
        .as_array()
        .expect("denied operations array");
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0]["cell"]["ctx_op"], "mutate");
    assert!(
        denied[0]["reason"]
            .as_str()
            .expect("reason")
            .contains("not covered by an admitted capability"),
        "denial reason must name the uncovered cell: {}",
        denied[0]["reason"]
    );
}

/// B11 full deny, stage 1: a tool outside `[tools].enabled` has no capability
/// at all. Wasmtime is never entered, the client sees `CapabilityNotGranted`,
/// and the audit row still carries the admission evidence that explains it.
#[tokio::test]
async fn unenabled_tool_is_denied_before_the_runtime_is_entered() {
    let (client, audit, _task) =
        spawn_native_with_admission(CapabilityAdmissionPolicy::default()).await;

    let events = client.events();
    let err = client
        .call_raw("noop.echo", serde_json::json!({ "message": "denied" }))
        .await
        .expect_err("the fail-closed default enables no tool");
    assert_eq!(
        err.code,
        lancet_protocol::CepErrorCode::CapabilityNotGranted
    );
    assert!(
        err.message.contains("noop.echo") && err.message.contains("not enabled"),
        "the error must name the tool and the refusing stage: {}",
        err.message
    );

    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        assert_ne!(
            event.method, "tool.event",
            "a denied call must never instantiate the Component"
        );
    }
    drop(guard);

    let metadata = noop_metadata(&audit).await;
    assert_eq!(metadata["admission_evidence"]["tool"], "noop.echo");
    assert_eq!(
        metadata["admission_evidence"]["approved_capabilities"],
        serde_json::json!([])
    );
    assert!(
        metadata.get("component_cells").is_none(),
        "a call that never ran produced no effect: {metadata}"
    );
}

/// B11 full deny, effect ceiling: with `session.result` subtracted the tool is
/// still admitted for its event capability, but its declared result cell is
/// outside the admitted set. The result fails closed with `EffectViolation`
/// while the operation effect already recorded survives into audit (B5).
#[tokio::test]
async fn denied_result_capability_fails_the_call_and_keeps_operation_evidence() {
    let (client, audit, _task) = spawn_native_with_admission(admission(&["session.result"])).await;

    let err = client
        .call_raw("noop.echo", serde_json::json!({ "message": "ceiling" }))
        .await
        .expect_err("an unadmitted result cell fails the call");
    assert_eq!(err.code, lancet_protocol::CepErrorCode::EffectViolation);

    let metadata = noop_metadata(&audit).await;
    assert_eq!(
        metadata["admission_evidence"]["approved_capabilities"],
        serde_json::json!(["session.events"])
    );
    assert_eq!(
        metadata["component_cells"],
        serde_json::json!(["mutate:none:session:in"]),
        "the emit-event effect survives the result-cell violation"
    );
    let effects = metadata["observed_effects"]["effects"]
        .as_array()
        .expect("observed effects array");
    assert_eq!(effects.len(), 1);
    assert_eq!(effects[0]["source"], "operation");
}

/// C2 at the server-construction seam: a deny entry naming no composed
/// capability is a startup error, so the run never begins.
#[tokio::test]
async fn unknown_deny_name_fails_server_construction() {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests()
        .with_capability_admission(admission(&["session.tyop"]));
    config.components = Some(components);

    let (_client, server) = CepTransport::pair(64);
    let err = server
        .run_native(MethodRegistry::new(), config)
        .await
        .expect_err("an unknown deny name must refuse startup");
    assert!(
        err.message.contains("session.tyop"),
        "the startup error must name the unknown deny entry: {}",
        err.message
    );
}

/// D.6: a native method (session.todo_write) still works alongside the
/// Component path — the dispatch split doesn't break the 5 production
/// handlers.
#[tokio::test]
async fn native_method_still_works_alongside_component() {
    use lancet_catalog::methods::session::{TodoItem, TodoStatus, TodoWriteParams};

    let (client, _audit, _task) = spawn_native_with_noop().await;

    let result = client
        .call_named::<TodoWriteParams, serde_json::Value>(
            "session.todo_write",
            TodoWriteParams {
                todos: vec![TodoItem {
                    content: "coexist".into(),
                    status: TodoStatus::Pending,
                }],
            },
        )
        .await
        .expect("native method still works");

    assert!(
        result.get("todos").is_some(),
        "todo_write result must have todos field"
    );
}

/// D.6: the Component call emits events through the CEP event stream.
/// The no-op guest calls `host_control::emit_event("{\"phase\":\"probed\"}")`,
/// which the host forwards as a `tool.event` CEP event. The event must carry
/// the guest's `phase: probed` payload and a `call_id` consistent with the
/// CEP call started/finished events.
#[tokio::test]
async fn component_call_emits_events_with_consistent_call_id() {
    let (client, _audit, _task) = spawn_native_with_noop().await;

    let events = client.events();
    let _value = client
        .call_raw("noop.echo", serde_json::json!({ "message": "events" }))
        .await
        .expect("call succeeds");

    // Drain events. Expect: cep.call_started, tool.event (phase=probed),
    // cep.call_finished — all with the same call_id.
    let mut started_id: Option<u64> = None;
    let mut found_probed = false;
    let mut finished_id: Option<u64> = None;
    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        let cid = event.params.get("call_id").and_then(|v| v.as_u64());
        match event.method.as_str() {
            "cep.call_started" => started_id = cid,
            "cep.call_finished" => finished_id = cid,
            "tool.event" => {
                // The guest's `{"phase":"probed"}` payload is forwarded by
                // `emit_sync`, which adds `call_id` to the object.
                let phase = event.params.get("phase").and_then(|v| v.as_str());
                assert_eq!(
                    phase,
                    Some("probed"),
                    "tool.event must carry the guest's phase=probed payload"
                );
                found_probed = true;
            }
            _ => {}
        }
    }
    assert!(found_probed, "expected a tool.event with phase=probed");
    let started_id = started_id.expect("cep.call_started event");
    let finished_id = finished_id.expect("cep.call_finished event");
    assert_eq!(
        started_id, finished_id,
        "started and finished call ids must match"
    );
}

/// #48: a cancelled Component call converges on `CallCancelled` end to end:
/// the client receives the server-mapped structured error, the audit
/// finalizer records `AuditOutcome::Cancelled`, and the server survives to
/// serve the next call. Cancellation state (never a guest error string) is
/// the classification authority at every observation point.
#[tokio::test]
async fn cancelled_component_call_returns_call_cancelled_and_audits_cancelled() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    // Cancel before awaiting: the server's pre-call check observes the
    // already-set watch, so the outcome is deterministic.
    let (call, control) = client.start_call_raw(
        "noop.echo".to_string(),
        serde_json::json!({ "message": "cancel" }),
    );
    control.cancel();

    let err = call.await.expect_err("cancelled call must fail");
    assert_eq!(
        err.code,
        lancet_protocol::CepErrorCode::CallCancelled,
        "client must receive the server-mapped CallCancelled"
    );

    // The audit finalizer records the cancellation as `cancelled`. The
    // finalize runs before the server responds, so the row is durable by
    // the time the client sees the error.
    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters {
            method: Some("noop.echo".to_string()),
            ..Default::default()
        })
        .expect("audit query");
    assert_eq!(rows.len(), 1, "exactly one noop.echo audit row");
    assert_eq!(
        rows[0].result_or_error.as_deref(),
        Some("cancelled"),
        "audit outcome must be cancelled"
    );

    // A subsequent call works — the server survived.
    let value: serde_json::Value = client
        .call_named(
            "noop.echo",
            serde_json::json!({ "message": "after-cancel" }),
        )
        .await
        .expect("server survives cancellation");
    assert_eq!(value, serde_json::json!({ "echoed": "after-cancel" }));
}

/// #48: dropping the call future mid-flight funnels through the same
/// server-side cancellation mapping (CancelGuard semantics unchanged): the
/// audit row records `cancelled` and the server survives.
#[tokio::test]
async fn dropped_component_call_audits_cancelled_and_server_survives() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    let (call, control) = client.start_call_raw(
        "noop.echo".to_string(),
        serde_json::json!({ "message": "cancel" }),
    );
    control.cancel();
    let handle = tokio::spawn(call);
    // Let the spawned call send its request, then drop it mid-flight.
    tokio::task::yield_now().await;
    handle.abort();
    let _ = handle.await;

    // A subsequent call works — the server survived.
    let value: serde_json::Value = client
        .call_named(
            "noop.echo",
            serde_json::json!({ "message": "after-cancel" }),
        )
        .await
        .expect("server survives cancellation");
    assert_eq!(value, serde_json::json!({ "echoed": "after-cancel" }));

    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters::default())
        .expect("audit");
    assert!(
        rows.iter()
            .any(|r| r.result_or_error.as_deref() == Some("cancelled")),
        "audit must record the dropped call as cancelled"
    );
    assert!(
        rows.iter().any(|r| {
            r.result_or_error
                .as_deref()
                .is_some_and(|s| s.starts_with("success:"))
        }),
        "audit must have a success row after cancel + retry"
    );
}

/// D.6: after a Component call error (method_not_found for a non-existent
/// component method), the server survives and a subsequent call works.
#[tokio::test]
async fn server_survives_after_component_method_not_found() {
    let (client, _audit, _task) = spawn_native_with_noop().await;

    // Call a method that doesn't exist in either the Component catalog or
    // the native registry.
    let err = client
        .call_raw("no.such.tool", serde_json::json!({}))
        .await
        .expect_err("unknown method must error");
    assert_eq!(
        lancet_protocol::error::to_jsonrpc_error(&err).0,
        lancet_protocol::error::jsonrpc::METHOD_NOT_FOUND
    );

    // A subsequent no-op call still works.
    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "survived" }))
        .await
        .expect("call after error");
    assert_eq!(value, serde_json::json!({ "echoed": "survived" }));
}

/// D.6: params schema mismatch on a Component Tool is rejected before
/// entering Wasmtime (the runtime validates params against the descriptor
/// JSON Schema). The server survives.
#[tokio::test]
async fn component_params_schema_mismatch_rejected() {
    let (client, _audit, _task) = spawn_native_with_noop().await;

    // Missing required "message" field.
    let err = client
        .call_raw("noop.echo", serde_json::json!({ "wrong": "field" }))
        .await
        .expect_err("schema mismatch must error");
    assert_eq!(err.code, lancet_protocol::CepErrorCode::SchemaMismatch);

    // Subsequent valid call works.
    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "ok" }))
        .await
        .expect("call after schema error");
    assert_eq!(value, serde_json::json!({ "echoed": "ok" }));
}
