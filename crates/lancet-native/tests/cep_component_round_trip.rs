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
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::evidence::{EffectSource, ObservedTarget};
use lancet_protocol::{CapabilityAdmissionPolicy, CepError, CepErrorCode, Trust};
use lancet_runtime::ComponentRuntime;
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
    spawn_native_with_policies(policy, BTreeSet::new(), None, false).await
}

async fn spawn_native_with_policies(
    policy: CapabilityAdmissionPolicy,
    denied_effect_kinds: BTreeSet<ImpactKind>,
    project_root: Option<&std::path::Path>,
    allow_localhost: bool,
) -> (
    lancet_native::CepClient,
    ServerRunConfig,
    tokio::task::JoinHandle<()>,
) {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests().with_capability_admission(policy);
    let admission = config.cep_context.policy.capability_admission.clone();
    let mut turn_policy = match project_root {
        Some(root) => lancet_authority::policy::CepPolicyConfig::defaults_for_project_root(root),
        None => config.cep_context.policy.clone(),
    };
    turn_policy.capability_admission = admission;
    turn_policy.denied_effect_kinds = denied_effect_kinds;
    if allow_localhost {
        turn_policy.network.allow_localhost = true;
        turn_policy.network.allowed_hosts = vec!["127.0.0.1".into()];
    }
    config.cep_context = Arc::new(CepTurnContext::from_policy(turn_policy));
    if allow_localhost {
        let mut hook_policy =
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                project_root
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf(),
            );
        hook_policy.network.allow_localhost = true;
        hook_policy.network.allowed_hosts = vec!["127.0.0.1".into()];
        let hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            hook_policy,
        ));
        config.policy_hook = hook.clone();
        config.default_policy_hook = Some(hook);
    }
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

fn all_impact_kinds() -> BTreeSet<ImpactKind> {
    BTreeSet::from([
        ImpactKind::WorkspaceObservation,
        ImpactKind::WorkspaceMutation,
        ImpactKind::CommandExecution,
        ImpactKind::ExternalObservation,
        ImpactKind::SessionStateMutation,
        ImpactKind::AuditObservation,
    ])
}

fn native_probe_params(url: &str) -> serde_json::Value {
    serde_json::json!({ "url": url })
}

async fn spawn_native_probe_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind localhost fixture");
    let addr = listener.local_addr().expect("fixture address");
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await;
        }
    });
    format!("http://{addr}/")
}

async fn spawn_native_with_project_root(
    project_root: &std::path::Path,
) -> (
    lancet_native::CepClient,
    ServerRunConfig,
    tokio::task::JoinHandle<()>,
) {
    spawn_native_with_policies(admission(&[]), BTreeSet::new(), Some(project_root), true).await
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
async fn native_method_still_works_alongside_component() {
    let dir = tempfile::TempDir::new().expect("temp project");
    let (client, config, _task) = spawn_native_with_project_root(dir.path()).await;
    let url = spawn_native_probe_server().await;

    let value = call_raw(&client, &config, "fetch.web", native_probe_params(&url))
        .await
        .expect("fetch.web must remain a native registry method");
    assert_eq!(value["status"], 200);
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
async fn legacy_effect_deny_refuses_native_methods_and_never_component_calls() {
    let denied = all_impact_kinds();
    let dir = tempfile::TempDir::new().expect("temp project");
    let (client, config, _task) =
        spawn_native_with_policies(admission(&[]), denied, Some(dir.path()), false).await;

    let err = call_raw(
        &client,
        &config,
        "fetch.web",
        native_probe_params("https://native.invalid/"),
    )
    .await
    .expect_err("every declared impact kind is denied");
    assert_eq!(err.code, CepErrorCode::EffectViolation);

    let receipt = call_receipt(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({"message": "isolated"}),
    )
    .await
    .expect("the legacy deny list has no authority over a Cell-admitted Tool");
    assert_eq!(
        receipt.result.expect("tool result"),
        serde_json::json!({"echoed": "isolated"})
    );
    assert_eq!(
        receipt.evidence.admission.approved_capabilities,
        vec!["session.events".to_string(), "session.result".to_string()]
    );
}

#[tokio::test]
async fn cell_admission_denial_never_refuses_a_native_method() {
    let dir = tempfile::TempDir::new().expect("temp project");
    let (client, config, _task) = spawn_native_with_policies(
        CapabilityAdmissionPolicy::default(),
        BTreeSet::new(),
        Some(dir.path()),
        true,
    )
    .await;

    let err = call_raw(
        &client,
        &config,
        "noop.echo",
        serde_json::json!({"message": "refused"}),
    )
    .await
    .expect_err("no tool is enabled");
    assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);

    let url = spawn_native_probe_server().await;
    let result = call_raw(&client, &config, "fetch.web", native_probe_params(&url))
        .await
        .expect("Cell admission has no authority over a native method");
    assert_eq!(result["status"], 200);
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
