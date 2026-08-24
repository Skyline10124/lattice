// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Dogfood the mixed native/Component production catalog through typed bindings.
//!
//! The historical test name is retained because test-triage documents index it.
//! The ADR 0005 hard-cut contract is now: real Tool execution, content-free Cell
//! evidence, one typed SessionId/TurnId, and structural replay from invocations.git.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lancet_api::{AuditReplayRequest, replay_audit};
use lancet_audit::invocation_store::InvocationStore;
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig, WorkspacePolicy};
use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_catalog::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_catalog::{CepMethod, catalog};
use lancet_native::{
    CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::{
    BindingIdentity, InvocationBinding, InvocationId, PlanKey, SessionId, TurnId, arguments_digest,
};
use tempfile::TempDir;

const TODO_WRITE: &str = "session.todo_write";
const OBSERVE_WORKSPACE: &str = "observe.workspace";
const MUTATE_EDIT: &str = "mutate.edit";
const COMPONENT_TOOLS: [&str; 4] = [OBSERVE_WORKSPACE, "mutate.create", MUTATE_EDIT, TODO_WRITE];
const FIXED_SESSION_ID: &str = "11111111111111111111111111111111";
const FIXED_TURN_ID: &str = "22222222-2222-4222-8222-222222222222";

struct ReservedDogfoodClient {
    inner: lancet_api::CepClient,
    invocations: Arc<InvocationStore>,
    descriptors: BTreeMap<String, String>,
    next: AtomicU32,
}

impl ReservedDogfoodClient {
    async fn call_receipt(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<lancet_api::ExecutionReceipt, lancet_protocol::CepError> {
        let ordinal = self.next.fetch_add(1, Ordering::Relaxed);
        let turn_id = TurnId::parse(FIXED_TURN_ID).expect("fixed TurnId");
        let binding = InvocationBinding {
            invocation_id: InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
                .expect("fixed InvocationId"),
            identity: BindingIdentity {
                session_id: SessionId::parse(FIXED_SESSION_ID).expect("fixed SessionId"),
                turn_id,
                model_step_ordinal: ordinal,
                part_ordinal: 0,
                method: method.to_string(),
                descriptor_identity: self
                    .descriptors
                    .get(method)
                    .cloned()
                    .expect("projected descriptor identity"),
                arguments_digest: arguments_digest(&params),
            },
        };
        self.invocations
            .reserve_batch(
                PlanKey {
                    turn_id,
                    model_step_ordinal: ordinal,
                },
                std::slice::from_ref(&binding),
            )
            .expect("reserve dogfood invocation");
        self.inner
            .call_raw_with_receipt(binding, method, params)
            .await
    }

    async fn call_raw(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, lancet_protocol::CepError> {
        self.call_receipt(method, params).await?.result
    }
}

async fn spawn_echo_server(body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind localhost");
    let addr = listener.local_addr().expect("local addr");
    let body = body.to_string();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    format!("http://127.0.0.1:{}/", addr.port())
}

#[tokio::test]
async fn dogfood_impact_matrix_five_production_classes() {
    let _state = lancet_audit::ScopedXdgStateHome::new();
    let dir = TempDir::new().expect("temp workspace");
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    let seed = "hello dog04\n";
    std::fs::write(root.join("src/dog04.txt"), seed).expect("seed file");
    let canonical = std::fs::canonicalize(root).expect("canonical workspace");
    let canonical_text = canonical.to_str().expect("utf-8 workspace");

    let invocations = Arc::new(
        InvocationStore::create_project(&canonical).expect("create project invocation ledger"),
    );
    let components =
        Arc::new(lancet_runtime::ComponentRuntime::from_embedded().expect("embedded composition"));
    let mut descriptors: BTreeMap<String, String> = lancet_cep::project_tool_definitions(catalog())
        .into_iter()
        .map(|definition| (definition.name, definition.descriptor_identity))
        .collect();
    descriptors.extend(
        components
            .descriptor_identities()
            .map(|(descriptor, identity)| (descriptor.name.clone(), identity.to_string())),
    );

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("production registry");
    let mut policy = CepPolicyConfig::defaults_for_project_root(canonical_text);
    policy.network.allow_localhost = true;
    policy.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    policy.capability_admission = lancet_protocol::evidence::CapabilityAdmissionPolicy {
        enabled_tools: COMPONENT_TOOLS
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        ..Default::default()
    };
    let mut policy_config = PolicyConfig::defaults_for_project_root(canonical.clone());
    policy_config.network.allow_localhost = true;
    policy_config.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    policy_config.workspace = WorkspacePolicy {
        denied_write_patterns: Vec::new(),
        ..Default::default()
    };
    let hook = Arc::new(DefaultPolicyHook::new(policy_config));

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.invocations = Arc::clone(&invocations);
    config.components = Some(components);
    config.cep_context = Arc::new(CepTurnContext::from_policy(policy));
    config.policy_hook = hook.clone();
    config.default_policy_hook = Some(hook);

    let (raw_client, server) = CepTransport::pair(32);
    let client = ReservedDogfoodClient {
        inner: raw_client,
        invocations,
        descriptors,
        next: AtomicU32::new(0),
    };
    let server_task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    tokio::task::yield_now().await;

    let read = client
        .call_receipt(
            OBSERVE_WORKSPACE,
            serde_json::json!({ "mode": "read_file", "path": "src/dog04.txt" }),
        )
        .await
        .expect("observe receipt");
    let read_value = read.result.expect("observe result");
    assert!(
        read_value["result"]["content"]
            .as_str()
            .is_some_and(|content| content.contains("hello dog04"))
    );

    let file_hash = format!("blake3:{}", blake3::hash(seed.as_bytes()).to_hex());
    let edit = client
        .call_receipt(
            MUTATE_EDIT,
            serde_json::json!({
                "path": "src/dog04.txt",
                "expected_hash": file_hash,
                "edits": [{"op": "replace_text", "old": "hello dog04", "new": "hello dog04 patched"}]
            }),
        )
        .await
        .expect("edit receipt");
    edit.result.expect("edit result");

    let fetch_url = spawn_echo_server("dog04-fetch-body").await;
    let fetch: FetchResult = serde_json::from_value(
        client
            .call_raw(
                Fetch::NAME,
                serde_json::to_value(FetchParams {
                    url: fetch_url,
                    headers: Default::default(),
                })
                .expect("fetch params"),
            )
            .await
            .expect("fetch result"),
    )
    .expect("typed fetch result");
    assert_eq!(fetch.body, "dog04-fetch-body");

    let todo = client
        .call_receipt(
            TODO_WRITE,
            serde_json::json!({
                "todos": [{"content": "dog04 impact matrix verified", "status": "completed"}]
            }),
        )
        .await
        .expect("todo receipt");
    todo.result.expect("todo result");

    for evidence in [&read.evidence, &edit.evidence, &todo.evidence] {
        let json = serde_json::to_string(evidence).expect("serialize evidence");
        assert!(!json.contains("hello dog04"));
        assert!(!json.contains("dog04 impact matrix verified"));
        assert!(!json.contains("audit_id"));
        assert!(!json.contains("evidence_id"));
    }

    let replay = replay_audit(AuditReplayRequest::new(
        &canonical,
        FIXED_SESSION_ID,
        FIXED_TURN_ID,
    ))
    .await
    .expect("replay authoritative ledger");
    assert_eq!(
        replay.trace.session_id,
        SessionId::parse(FIXED_SESSION_ID).unwrap()
    );
    assert_eq!(replay.trace.turn_id, TurnId::parse(FIXED_TURN_ID).unwrap());
    let methods: Vec<&str> = replay
        .trace
        .calls
        .iter()
        .map(|row| row.record.binding.method.as_str())
        .collect();
    assert_eq!(
        methods,
        vec![OBSERVE_WORKSPACE, MUTATE_EDIT, Fetch::NAME, TODO_WRITE]
    );
    let ordinals: Vec<u32> = replay
        .trace
        .calls
        .iter()
        .map(|row| row.record.binding.model_step_ordinal)
        .collect();
    assert_eq!(ordinals, vec![0, 1, 2, 3]);

    server_task.abort();
}
