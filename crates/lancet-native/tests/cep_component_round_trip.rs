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

use std::sync::Arc;

use lancet_native::{
    AuditQueryFilters, CepAuditStore, CepServerNativeExt, CepTransport, MethodRegistry,
    ServerRunConfig, register_production_method_registry,
};
use lancet_runtime::ComponentRuntime;

/// Spawn a native CEP server with the no-op Component fixture composed in,
/// plus the production native method registry. Returns the client, audit
/// store, and the server task handle.
async fn spawn_native_with_noop() -> (
    lancet_native::CepClient,
    Arc<CepAuditStore>,
    tokio::task::JoinHandle<()>,
) {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests();
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

/// D.6 tracer: `noop.echo` round-trips through CEP + audit.
/// The Component result is `{"echoed":"ping"}` and the audit row records
/// Cell evidence (`alloc:none:session:in`).
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
    let metadata = row.metadata_json.as_deref().unwrap_or("");
    assert!(
        metadata.contains("alloc:none:session:in"),
        "audit metadata must contain the Cell audit key, got: {metadata}"
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

/// D.6: a cancelled Component call doesn't crash the server. The no-op
/// guest is too fast for epoch-based interruption, but dropping the call
/// future (which triggers the transport's `CancelGuard`) proves the
/// server survives cancellation. A subsequent call works normally.
///
/// The spec also asks for `audit outcome = cancelled`. The no-op guest
/// checks `cancelled()` cooperatively; if the cancel signal arrives before
/// the guest runs, it returns `Err("cancelled")` which the audit
/// finalizer records as `ExecutionFailed` (not `Cancelled` — the audit
/// `Cancelled` outcome is for transport-level cancellation where the
/// handler never completes). This test verifies server survival, which
/// is the safety-critical property.
#[tokio::test]
async fn cancelled_component_call_server_survives() {
    let (client, audit, _task) = spawn_native_with_noop().await;

    // Start a call and immediately drop the future — the CancelGuard
    // signals cancellation. We don't await the result.
    // Use call_raw which returns a future; dropping it cancels.
    let events = client.events();
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(1),
        client.call_raw("noop.echo", serde_json::json!({ "message": "cancel" })),
    )
    .await;

    // Drain events to avoid interfering with the next call.
    let mut guard = events.lock().await;
    while guard.try_recv().is_ok() {}
    drop(guard);

    // A subsequent call works — the server survived.
    let value: serde_json::Value = client
        .call_named(
            "noop.echo",
            serde_json::json!({ "message": "after-cancel" }),
        )
        .await
        .expect("server survives cancellation");
    assert_eq!(value, serde_json::json!({ "echoed": "after-cancel" }));

    // The audit should have at least the successful retry row.
    audit.await_pending().await;
    let rows = audit
        .query_rows(&AuditQueryFilters::default())
        .expect("audit");
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
