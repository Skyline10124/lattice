// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Impact matrix: one representative method per `ImpactKind` round-trips
//! from catalog-derived LLM tool shape through `reserved CepClient fixture` and back.

use lancet_catalog::CepMethod;
use lancet_catalog::catalog;
use lancet_catalog::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_native::{
    CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use lancet_protocol::error::{CepErrorData, ExecutionErrorData, jsonrpc};
use lancet_protocol::{
    BindingIdentity, CepError, CepErrorCode, InvocationBinding, InvocationId, PlanKey,
    Retriability, SessionId, TurnId, arguments_digest,
};

use lancet_audit::invocation_store::InvocationStore;
use lattice_api::{CepErrorPayload, ToolExecutionError};
use lattice_llm::{FunctionCall, ToolCall};
use serde_json::Value;
use tempfile::TempDir;

struct ImpactRow {
    impact: ImpactKind,
    method: &'static str,
    arguments: Value,
    assert_result: fn(&Value),
}

struct ReservedClient {
    client: lancet_api::CepClient,
    invocations: Arc<InvocationStore>,
    descriptors: BTreeMap<String, String>,
    enabled_names: HashSet<String>,
    next: AtomicU32,
}

impl ReservedClient {
    fn new(
        client: lancet_api::CepClient,
        invocations: Arc<InvocationStore>,
        definitions: &[lancet_cep::ToolDefinition],
        enabled_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            client,
            invocations,
            descriptors: definitions
                .iter()
                .map(|definition| {
                    (
                        definition.name.clone(),
                        definition.descriptor_identity.clone(),
                    )
                })
                .collect(),
            enabled_names: enabled_names.into_iter().map(Into::into).collect(),
            next: AtomicU32::new(0),
        }
    }

    fn binding(&self, call: &ToolCall) -> InvocationBinding {
        let ordinal = self.next.fetch_add(1, Ordering::Relaxed);
        let params: Value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
        let turn_id = TurnId::parse("22222222-2222-4222-8222-222222222222").unwrap();
        let binding = InvocationBinding {
            invocation_id: InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}",))
                .unwrap(),
            identity: BindingIdentity {
                session_id: SessionId::parse("11111111111111111111111111111111").unwrap(),
                turn_id,
                model_step_ordinal: ordinal,
                part_ordinal: 0,
                method: call.function.name.clone(),
                descriptor_identity: self
                    .descriptors
                    .get(&call.function.name)
                    .cloned()
                    .unwrap_or_else(|| "0".repeat(64)),
                arguments_digest: arguments_digest(&params),
            },
        };
        if self.descriptors.contains_key(&call.function.name)
            && serde_json::from_str::<Value>(&call.function.arguments).is_ok()
        {
            self.invocations
                .reserve_batch(
                    PlanKey {
                        turn_id,
                        model_step_ordinal: ordinal,
                    },
                    std::slice::from_ref(&binding),
                )
                .expect("reserve round-trip invocation");
        }
        binding
    }

    async fn execute(&self, call: &ToolCall) -> Result<String, ToolExecutionError> {
        let name = &call.function.name;
        if !self.descriptors.contains_key(name) || !self.enabled_names.contains(name) {
            return Err(map_test_error(lancet_protocol::error::method_not_found(
                name,
            )));
        }
        let params = serde_json::from_str(&call.function.arguments).map_err(|error| {
            map_test_error(lancet_protocol::error::parse_error(format!(
                "invalid tool arguments JSON: {error}"
            )))
        })?;
        let value = self
            .client
            .call_raw(self.binding(call), name.clone(), params)
            .await
            .map_err(map_test_error)?;
        serde_json::to_string(&value).map_err(|error| ToolExecutionError {
            message: format!("serialize tool result: {error}"),
            cep_error: None,
        })
    }

    async fn execute_receipt(
        &self,
        call: &ToolCall,
    ) -> Result<lancet_api::ExecutionReceipt, lancet_protocol::CepError> {
        let params: Value = serde_json::from_str(&call.function.arguments).unwrap_or(Value::Null);
        self.client
            .call_raw_with_receipt(self.binding(call), call.function.name.clone(), params)
            .await
    }
}

fn map_test_error(error: CepError) -> ToolExecutionError {
    let code = if let CepErrorData::Validation(data) = &error.data
        && data.reason == "invalid params"
    {
        jsonrpc::INVALID_PARAMS
    } else {
        lancet_native::wire_code_for_error(&error)
    };
    let retriability = match error.retriability {
        Retriability::Permanent => "permanent",
        Retriability::Transient => "transient",
        Retriability::RequiresReplan => "requires_replan",
        Retriability::Unknown => "unknown",
    };
    ToolExecutionError {
        message: error.message.clone(),
        cep_error: Some(CepErrorPayload {
            code,
            message: error.message,
            data: serde_json::to_value(&error.data).unwrap_or(Value::Null),
            retriability: retriability.into(),
        }),
    }
}

async fn reserved_client_with_registry(registry: MethodRegistry) -> ReservedClient {
    let (client, server) = CepTransport::pair(8);
    let config = ServerRunConfig::in_memory_for_tests();
    let invocations = Arc::clone(&config.invocations);
    let server_task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    let definitions = lancet_cep::project_tool_definitions(catalog());
    let enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    let adapter = ReservedClient::new(client, invocations, &definitions, enabled);
    tokio::task::yield_now().await;
    std::mem::forget(server_task);
    adapter
}

async fn fetch_only_client() -> ReservedClient {
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<Fetch, _, _>(|_params: FetchParams, _ctx| async move {
            Ok(FetchResult {
                status: 200,
                body: "external observation body".into(),
                content_type: Some("text/plain".into()),
                truncated: None,
            })
        })
        .expect("fetch handler");
    reserved_client_with_registry(registry).await
}

/// A method the production runtime can actually serve: a native catalog method,
/// or one of the embedded Component Tools (#29 — `session.todo_write` is the
/// first, and its descriptor authority is its source manifest, not the catalog).
fn assert_dispatchable_method(method: &str) {
    let native = catalog().iter().any(|descriptor| descriptor.name == method);
    let component = COMPONENT_TOOLS.contains(&method);
    assert!(
        native || component,
        "method is neither a native catalog method nor an embedded Component: {method}"
    );
}

/// The embedded Component Tools of the default distribution. `observe.workspace`
/// joined `session.todo_write` here in #30, which is why the native catalog is
/// down to three methods while the Agent-visible surface is still five.
const MUTATE_CREATE: &str = "mutate.create";
const MUTATE_EDIT: &str = "mutate.edit";
const COMPONENT_TOOLS: [&str; 4] = [OBSERVE_WORKSPACE, MUTATE_CREATE, MUTATE_EDIT, TODO_WRITE];

/// The migrated Tool's method name. Not a catalog constant any more: its name
/// lives in `tools/todo-write/manifest.toml`.
const TODO_WRITE: &str = "session.todo_write";

/// Likewise since #30: `observe.workspace` has no `lancet-catalog` type, and its
/// contract lives in `tools/observe-workspace/manifest.toml`.
const OBSERVE_WORKSPACE: &str = "observe.workspace";

fn llm_tool_call(method: &str, arguments: Value) -> ToolCall {
    assert_dispatchable_method(method);
    ToolCall {
        id: format!("call_{method}"),
        function: FunctionCall {
            name: method.to_string(),
            arguments: serde_json::to_string(&arguments).expect("serialize tool arguments"),
        },
    }
}

fn assert_cep_payload(err: &ToolExecutionError, expected_code: i32) {
    let payload = err
        .cep_error
        .as_ref()
        .expect("structured cep_error payload");
    assert_eq!(payload.code, expected_code);
    assert!(!payload.message.is_empty());
    assert!(!payload.data.is_null());
    assert!(!payload.retriability.is_empty());
}

#[tokio::test]
async fn cep_round_trip_impact_matrix() {
    let dir = TempDir::new().expect("temp workspace");
    let root = dir.path();
    std::fs::write(root.join("hello.txt"), "hello impact matrix\n").expect("seed file");

    let adapter = production_client(
        root.to_str().unwrap(),
        &[OBSERVE_WORKSPACE, "mutate.create", TODO_WRITE],
        false,
    )
    .await;

    // `ImpactRow::impact` labels the *semantic* class each row represents, not
    // the value its audit row records: two of these three are Component Tools
    // since #29/#30, and a Cell-governed row's `impact_kind` column reads
    // "unknown". `dogfood_impact_matrix` is where the recorded values are
    // asserted; here the label only names what the round-trip covers.
    let rows = [
        ImpactRow {
            impact: ImpactKind::WorkspaceObservation,
            method: OBSERVE_WORKSPACE,
            arguments: serde_json::json!({
                "mode": "read_file",
                "path": "hello.txt",
                "offset": 1,
                "limit": 10
            }),
            assert_result: |value| {
                assert_eq!(value["mode"].as_str(), Some("read_file"));
                let content = value["result"]["content"].as_str().unwrap_or_default();
                assert!(content.contains("hello impact matrix"));
            },
        },
        ImpactRow {
            impact: ImpactKind::WorkspaceMutation,
            method: "mutate.create",
            arguments: serde_json::json!({
                "path": "created.txt",
                "content": "written via reserved client"
            }),
            assert_result: |value| {
                assert_eq!(value["path"].as_str(), Some("created.txt"));
                assert!(value["bytes_written"].as_u64().unwrap_or(0) > 0);
            },
        },
        ImpactRow {
            impact: ImpactKind::SessionStateMutation,
            method: TODO_WRITE,
            arguments: serde_json::json!({
                "todos": [{
                    "content": "verify impact matrix",
                    "status": "in_progress"
                }]
            }),
            assert_result: |value| {
                assert_eq!(value["todos"].as_array().map(|t| t.len()), Some(1));
            },
        },
    ];

    for row in rows {
        let call = llm_tool_call(row.method, row.arguments);
        let result_json = adapter
            .execute(&call)
            .await
            .unwrap_or_else(|err| panic!("{} ({:?}) failed: {err:?}", row.method, row.impact));
        let parsed: Value = serde_json::from_str(&result_json).expect("reserved CEP result JSON");
        (row.assert_result)(&parsed);
    }

    let fetch_adapter = fetch_only_client().await;
    let fetch_call = llm_tool_call(
        Fetch::NAME,
        serde_json::json!({ "url": "https://example.invalid/cep-round-trip" }),
    );
    let fetch_result = fetch_adapter
        .execute(&fetch_call)
        .await
        .expect("fetch.web round-trip");
    let fetch_parsed: Value = serde_json::from_str(&fetch_result).expect("fetch result JSON");
    assert_eq!(
        fetch_parsed["body"].as_str(),
        Some("external observation body")
    );
    assert_eq!(fetch_parsed["status"].as_u64(), Some(200));
}

#[tokio::test]
async fn cep_round_trip_unknown_method_rejected_at_client() {
    let dir = TempDir::new().unwrap();
    let adapter =
        production_client(dir.path().to_str().unwrap(), &[OBSERVE_WORKSPACE], false).await;

    let err = adapter
        .execute(&ToolCall {
            id: "call_unknown".into(),
            function: FunctionCall {
                name: "crafted.unknown".into(),
                arguments: "{}".into(),
            },
        })
        .await
        .expect_err("unknown method must fail before server dispatch");

    assert_cep_payload(&err, jsonrpc::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn cep_round_trip_schema_mismatch_returns_invalid_params() {
    let dir = TempDir::new().unwrap();
    let adapter =
        production_client(dir.path().to_str().unwrap(), &[OBSERVE_WORKSPACE], false).await;

    let err = adapter
        .execute(&llm_tool_call(
            OBSERVE_WORKSPACE,
            serde_json::json!({ "mode": "read_file" }),
        ))
        .await
        .expect_err("missing required path must fail");

    // D.5: the reserved client no longer does client-side typed deserialization, so it
    // does not extract the missing field path. Since #30 the validator is the
    // Component host's `params_validator` running the manifest's JSON Schema
    // before Wasmtime is entered, rather than the native registry's typed
    // deserialization — both answer invalid_params (-32602), which is what the
    // caller contract promises.
    assert_cep_payload(&err, jsonrpc::INVALID_PARAMS);
}

#[tokio::test]
async fn cep_round_trip_cep_family_error_preserves_payload() {
    let expected = CepError::new(
        CepErrorCode::ExecutionFailed,
        "command failed",
        CepErrorData::Execution(ExecutionErrorData {
            command: "echo".into(),
            exit_code: Some(1),
            stderr: Some("boom".into()),
        }),
        Retriability::Permanent,
    );
    let expected_message = expected.message.clone();
    let expected_data = expected.data.clone();
    let wire_code = expected.wire_code();
    let server_err = Arc::new(expected);

    // A native method with a stub handler that fails: this test is about the
    // adapter preserving a structured `CepError` payload, so it needs a handler
    // it can register. A Component Tool has no registrable handler seam.
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<Fetch, _, _>({
            let server_err = Arc::clone(&server_err);
            move |_params: lancet_catalog::methods::external::FetchParams, _ctx| {
                let err = Arc::clone(&server_err);
                async move { Err((*err).clone()) }
            }
        })
        .expect("fetch handler");
    let adapter = reserved_client_with_registry(registry).await;

    let err = adapter
        .execute(&llm_tool_call(
            Fetch::NAME,
            serde_json::json!({ "url": "https://example.com/" }),
        ))
        .await
        .expect_err("CEP family error must propagate");

    assert_cep_payload(&err, wire_code);
    let payload = err.cep_error.as_ref().unwrap();
    assert_eq!(payload.message, expected_message);
    assert_eq!(payload.retriability, "permanent");
    assert_eq!(payload.data, serde_json::to_value(&expected_data).unwrap());
}

async fn spawn_echo_server(status_line: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind localhost");
    let addr = listener.local_addr().expect("local addr");
    let body_owned = body.to_string();
    let status_owned = status_line.to_string();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 {status_owned}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_owned}",
                body_owned.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    format!("http://127.0.0.1:{}/", addr.port())
}

async fn production_client(root: &str, enabled: &[&str], allow_localhost: bool) -> ReservedClient {
    use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig, WorkspacePolicy};
    use lancet_authority::{CepPolicyConfig, CepTurnContext};

    let canonical_root = std::fs::canonicalize(root).expect("canonicalize production root");
    let root = canonical_root.to_str().expect("utf-8 production root");

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("production registry");

    let mut policy = CepPolicyConfig::defaults_for_project_root(root);
    policy.network.allow_localhost = allow_localhost;
    if allow_localhost {
        policy.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    }
    let mut policy_config = PolicyConfig::defaults_for_project_root(canonical_root.clone());
    policy_config.network.allow_localhost = allow_localhost;
    if allow_localhost {
        policy_config.network.allowed_hosts = vec!["127.0.0.1".into(), "localhost".into()];
    }
    // Allow all writes under the project root (empty deny list).
    policy_config.workspace = WorkspacePolicy {
        denied_write_patterns: vec![],
        ..Default::default()
    };

    let (client, server) = CepTransport::pair(16);
    let mut config = ServerRunConfig::in_memory_for_tests();
    // #29: the production dispatch surface is native handlers *plus* the
    // embedded Component catalog. Composing from `from_embedded()` here is what
    // makes `session.todo_write` reachable at all — a hand-built transport
    // without it would report `method_not_found`, which is exactly the
    // native-fallback absence the migration requires.
    let components =
        Arc::new(lancet_runtime::ComponentRuntime::from_embedded().expect("embedded composition"));
    policy.capability_admission = lancet_protocol::evidence::CapabilityAdmissionPolicy {
        enabled_tools: enabled.iter().map(|name| (*name).to_string()).collect(),
        ..Default::default()
    };
    config.components = Some(Arc::clone(&components));
    config.cep_context = Arc::new(CepTurnContext::from_policy(policy));
    let default_hook = Arc::new(DefaultPolicyHook::new(policy_config));
    config.policy_hook = default_hook.clone();
    config.default_policy_hook = Some(default_hook);

    let invocations = Arc::clone(&config.invocations);
    let server_task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    let enabled_names: Vec<String> = enabled.iter().map(|s| (*s).to_string()).collect();
    // Mirror `LancetRuntime::build`: the LLM-facing set is the native catalog
    // projection extended with the Component descriptors and pinned identities.
    let mut definitions = lancet_cep::project_tool_definitions(catalog());
    definitions.extend(
        components
            .descriptor_identities()
            .map(|(descriptor, identity)| {
                lancet_cep::ToolDefinition::new(
                    descriptor.name.clone(),
                    descriptor.description.clone(),
                    descriptor.params_schema.clone(),
                    identity.to_string(),
                )
            }),
    );
    let adapter = ReservedClient::new(client, invocations, &definitions, enabled_names);
    tokio::task::yield_now().await;
    std::mem::forget(server_task);
    adapter
}

#[tokio::test]
async fn web_fetch_and_todo_write_return_content_free_receipts() {
    let dir = TempDir::new().expect("temp workspace");
    let fetch_url = spawn_echo_server("200 OK", "cep-body").await;
    let adapter = production_client(
        dir.path().to_str().unwrap(),
        &[Fetch::NAME, TODO_WRITE],
        true,
    )
    .await;

    let fetch_call = llm_tool_call(Fetch::NAME, serde_json::json!({ "url": fetch_url }));
    let fetch = adapter
        .execute_receipt(&fetch_call)
        .await
        .expect("fetch receipt");
    let fetch_value = fetch.result.expect("fetch result");
    assert_eq!(fetch_value["body"].as_str(), Some("cep-body"));
    assert_eq!(fetch.evidence.admission.tool, Fetch::NAME);

    let todo_call = llm_tool_call(
        TODO_WRITE,
        serde_json::json!({
            "todos": [{ "content": "secret todo text", "status": "in_progress" }]
        }),
    );
    let todo = adapter
        .execute_receipt(&todo_call)
        .await
        .expect("todo receipt");
    todo.result.expect("todo result");
    assert_eq!(todo.evidence.admission.tool, TODO_WRITE);
    assert_eq!(todo.evidence.effects.effects.len(), 1);
    assert_eq!(
        todo.evidence.effects.effects[0].source,
        lancet_protocol::evidence::EffectSource::Result
    );

    for evidence in [&fetch.evidence, &todo.evidence] {
        let json = serde_json::to_string(evidence).expect("serialize evidence");
        assert!(!json.contains("cep-body"));
        assert!(!json.contains("secret todo text"));
    }
}
