// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use crate::turn::cep::ToolExecutionError;
use lancet_catalog::CepMethod;
use lancet_catalog::catalog;
use lancet_catalog::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_catalog::test_method::{TestEcho, TestEchoParams, TestEchoResult};
use lancet_native::{CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::error::{
    CepErrorData, EffectErrorData, ExecutionErrorData, GuardErrorData, PermissionErrorData,
    StateErrorData, ValidationErrorData, jsonrpc,
};
use lancet_protocol::{
    BindingIdentity, CepError, CepErrorCode, InvocationBinding, InvocationId, PlanKey,
    Retriability, SessionId, TurnId, arguments_digest,
};
#[derive(Clone)]
struct FunctionCall {
    name: String,
    arguments: String,
}

#[derive(Clone)]
struct ToolCall {
    function: FunctionCall,
}
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::cep::{CepErrorPayload, retriability_label, test_wire_code};
use std::collections::HashSet;

struct TestCepClient {
    client: lancet_native::CepClient,
    available_names: HashSet<String>,
    enabled_names: HashSet<String>,
}

impl TestCepClient {
    fn new(
        client: lancet_native::CepClient,
        definitions: &[lancet_cep::ToolDefinition],
        enabled_names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            client,
            available_names: definitions
                .iter()
                .map(|definition| definition.name.clone())
                .collect(),
            enabled_names: enabled_names.into_iter().map(Into::into).collect(),
        }
    }

    async fn execute(
        &self,
        binding: InvocationBinding,
        call: &ToolCall,
    ) -> Result<String, ToolExecutionError> {
        let name = &call.function.name;
        if !self.available_names.contains(name) || !self.enabled_names.contains(name) {
            return Err(test_tool_error(lancet_protocol::error::method_not_found(
                name,
            )));
        }
        let params = serde_json::from_str(&call.function.arguments).map_err(|error| {
            test_tool_error(lancet_protocol::error::parse_error(format!(
                "invalid tool arguments JSON: {error}"
            )))
        })?;
        let value = self
            .client
            .call_raw(binding, name.clone(), params)
            .await
            .map_err(test_tool_error)?;
        serde_json::to_string(&value).map_err(|error| ToolExecutionError {
            message: format!("serialize tool result: {error}"),
            cep_error: None,
        })
    }
}

fn test_tool_error(error: CepError) -> ToolExecutionError {
    ToolExecutionError {
        message: error.message.clone(),
        cep_error: Some(CepErrorPayload {
            code: test_wire_code(&error),
            message: error.message,
            data: serde_json::to_value(&error.data).unwrap_or(serde_json::Value::Null),
            retriability: retriability_label(&error.retriability),
        }),
    }
}
fn todo_registry<F, Fut>(handler: F) -> MethodRegistry
where
    F: Fn(TestEchoParams, lancet_native::HandlerContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<TestEchoResult, CepError>> + Send + 'static,
{
    let mut registry = MethodRegistry::new();
    registry.register_method::<TestEcho, _, _>(handler).unwrap();
    registry
}

struct ReservedTestClient {
    adapter: TestCepClient,
    invocations: Arc<lancet_audit::invocation_store::InvocationStore>,
    definitions: Vec<lancet_cep::ToolDefinition>,
}

impl ReservedTestClient {
    async fn execute_reserved(&self, call: &ToolCall) -> Result<String, ToolExecutionError> {
        let ordinal = NEXT_TEST_INVOCATION.fetch_add(1, Ordering::Relaxed) as u32;
        let params =
            serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);
        let descriptor_identity = self
            .definitions
            .iter()
            .find(|definition| definition.name == call.function.name)
            .map(|definition| definition.descriptor_identity.clone())
            .unwrap_or_else(|| "0".repeat(64));
        let turn_id = TurnId::parse("22222222-2222-4222-8222-222222222222").unwrap();
        let binding = InvocationBinding {
            invocation_id: InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
                .unwrap(),
            identity: BindingIdentity {
                session_id: SessionId::parse("11111111111111111111111111111111").unwrap(),
                turn_id,
                model_step_ordinal: ordinal,
                part_ordinal: 0,
                method: call.function.name.clone(),
                descriptor_identity,
                arguments_digest: arguments_digest(&params),
            },
        };
        if self
            .definitions
            .iter()
            .any(|definition| definition.name == call.function.name)
            && params != serde_json::Value::Null
        {
            self.invocations
                .reserve_batch(
                    PlanKey {
                        turn_id,
                        model_step_ordinal: ordinal,
                    },
                    std::slice::from_ref(&binding),
                )
                .expect("reserve test invocation");
        }
        self.adapter.execute(binding, call).await
    }
}

static NEXT_TEST_INVOCATION: AtomicU64 = AtomicU64::new(1);

fn unreserved_binding(call: &ToolCall) -> InvocationBinding {
    let ordinal = NEXT_TEST_INVOCATION.fetch_add(1, Ordering::Relaxed) as u32;
    let turn_id = TurnId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    let params = serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::Value::Null);
    InvocationBinding {
        invocation_id: InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
            .unwrap(),
        identity: BindingIdentity {
            session_id: SessionId::parse("11111111111111111111111111111111").unwrap(),
            turn_id,
            model_step_ordinal: ordinal,
            part_ordinal: 0,
            method: call.function.name.clone(),
            descriptor_identity: "0".repeat(64),
            arguments_digest: arguments_digest(&params),
        },
    }
}

async fn reserved_client_over_registry(registry: MethodRegistry) -> ReservedTestClient {
    let (client, server) = CepTransport::pair(8);
    let config = ServerRunConfig::in_memory_for_tests();
    let invocations = Arc::clone(&config.invocations);
    let server_task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });
    let mut definitions = lancet_cep::project_tool_definitions(catalog());
    definitions.push(lancet_cep::ToolDefinition::new(
        TestEcho::NAME,
        TestEcho::DESCRIPTION,
        serde_json::json!({"type": "object"}),
        "0".repeat(64),
    ));
    let enabled: Vec<String> = definitions.iter().map(|d| d.name.clone()).collect();
    let adapter = TestCepClient::new(client, &definitions, enabled);
    tokio::task::yield_now().await;
    std::mem::forget(server_task);
    ReservedTestClient {
        adapter,
        invocations,
        definitions,
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
async fn reserved_cep_client_retired_methods_return_method_not_found() {
    let adapter = reserved_client_over_registry(todo_registry(|params, _ctx| async move {
        Ok(TestEchoResult {
            items: params.items,
        })
    }))
    .await;

    for method in ["observe.audit", "observe.audit_replay", "mutate.batch"] {
        let err = adapter
            .execute_reserved(&ToolCall {
                function: FunctionCall {
                    name: method.to_string(),
                    arguments: "{}".to_string(),
                },
            })
            .await
            .expect_err(method);
        assert_cep_payload(&err, jsonrpc::METHOD_NOT_FOUND);
    }
}

#[tokio::test]
async fn reserved_cep_client_parse_error_maps_to_32700() {
    let adapter = reserved_client_over_registry(todo_registry(|params, _ctx| async move {
        Ok(TestEchoResult {
            items: params.items,
        })
    }))
    .await;

    let err = adapter
        .execute_reserved(&ToolCall {
            function: FunctionCall {
                name: TestEcho::NAME.to_string(),
                arguments: "{not-json".to_string(),
            },
        })
        .await
        .expect_err("malformed JSON must fail");

    assert_cep_payload(&err, jsonrpc::PARSE_ERROR);
}

/// A single-name enabled set lets that name through the reserved client and the
/// arguments reach the handler.
///
/// Driven through `fetch.web`: this fixture needs a *registrable* handler seam.
/// Component Tools have no `register_method` seam; their reserved-client coverage lives
/// in `component_client::reserved_cep_client_executes_component_tool`.
#[tokio::test]
async fn reserved_cep_client_allows_enabled_native_method() {
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<Fetch, _, _>(|_params: FetchParams, _ctx| async move {
            Ok(FetchResult {
                status: 200,
                body: "ok-from-handler".into(),
                content_type: None,
                truncated: None,
            })
        })
        .unwrap();
    let adapter = reserved_client_over_registry(registry).await;

    let result = adapter
        .execute_reserved(&ToolCall {
            function: FunctionCall {
                name: "fetch.web".to_string(),
                arguments: r#"{"url":"https://example.com/"}"#.to_string(),
            },
        })
        .await
        .expect("enabled tool should execute");
    assert!(
        result.contains("ok-from-handler") || result.contains("status"),
        "handler result must surface through the reserved client: {result}"
    );
}

#[tokio::test]
async fn reserved_cep_client_unknown_method_maps_to_32601() {
    let adapter = reserved_client_over_registry(MethodRegistry::new()).await;

    let err = adapter
        .execute_reserved(&ToolCall {
            function: FunctionCall {
                name: "crafted.unknown".to_string(),
                arguments: "{}".to_string(),
            },
        })
        .await
        .expect_err("unknown method must fail before server dispatch");

    assert_cep_payload(&err, jsonrpc::METHOD_NOT_FOUND);
}

/// Params the handler's type cannot accept fail as invalid_params (-32602).
///
/// Driven through `fetch.web`: this asserts the *native registry's* server-side
/// validation, which needs a registered handler. `url` is the omitted required
/// field.
#[tokio::test]
async fn reserved_cep_client_invalid_params_maps_to_32602() {
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<Fetch, _, _>(|_params: FetchParams, _ctx| async move {
            Ok(FetchResult {
                status: 200,
                body: "ok".into(),
                content_type: None,
                truncated: None,
            })
        })
        .unwrap();
    let adapter = reserved_client_over_registry(registry).await;

    let err = adapter
        .execute_reserved(&ToolCall {
            function: FunctionCall {
                name: "fetch.web".to_string(),
                arguments: r#"{"headers":{}}"#.to_string(),
            },
        })
        .await
        .expect_err("schema mismatch must fail");

    // D.5: the adapter no longer does client-side typed deserialization,
    // so it does not extract the missing field path. The server-side native
    // registry validates params and returns invalid_params (-32602) with
    // field="params" (the registry's default, not the extracted field name).
    assert_cep_payload(&err, jsonrpc::INVALID_PARAMS);
}

macro_rules! cep_family_test {
    ($name:ident, $code:expr_2021, $data:expr_2021, $retriability:expr_2021) => {
        #[tokio::test]
        async fn $name() {
            let expected = CepError::new($code, "family error", $data, $retriability);
            let expected_message = expected.message.clone();
            let expected_data = expected.data.clone();
            let wire_code = expected.wire_code();
            let server_err = Arc::new(expected);
            let adapter = reserved_client_over_registry(todo_registry({
                let server_err = Arc::clone(&server_err);
                move |_params, _ctx| {
                    let err = Arc::clone(&server_err);
                    async move { Err((*err).clone()) }
                }
            }))
            .await;

            let err = adapter
                .execute_reserved(&ToolCall {
                    function: FunctionCall {
                        name: TestEcho::NAME.to_string(),
                        arguments: r#"{"items":[]}"#.to_string(),
                    },
                })
                .await
                .expect_err("CEP family error must propagate");

            assert_cep_payload(&err, wire_code);
            let payload = err.cep_error.as_ref().unwrap();
            assert_eq!(payload.message, expected_message);
            assert_eq!(payload.retriability, "permanent");
            assert_eq!(payload.data, serde_json::to_value(&expected_data).unwrap());
        }
    };
}

cep_family_test!(
    reserved_cep_client_validation_family_preserves_payload,
    CepErrorCode::SchemaMismatch,
    CepErrorData::Validation(ValidationErrorData {
        field: "path".into(),
        reason: "bad".into(),
    }),
    Retriability::Permanent
);

cep_family_test!(
    reserved_cep_client_permission_family_preserves_payload,
    CepErrorCode::CapabilityNotGranted,
    CepErrorData::Permission(PermissionErrorData {
        required_capability: "read_path".into(),
        granted_capabilities: vec![],
    }),
    Retriability::Permanent
);

cep_family_test!(
    reserved_cep_client_execution_family_preserves_payload,
    CepErrorCode::ExecutionFailed,
    CepErrorData::Execution(ExecutionErrorData {
        command: "ls".into(),
        exit_code: Some(1),
        stderr: Some("failed".into()),
    }),
    Retriability::Permanent
);

cep_family_test!(
    reserved_cep_client_state_family_preserves_payload,
    CepErrorCode::StaleContext,
    CepErrorData::State(StateErrorData {
        resource: "session".into(),
        reason: "stale".into(),
    }),
    Retriability::Permanent
);

cep_family_test!(
    reserved_cep_client_guard_family_preserves_payload,
    CepErrorCode::GuardDenied,
    CepErrorData::Guard(GuardErrorData {
        method: TestEcho::NAME.into(),
        reason: "denied".into(),
    }),
    Retriability::Permanent
);

// The effect payload names `mutate.create` and the kind that method really
// declares. It used to name `observe.workspace`/`workspace_observation`, but
// since #30 that method is a Component Tool governed by the Cell vocabulary and
// declares no `ImpactKind` at all — a fixture claiming otherwise would document
// a combination the production surface can no longer produce.
cep_family_test!(
    reserved_cep_client_effect_family_preserves_payload,
    CepErrorCode::EffectViolation,
    CepErrorData::Effect(Box::new(EffectErrorData {
        method: "mutate.create".into(),
        declared_effect: vec![ImpactKind::WorkspaceMutation],
        authorized_effect: vec![ImpactKind::WorkspaceMutation],
        observed_effects: vec![
            ImpactKind::WorkspaceMutation,
            ImpactKind::ExternalObservation,
        ],
        violations: vec!["observed external_observation not in declared effect ceiling".into(),],
        cell_ref: None,
        capability: None,
    })),
    Retriability::Permanent
);

#[tokio::test]
async fn turn_runtime_constructs_one_client_per_turn() {
    use tempfile::TempDir;

    use lattice_agent::LlmClient;
    use lattice_agent::testing::FakeStreamingLlmClient;

    use super::llm::RealLlmClient;
    use super::test_support::build_test_production_deps;

    // The second build below persists audit (test_persist_audit=false path),
    // which resolves the external audit DB via dirs::state_dir() — an
    // XDG_STATE_HOME read. Hold the shared env lock so a concurrent
    // ScopedXdgStateHome test cannot point XDG_STATE_HOME at its tempdir and
    // drop it under our open SQLite store (FINDINGS ENV-OOM-02 "disk I/O
    // error" flake); the isolated state dir also keeps the audit DB out of
    // the real Linux XDG state directory.
    let _xdg = lancet_audit::ScopedXdgStateHome::new();
    let dir = TempDir::new().unwrap();

    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![lattice_llm::LlmEvent::Done {
                finish_reason: "stop".to_string(),
                usage: None,
            }])
            .build(),
    );

    let deps = build_test_production_deps(dir.path().to_str().unwrap(), &[], llm, true, None).await;

    assert_eq!(deps.build_metrics.client_instances, 1);
    assert_eq!(deps.build_metrics.transport_pairs, 1);

    let deps2 = build_test_production_deps(
        dir.path().to_str().unwrap(),
        &["session.todo_write".to_string()],
        Arc::new(RealLlmClient),
        false,
        None,
    )
    .await;
    assert_eq!(deps2.build_metrics.client_instances, 1);
    assert_eq!(deps2.build_metrics.transport_pairs, 1);
}

#[cfg(test)]
mod cep_native_registry {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn cep_native_registry_production_runtime_uses_native_audit_stage() {
        use lattice_agent::LlmClient;
        use lattice_agent::testing::FakeStreamingLlmClient;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        use crate::turn::test_support::build_test_production_deps;

        let _xdg = lancet_audit::ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();

        let llm: Arc<dyn LlmClient> = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(vec![lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                }])
                .build(),
        );

        // Localhost echo — avoids depending on public HTTPS for an audit-stage pin.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf).await;
                let body = b"native-audit";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });
        let url = format!("http://127.0.0.1:{}/", addr.port());

        // A native method: this test asserts the *native* audit stage, so a
        // Component Tool would be labeled `cell_component` and prove the
        // opposite. It used to drive `observe.workspace` / `mutate.create`;
        // both migrated to Components (#30/#31). `fetch.web` remains native.
        let lancet = lattice_profile::LancetSection {
            network: Some(lattice_profile::LancetNetworkSection {
                allow_localhost: Some(true),
                allowed_hosts: vec!["127.0.0.1".into(), "localhost".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let deps = build_test_production_deps(
            dir.path().to_str().unwrap(),
            &["fetch.web".to_string()],
            llm,
            true,
            Some(&lancet),
        )
        .await;

        let params = FetchParams {
            url,
            headers: Default::default(),
        };
        let value = serde_json::to_value(&params).unwrap();
        let binding = lancet_protocol::InvocationBinding {
            invocation_id: lancet_protocol::InvocationId::parse(
                "00000000-0000-4000-8000-000000000001",
            )
            .unwrap(),
            identity: lancet_protocol::BindingIdentity {
                session_id: lancet_protocol::SessionId::parse("11111111111111111111111111111111")
                    .unwrap(),
                turn_id: lancet_protocol::TurnId::parse("22222222-2222-4222-8222-222222222222")
                    .unwrap(),
                model_step_ordinal: 0,
                part_ordinal: 0,
                method: Fetch::NAME.to_string(),
                descriptor_identity: deps
                    .tool_catalog
                    .iter()
                    .find(|tool| tool.name == Fetch::NAME)
                    .expect("fetch definition")
                    .descriptor_identity
                    .clone(),
                arguments_digest: lancet_protocol::arguments_digest(&value),
            },
        };
        let layout = lattice_session::StateLayout::resolve().unwrap();
        let key = crate::turn::test_support::test_project_key(dir.path());
        lancet_audit::invocation_store::InvocationStore::open(&layout, &key)
            .unwrap()
            .reserve_batch(
                lancet_protocol::PlanKey {
                    turn_id: binding.identity.turn_id,
                    model_step_ordinal: 0,
                },
                std::slice::from_ref(&binding),
            )
            .unwrap();
        deps.cep_client
            .call::<Fetch>(binding, params)
            .await
            .expect("native fetch.web");
    }
}

/// D.6: `reserved CepClient fixture` can execute a Component Tool using a
/// descriptor-projected definition. This proves the LLM tool-call path
/// is reachable for Component Tools (D.5's `call_raw` dispatch + D.4's
/// descriptor projection).
mod component_client {
    use super::*;
    use lancet_native::{
        CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
        register_production_method_registry,
    };
    use lancet_runtime::ComponentRuntime;
    use std::sync::Arc;

    /// Spawn a native server with the no-op Component fixture, then build
    /// a `reserved CepClient fixture` from the projected Component descriptor.
    async fn reserved_client_with_noop_component() -> ReservedTestClient {
        let components =
            Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op composition"));
        // #28: admission is fail-closed by default, so the fixture's tool has
        // to be enabled for the reserved client to reach it at all.
        let mut config = ServerRunConfig::in_memory_for_tests().with_capability_admission(
            lancet_protocol::CapabilityAdmissionPolicy {
                enabled_tools: std::collections::BTreeSet::from(["noop.echo".to_string()]),
                ..Default::default()
            },
        );
        let invocations = Arc::clone(&config.invocations);
        config.components = Some(components);

        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();

        let (client, server) = CepTransport::pair(64);
        let server_task = tokio::spawn(async move {
            let _ = server.run_native(registry, config).await;
        });

        // Project the Component descriptor into a ToolDefinition — same
        // shape as LancetRuntime::build (D.4).
        let components = ComponentRuntime::noop_fixture().expect("fixture");
        let definitions: Vec<lancet_api::ToolDefinition> = components
            .descriptor_identities()
            .map(|(descriptor, identity)| {
                lancet_api::ToolDefinition::new(
                    descriptor.name.clone(),
                    descriptor.description.clone(),
                    descriptor.params_schema.clone(),
                    identity,
                )
            })
            .collect();

        let adapter = TestCepClient::new(client, &definitions, ["noop.echo".to_string()]);
        tokio::task::yield_now().await;
        std::mem::forget(server_task);
        ReservedTestClient {
            adapter,
            invocations,
            definitions,
        }
    }

    #[tokio::test]
    async fn reserved_cep_client_executes_component_tool() {
        let adapter = reserved_client_with_noop_component().await;

        let result = adapter
            .execute_reserved(&ToolCall {
                function: FunctionCall {
                    name: "noop.echo".to_string(),
                    arguments: r#"{"message":"reserved-client"}"#.to_string(),
                },
            })
            .await
            .expect("reserved client executes component tool");

        assert!(
            result.contains("reserved-client"),
            "result must contain echoed message: {result}"
        );
    }

    #[tokio::test]
    async fn reserved_cep_client_rejects_disabled_component_tool() {
        // The no-op descriptor is in the available set, but not in the
        // enabled set → method_not_found.
        let components = ComponentRuntime::noop_fixture().expect("fixture");
        let definitions: Vec<lancet_api::ToolDefinition> = components
            .descriptor_identities()
            .map(|(descriptor, identity)| {
                lancet_api::ToolDefinition::new(
                    descriptor.name.clone(),
                    descriptor.description.clone(),
                    descriptor.params_schema.clone(),
                    identity,
                )
            })
            .collect();
        let (client, server) = CepTransport::pair(8);
        let server_task = tokio::spawn(async move {
            let _ = server
                .run_native(
                    MethodRegistry::new(),
                    ServerRunConfig::in_memory_for_tests(),
                )
                .await;
        });
        let adapter = TestCepClient::new(
            client,
            &definitions,
            Vec::<String>::new(), // nothing enabled
        );
        tokio::task::yield_now().await;
        std::mem::forget(server_task);

        let call = ToolCall {
            function: FunctionCall {
                name: "noop.echo".to_string(),
                arguments: r#"{"message":"x"}"#.to_string(),
            },
        };
        let err = adapter
            .execute(unreserved_binding(&call), &call)
            .await
            .expect_err("disabled tool must fail");
        assert_cep_payload(&err, jsonrpc::METHOD_NOT_FOUND);
    }
}

/// #71: the `[lancet.effects] deny` configuration path, from TOML to the
/// `denied_effect_kinds` set both pre-dispatch layers read.
///
/// The `lancet-native` transport-seam tests build the deny set in Rust, so
/// without this the snake_case wire names and the profile → policy translation
/// are only covered for `mutate.create` (through the LATTICE_BIN harness).
mod effects_deny_profile_path {
    use lancet_protocol::effect_types::ImpactKind;
    use lattice_profile::AgentProfile;
    use tempfile::TempDir;

    use crate::turn::cep::load_policy_config;

    fn lancet_section_from_toml(effects: &str) -> lattice_profile::LancetSection {
        let toml = format!(
            r#"
schema_version = 1

[agent]
name = "deny-path"
model = "sonnet"

[prompt]
system = "test"

[lancet.effects]
deny = [{effects}]
"#
        );
        toml::from_str::<AgentProfile>(&toml)
            .expect("profile parses")
            .lancet
    }

    #[test]
    fn profile_deny_names_reach_the_policy_denied_effect_kinds() {
        let root = TempDir::new().unwrap();

        // Both kinds the issue's acceptance criteria name, spelled the way an
        // operator writes them in `agent.toml`.
        let lancet = lancet_section_from_toml(r#""workspace_mutation", "workspace_observation""#);
        let policy = load_policy_config(&lancet, root.path()).expect("policy config");

        assert!(
            policy
                .denied_effect_kinds
                .contains(&ImpactKind::WorkspaceMutation),
            "workspace_mutation must reach the deny set: {:?}",
            policy.denied_effect_kinds
        );
        assert!(
            policy
                .denied_effect_kinds
                .contains(&ImpactKind::WorkspaceObservation),
            "workspace_observation must reach the deny set: {:?}",
            policy.denied_effect_kinds
        );
        assert_eq!(policy.denied_effect_kinds.len(), 2);
    }

    #[test]
    fn absent_effects_section_denies_nothing() {
        let root = TempDir::new().unwrap();
        let toml = r#"
schema_version = 1

[agent]
name = "no-deny"
model = "sonnet"

[prompt]
system = "test"
"#;
        let lancet = toml::from_str::<AgentProfile>(toml)
            .expect("profile parses")
            .lancet;
        let policy = load_policy_config(&lancet, root.path()).expect("policy config");
        assert!(
            policy.denied_effect_kinds.is_empty(),
            "no [lancet.effects] must deny nothing, got {:?}",
            policy.denied_effect_kinds
        );
    }

    #[test]
    fn an_unknown_effect_kind_name_fails_the_profile_closed() {
        let toml = r#"
schema_version = 1

[agent]
name = "typo"
model = "sonnet"

[prompt]
system = "test"

[lancet.effects]
deny = ["workspace_mutuation"]
"#;
        toml::from_str::<AgentProfile>(toml)
            .expect_err("a misspelled effect kind must not parse as an empty deny set");
    }
}
