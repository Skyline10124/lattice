// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Dogfood the five-Component production catalog through typed bindings.
//!
//! The historical test name is retained because test-triage documents index it.
//! The ADR 0005 hard-cut contract is now: real Tool execution, content-free Cell
//! evidence, one typed SessionId/TurnId, and structural replay from invocations.git.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lancet_api::{AuditReplayRequest, replay_audit};
use lancet_audit::invocation_store::{
    InvocationStore, TerminalEvidence, TerminalMaterial, TerminalOutcome,
};
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig, WorkspacePolicy};
use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_native::{
    CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::{
    BindingIdentity, InvocationBinding, InvocationId, ModelMessageAddress, PlanKey, SessionId,
    ToolCallAddress, TurnId, arguments_digest, canonical_json,
};
use lattice_session::pending::{DraftPublication, ResultDraft, binding_digest};
use lattice_session::{GitSessionGraphStore, SessionGraphStore};
use lattice_store::{Blocking, FlockGuard, LockMode};
use tempfile::TempDir;

const TODO_WRITE: &str = "session.todo_write";
const OBSERVE_WORKSPACE: &str = "observe.workspace";
const FETCH_WEB: &str = "fetch.web";
const MUTATE_EDIT: &str = "mutate.edit";
const COMPONENT_TOOLS: [&str; 5] = [
    OBSERVE_WORKSPACE,
    "mutate.create",
    MUTATE_EDIT,
    FETCH_WEB,
    TODO_WRITE,
];
const FIXED_SESSION_ID: &str = "11111111111111111111111111111111";
const FIXED_TURN_ID: &str = "22222222-2222-4222-8222-222222222222";

struct ReservedDogfoodClient {
    inner: lancet_api::CepClient,
    invocations: Arc<InvocationStore>,
    sessions: Arc<GitSessionGraphStore>,
    execution_lock: Arc<FlockGuard>,
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
        let address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id: binding.identity.session_id,
                turn_id: binding.identity.turn_id,
                model_step_ordinal: binding.identity.model_step_ordinal,
            },
            part_ordinal: binding.identity.part_ordinal,
        };
        let material_binding_digest = binding_digest(
            &binding.invocation_id,
            &address,
            &binding.identity.method,
            &binding.identity.descriptor_identity,
            &binding.identity.arguments_digest,
        );
        self.invocations
            .reserve_batch(
                PlanKey {
                    turn_id,
                    model_step_ordinal: ordinal,
                },
                std::slice::from_ref(&binding),
            )
            .expect("reserve dogfood invocation");
        let invocation_id = binding.invocation_id;
        let receipt = self
            .inner
            .call_raw_with_receipt(binding, method, params)
            .await?;
        let canonical_content = canonical_json(
            receipt
                .result
                .as_ref()
                .expect("dogfood Component call succeeds"),
        );
        let draft = ResultDraft::new(
            invocation_id,
            material_binding_digest,
            turn_id,
            address.message,
            address.part_ordinal,
            canonical_content,
        )
        .expect("build production-shaped ResultDraft");
        let publication = self
            .sessions
            .publish_draft(&draft, &self.execution_lock)
            .expect("publish dogfood ResultDraft");
        assert!(
            matches!(publication, DraftPublication::Durable { .. }),
            "available terminal material requires durable draft publication: {publication:?}"
        );
        self.invocations
            .publish_terminal(
                invocation_id,
                TerminalOutcome::Succeeded,
                TerminalMaterial::Available {
                    digest: draft.content_digest,
                    byte_length: draft.byte_length,
                },
                TerminalEvidence {
                    admission: receipt.evidence.admission.clone(),
                    effects: receipt.evidence.effects.clone(),
                    denied: receipt.evidence.denied.clone(),
                },
            )
            .expect("publish dogfood terminal");
        Ok(receipt)
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
    let sessions_dir = TempDir::new().expect("temp Session store");
    let sessions = Arc::new(
        GitSessionGraphStore::create(sessions_dir.path().join("sessions.git"))
            .expect("create Session store"),
    );
    let project_key =
        lattice_session::ProjectStoreKey::from_canonical_path_bytes(canonical_text.as_bytes());
    sessions
        .create_session(
            FIXED_SESSION_ID,
            Some("dogfood-impact-matrix"),
            "dogfood",
            "main",
            &project_key,
        )
        .expect("create dogfood Session");
    let execution_lock = Arc::new(
        sessions
            .lock_execution(FIXED_SESSION_ID, LockMode::Exclusive, Blocking::Block)
            .expect("lock dogfood Session"),
    );
    let components =
        Arc::new(lancet_runtime::ComponentRuntime::from_embedded().expect("embedded composition"));
    let mut descriptors = BTreeMap::new();
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
        max_boundary: lancet_protocol::Trust::Ex,
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
        sessions,
        execution_lock,
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
    let fetch = client
        .call_receipt(FETCH_WEB, serde_json::json!({"url": fetch_url}))
        .await
        .expect("fetch receipt");
    let fetch_value = fetch.result.as_ref().expect("fetch result");
    assert_eq!(fetch_value["body"], "dog04-fetch-body");
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

    for evidence in [
        &read.evidence,
        &edit.evidence,
        &fetch.evidence,
        &todo.evidence,
    ] {
        let json = serde_json::to_string(evidence).expect("serialize evidence");
        assert!(!json.contains("hello dog04"));
        assert!(!json.contains("dog04 impact matrix verified"));
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
        vec![OBSERVE_WORKSPACE, MUTATE_EDIT, FETCH_WEB, TODO_WRITE]
    );
    let fetch_record = replay
        .trace
        .calls
        .iter()
        .find(|row| row.record.binding.method == FETCH_WEB)
        .expect("fetch replay row");
    let lancet_audit::invocation_store::InvocationState::Terminal { evidence, .. } =
        &fetch_record.record.state
    else {
        panic!("fetch row must be terminal");
    };
    assert!(
        evidence.effects.effects.iter().any(|effect| {
            effect
                .network
                .as_ref()
                .is_some_and(|network| network.http_status == Some(200))
        }),
        "replay must preserve network transport evidence"
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
