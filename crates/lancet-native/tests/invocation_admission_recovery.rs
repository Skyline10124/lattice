// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Recovery proofs for #139-B handle_request: reserved admission + duplicate reject.

use std::sync::Arc;

use lancet_native::{
    CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::{CapabilityAdmissionPolicy, CepErrorCode, Trust};
use lancet_runtime::ComponentRuntime;
use std::collections::BTreeSet;

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

async fn spawn_noop() -> (
    lancet_native::CepClient,
    ServerRunConfig,
    tokio::task::JoinHandle<()>,
) {
    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests().with_capability_admission(
        CapabilityAdmissionPolicy {
            enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
            denied_capabilities: BTreeSet::new(),
            max_boundary: Trust::In,
        },
    );
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

#[tokio::test]
async fn reserved_component_call_executes_once() {
    let (client, config, _task) = spawn_noop().await;
    let params = serde_json::json!({ "message": "ping" });
    let binding = binding("noop.echo", &params);
    reserve(&config, &binding);

    let value = client
        .call_raw(binding, "noop.echo", params)
        .await
        .expect("reserved component call must execute");
    assert_eq!(value, serde_json::json!({ "echoed": "ping" }));
}

#[tokio::test]
async fn duplicate_reserved_call_is_rejected_without_second_execution() {
    let (client, config, _task) = spawn_noop().await;
    let params = serde_json::json!({ "message": "once" });
    let binding = binding("noop.echo", &params);
    reserve(&config, &binding);

    let first = client
        .call_raw(binding.clone(), "noop.echo", params.clone())
        .await
        .expect("first reserved call executes");
    assert_eq!(first, serde_json::json!({ "echoed": "once" }));

    let err = client
        .call_raw(binding, "noop.echo", params)
        .await
        .expect_err("duplicate DispatchStarted admission must fail closed");
    assert_eq!(err.code, CepErrorCode::ResourceConflict);
}
