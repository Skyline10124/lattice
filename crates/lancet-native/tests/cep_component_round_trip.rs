// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! #27/#28 Component dispatch round-trip through native CEP server lifecycle.
//! Recovery rewrite: every call supplies a reserved typed InvocationBinding.

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_authority::CepTurnContext;
use lancet_native::{
    CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::cell::CtxOp;
use lancet_protocol::evidence::{
    EffectSource, NetworkPolicyVerdict, NetworkResolution, ObservedTarget,
};
use lancet_protocol::{CapabilityAdmissionPolicy, CepError, CepErrorCode, Trust};
use lancet_runtime::{ComponentRuntime, ExecutionLimits};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn binding(method: &str, params: &serde_json::Value) -> lancet_protocol::InvocationBinding {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    lancet_protocol::InvocationBinding {
        invocation_id: lancet_protocol::InvocationId::parse(&format!(
            "00000000-0000-4000-8000-{ordinal:012x}"
        ))
        .expect("invocation id"),
        identity: lancet_protocol::BindingIdentity {
            session_id: lancet_protocol::SessionId::parse("00000000000000000000000000000001")
                .expect("session id"),
            turn_id: lancet_protocol::TurnId::parse("00000000-0000-4000-8000-000000000001")
                .expect("turn id"),
            model_step_ordinal: ordinal as u32,
            part_ordinal: 0,
            method: method.to_string(),
            descriptor_identity: "test-descriptor".to_string(),
            arguments_digest: lancet_protocol::arguments_digest(params),
        },
    }
}

fn reserve(config: &ServerRunConfig, binding: &lancet_protocol::InvocationBinding) {
    config
        .invocations
        .reserve_batch(
            lancet_protocol::PlanKey {
                turn_id: binding.identity.turn_id,
                model_step_ordinal: binding.identity.model_step_ordinal,
            },
            std::slice::from_ref(binding),
        )
        .expect("reserve invocation");
}

async fn call_receipt(
    client: &lancet_native::CepClient,
    config: &ServerRunConfig,
    method: &str,
    params: serde_json::Value,
) -> Result<lancet_dispatch::ExecutionReceipt, CepError> {
    let binding = binding(method, &params);
    reserve(config, &binding);
    client
        .call_raw_with_receipt(binding, method.to_string(), params)
        .await
}

async fn call_raw(
    client: &lancet_native::CepClient,
    config: &ServerRunConfig,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, CepError> {
    call_receipt(client, config, method, params).await?.result
}

fn admission(deny: &[&str]) -> CapabilityAdmissionPolicy {
    CapabilityAdmissionPolicy {
        enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
        denied_capabilities: deny.iter().map(|name| (*name).to_string()).collect(),
        max_boundary: Trust::In,
    }
}

async fn spawn_native_with_noop() -> (
    lancet_native::CepClient,
    ServerRunConfig,
    tokio::task::JoinHandle<()>,
) {
    spawn_native_with_admission(admission(&[])).await
}
async fn spawn_native_with_admission(
    policy: CapabilityAdmissionPolicy,
) -> (
    lancet_native::CepClient,
    ServerRunConfig,
    tokio::task::JoinHandle<()>,
) {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests().with_capability_admission(policy);
    config.components = Some(components);

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).unwrap();
    let (client, server) = CepTransport::pair(64);
    let config_for_calls = config.clone();
    let task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    tokio::task::yield_now().await;
    (client, config_for_calls, task)
}

async fn spawn_fetch_server_with_deadline(
    allow_localhost: bool,
    max_response_bytes: usize,
    deadline: Option<std::time::Duration>,
) -> (
    lancet_native::CepClient,
    ServerRunConfig,
    tokio::task::JoinHandle<()>,
) {
    let admission = CapabilityAdmissionPolicy {
        enabled_tools: BTreeSet::from(["fetch.web".to_string()]),
        denied_capabilities: BTreeSet::new(),
        max_boundary: Trust::Ex,
    };
    let mut component_runtime =
        ComponentRuntime::from_embedded().expect("closed built-in composition");
    if let Some(deadline) = deadline {
        component_runtime.set_limits(ExecutionLimits {
            deadline,
            ..ExecutionLimits::default()
        });
    }
    let components = Arc::new(component_runtime);
    let mut config = ServerRunConfig::in_memory_for_tests();
    let mut turn_policy = config.cep_context.policy.clone();
    turn_policy.capability_admission = admission;
    turn_policy.network.allow_localhost = allow_localhost;
    turn_policy.network.allowed_hosts = vec!["127.0.0.1".into()];
    turn_policy.network.max_web_response_bytes = max_response_bytes;
    config.cep_context = Arc::new(CepTurnContext::from_policy(turn_policy));
    config.components = Some(components);

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).unwrap();
    let (client, server) = CepTransport::pair(64);
    let config_for_calls = config.clone();
    let task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    tokio::task::yield_now().await;
    (client, config_for_calls, task)
}

async fn spawn_fetch_server(
    allow_localhost: bool,
    max_response_bytes: usize,
) -> (
    lancet_native::CepClient,
    ServerRunConfig,
    tokio::task::JoinHandle<()>,
) {
    spawn_fetch_server_with_deadline(allow_localhost, max_response_bytes, None).await
}

async fn spawn_body_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{addr}/")
}

async fn spawn_redirect_server(
    body: &'static str,
) -> (String, tokio::sync::oneshot::Receiver<String>) {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let redirect = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let redirect_addr = redirect.local_addr().unwrap();
    let (request_tx, request_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let (mut stream, _) = target.accept().await.unwrap();
        let mut request = [0_u8; 2048];
        let size = stream.read(&mut request).await.unwrap();
        let _ = request_tx.send(String::from_utf8_lossy(&request[..size]).into_owned());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    tokio::spawn(async move {
        let (mut stream, _) = redirect.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await;
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{target_addr}/final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    (format!("http://{redirect_addr}/start"), request_rx)
}

async fn spawn_redirect_to(location: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await;
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{addr}/start")
}

async fn spawn_stalled_server() -> (String, tokio::sync::oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request).await;
        let _ = started_tx.send(());
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    });
    (format!("http://{addr}/"), started_rx)
}

#[tokio::test]
async fn embedded_noop_round_trips_with_cell_evidence_receipt() {
    let (client, config, _task) = spawn_native_with_noop().await;

    let receipt = call_receipt(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({ "message": "ping" }),
    )
    .await
    .expect("component round trip");
    assert_eq!(
        receipt.result.expect("tool result"),
        serde_json::json!({ "echoed": "ping" })
    );

    let evidence = receipt.evidence;
    assert_eq!(
        evidence.admission.approved_capabilities,
        vec!["session.events".to_string(), "session.result".to_string()]
    );
    assert_eq!(evidence.admission.max_boundary, Trust::In);
    let effects = &evidence.effects.effects;
    assert_eq!(effects.len(), 2, "emit-event operation + result");
    assert_eq!(effects[0].source, EffectSource::Operation);
    assert_eq!(
        effects[0].target,
        ObservedTarget::Session {
            turn_id: "00000000-0000-4000-8000-000000000001".into()
        }
    );
    assert_eq!(effects[1].source, EffectSource::Result);
    assert_eq!(
        effects[1].context_measure,
        Some(r#"{"echoed":"ping"}"#.len() as u64)
    );
    assert!(evidence.denied.denied.is_empty());
}

#[tokio::test]
async fn partial_admission_denies_only_the_uncovered_operation() {
    let (client, config, _task) = spawn_native_with_admission(admission(&["session.events"])).await;

    let events = client.events();
    let receipt = call_receipt(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({ "message": "partial" }),
    )
    .await
    .expect("the tool stays callable with one capability");
    assert_eq!(
        receipt.result.expect("tool result"),
        serde_json::json!({ "echoed": "partial" })
    );

    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        assert_ne!(
            event.method, "tool.event",
            "a denied operation must not reach the event sink"
        );
    }
    drop(guard);

    let evidence = receipt.evidence;
    assert_eq!(
        evidence.admission.approved_capabilities,
        vec!["session.result".to_string()]
    );
    assert_eq!(
        evidence.admission.denied_capabilities,
        vec!["session.events".to_string()]
    );
    assert_eq!(evidence.denied.denied.len(), 1);
    assert_eq!(evidence.denied.denied[0].cell.ctx_op, CtxOp::Mutate);
}

#[tokio::test]
async fn unenabled_tool_is_denied_before_the_runtime_is_entered() {
    let (client, config, _task) =
        spawn_native_with_admission(CapabilityAdmissionPolicy::default()).await;

    let events = client.events();
    let err = call_raw(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({ "message": "denied" }),
    )
    .await
    .expect_err("the fail-closed default enables no tool");
    assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);

    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        assert_ne!(
            event.method, "tool.event",
            "a denied call must never instantiate the Component"
        );
    }
    drop(guard);
}

#[tokio::test]
async fn fetch_web_component_round_trips_with_transport_evidence() {
    let url = spawn_body_server("ok").await;
    let (client, config, _task) = spawn_fetch_server(true, 1024).await;
    let receipt = call_receipt(
        &client,
        &config,
        "fetch.web",
        serde_json::json!({"url": url}),
    )
    .await
    .expect("transport receipt");
    assert_eq!(
        receipt.result.expect("fetch result"),
        serde_json::json!({
            "status": 200,
            "content_type": "text/plain",
            "body": "ok",
            "truncated": false,
            "final_origin": url.trim_end_matches('/'),
            "redirect_count": 0
        })
    );

    let operation = receipt
        .evidence
        .effects
        .effects
        .iter()
        .find(|effect| effect.network.is_some())
        .expect("network operation evidence");
    let network = operation.network.as_ref().unwrap();
    assert_eq!(network.hop, 0);
    assert_eq!(network.resolution, NetworkResolution::Literal);
    assert_eq!(network.policy_verdict, NetworkPolicyVerdict::Allowed);
    assert_eq!(network.http_status, Some(200));
    assert_eq!(network.body_bytes_returned, Some(2));
    assert!(network.transport.connected_peer.is_some());
}

#[tokio::test]
async fn fetch_web_policy_denial_happens_before_network_io() {
    let (client, config, _task) = spawn_fetch_server(false, 1024).await;
    let receipt = call_receipt(
        &client,
        &config,
        "fetch.web",
        serde_json::json!({"url": "http://127.0.0.1:9/"}),
    )
    .await
    .expect("transport receipt");
    assert_eq!(
        receipt.result.expect_err("localhost is disabled").code,
        CepErrorCode::AmbientAuthorityDenied
    );
    assert!(receipt.evidence.effects.effects.is_empty());
    let denial = receipt.evidence.denied.denied.first().unwrap();
    assert_eq!(
        denial.network.as_ref().unwrap().policy_verdict,
        NetworkPolicyVerdict::Denied
    );
}

#[tokio::test]
async fn fetch_web_follows_redirect_and_caps_shaped_body() {
    let (url, target_request) = spawn_redirect_server("abcdef").await;
    let (client, config, _task) = spawn_fetch_server(true, 4).await;
    let receipt = call_receipt(
        &client,
        &config,
        "fetch.web",
        serde_json::json!({
            "url": url,
            "headers": {"x-secret": "credential"}
        }),
    )
    .await
    .expect("transport receipt");
    let result = receipt.result.expect("redirected fetch result");
    assert_eq!(result["body"], "abcd");
    assert_eq!(result["truncated"], true);
    assert_eq!(result["redirect_count"], 1);

    let hops: Vec<_> = receipt
        .evidence
        .effects
        .effects
        .iter()
        .filter_map(|effect| effect.network.as_ref())
        .collect();
    assert_eq!(hops.len(), 2);
    assert_eq!((hops[0].hop, hops[0].http_status), (0, Some(302)));
    assert_eq!((hops[1].hop, hops[1].http_status), (1, Some(200)));
    assert_eq!(hops[1].body_bytes_returned, Some(4));

    let target_request = target_request.await.expect("capture redirected request");
    assert!(
        !target_request.to_ascii_lowercase().contains("x-secret:"),
        "cross-origin redirect leaked caller headers: {target_request}"
    );
}

#[tokio::test]
async fn fetch_web_revalidates_redirect_target_before_connecting() {
    let url = spawn_redirect_to("http://10.0.0.1/private").await;
    let (client, config, _task) = spawn_fetch_server(true, 1024).await;
    let receipt = call_receipt(
        &client,
        &config,
        "fetch.web",
        serde_json::json!({"url": url}),
    )
    .await
    .expect("transport receipt");
    assert_eq!(
        receipt.result.expect_err("private redirect must fail").code,
        CepErrorCode::AmbientAuthorityDenied
    );
    assert_eq!(receipt.evidence.effects.effects.len(), 1);
    let denial = receipt.evidence.denied.denied.first().unwrap();
    let network = denial.network.as_ref().unwrap();
    assert_eq!(network.hop, 1);
    assert_eq!(network.policy_verdict, NetworkPolicyVerdict::Denied);
}

#[tokio::test]
async fn fetch_web_cancellation_interrupts_in_flight_io() {
    let (url, started) = spawn_stalled_server().await;
    let (client, config, _task) = spawn_fetch_server(true, 1024).await;
    let params = serde_json::json!({"url": url});
    let binding = binding("fetch.web", &params);
    reserve(&config, &binding);
    let (call, control) = client.start_call_raw(binding, "fetch.web".to_string(), params);
    let call = tokio::spawn(call);
    started.await.expect("request reached fixture");
    control.cancel();
    let error = call
        .await
        .expect("call task")
        .expect_err("cancelled fetch must fail");
    assert_eq!(error.code, CepErrorCode::CallCancelled);
}

#[tokio::test]
async fn fetch_web_deadline_interrupts_in_flight_io() {
    let (url, started) = spawn_stalled_server().await;
    let (client, config, _task) =
        spawn_fetch_server_with_deadline(true, 1024, Some(std::time::Duration::from_millis(250)))
            .await;
    let params = serde_json::json!({"url": url});
    let binding = binding("fetch.web", &params);
    reserve(&config, &binding);
    let (call, _control) = client.start_call_raw(binding, "fetch.web".to_string(), params);
    let call = tokio::spawn(call);
    tokio::time::timeout(std::time::Duration::from_secs(2), started)
        .await
        .expect("request must start before the deadline")
        .expect("fixture must observe request");
    let error = call
        .await
        .expect("call task")
        .expect_err("timed out fetch must fail");
    assert_eq!(error.code, CepErrorCode::ToolTimeout);
}

#[tokio::test]
async fn migrated_todo_write_has_no_native_fallback() {
    let (client, config, _task) = spawn_native_with_noop().await;
    let params = serde_json::json!({"todos": [{"content": "isolation", "status": "pending"}]});
    let err = call_raw(&client, &config, "session.todo_write", params)
        .await
        .expect_err("no native handler may serve the migrated method");
    assert_eq!(
        lancet_protocol::error::to_jsonrpc_error(&err).0,
        -32601,
        "a migrated method with no embedded Component must be unrouted: {err:?}"
    );
}

#[tokio::test]
async fn component_call_emits_events_with_consistent_invocation_id() {
    let (client, config, _task) = spawn_native_with_noop().await;

    let events = client.events();
    let params = serde_json::json!({ "message": "events" });
    let binding = binding("noop.echo", &params);
    let expected = binding.invocation_id.to_string();
    reserve(&config, &binding);
    let _value = client
        .call_raw(binding, "noop.echo", params)
        .await
        .expect("call succeeds");

    let mut started_id: Option<String> = None;
    let mut found_probed = false;
    let mut finished_id: Option<String> = None;
    let mut guard = events.lock().await;
    while let Ok(event) = guard.try_recv() {
        let iid = event
            .params
            .get("invocation_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        match event.method.as_str() {
            "cep.call_started" => started_id = iid,
            "cep.call_finished" => finished_id = iid,
            "tool.event" => {
                let phase = event.params.get("phase").and_then(|v| v.as_str());
                assert_eq!(phase, Some("probed"));
                assert_eq!(iid.as_deref(), Some(expected.as_str()));
                found_probed = true;
            }
            _ => {}
        }
    }
    assert!(found_probed, "expected a tool.event with phase=probed");
    assert_eq!(started_id.as_deref(), Some(expected.as_str()));
    assert_eq!(finished_id.as_deref(), Some(expected.as_str()));
}

#[tokio::test]
async fn cancelled_component_call_returns_call_cancelled_and_server_survives() {
    let (client, config, _task) = spawn_native_with_noop().await;

    let params = serde_json::json!({ "message": "cancel" });
    let binding = binding("noop.echo", &params);
    reserve(&config, &binding);
    let (call, control) = client.start_call_raw(binding, "noop.echo".to_string(), params);
    control.cancel();

    let err = call.await.expect_err("cancelled call must fail");
    assert_eq!(err.code, CepErrorCode::CallCancelled);

    let value = call_raw(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({ "message": "after-cancel" }),
    )
    .await
    .expect("server survives cancellation");
    assert_eq!(value, serde_json::json!({ "echoed": "after-cancel" }));
}

#[tokio::test]
async fn server_survives_after_component_method_not_found() {
    let (client, config, _task) = spawn_native_with_noop().await;

    let err = call_raw(&client, &config, "no.such.tool", serde_json::json!({}))
        .await
        .expect_err("unknown method must error");
    assert_eq!(
        lancet_protocol::error::to_jsonrpc_error(&err).0,
        lancet_protocol::error::jsonrpc::METHOD_NOT_FOUND
    );

    let value = call_raw(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({ "message": "survived" }),
    )
    .await
    .expect("call after error");
    assert_eq!(value, serde_json::json!({ "echoed": "survived" }));
}

#[tokio::test]
async fn component_params_schema_mismatch_rejected() {
    let (client, config, _task) = spawn_native_with_noop().await;

    let err = call_raw(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({ "wrong": "field" }),
    )
    .await
    .expect_err("schema mismatch must error");
    assert_eq!(err.code, CepErrorCode::SchemaMismatch);

    let value = call_raw(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({ "message": "ok" }),
    )
    .await
    .expect("call after schema error");
    assert_eq!(value, serde_json::json!({ "echoed": "ok" }));
}
