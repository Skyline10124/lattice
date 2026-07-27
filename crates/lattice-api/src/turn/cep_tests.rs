// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use lancet_catalog::CepMethod;
use lancet_catalog::catalog;
use lancet_catalog::methods::session::{TodoWrite, TodoWriteParams, TodoWriteResult};
use lancet_catalog::methods::workspace::{
    ObserveWorkspace, ObserveWorkspaceParams, ObserveWorkspaceResult, ReadFileResult,
};
use lancet_native::{CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::error::{
    CepErrorData, EffectErrorData, ExecutionErrorData, GuardErrorData, PermissionErrorData,
    StateErrorData, ValidationErrorData, jsonrpc,
};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use lattice_agent::{ToolCallExecutor, ToolExecutionError};
use lattice_llm::{FunctionCall, ToolCall};

use super::cep::CepExecutorAdapter;

fn todo_registry<F, Fut>(handler: F) -> MethodRegistry
where
    F: Fn(TodoWriteParams, lancet_native::HandlerContext) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<TodoWriteResult, CepError>> + Send + 'static,
{
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<TodoWrite, _, _>(handler)
        .unwrap();
    registry
}

async fn adapter_over_registry(registry: MethodRegistry) -> CepExecutorAdapter {
    let (client, server) = CepTransport::pair(8);
    let server_task = tokio::spawn(async move {
        let _ = server
            .run_native(registry, ServerRunConfig::in_memory_for_tests())
            .await;
    });
    // D.5: the adapter's available set is driven by runtime definitions,
    // not by hardcoded catalog membership. Project the catalog definitions
    // and pass them — same shape as LancetRuntime::build. `lancet_cep` is
    // a dev-dependency; `lancet_api::ToolDefinition` re-exports the same type.
    let definitions = lancet_cep::project_tool_definitions(catalog());
    let enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    let adapter = CepExecutorAdapter::new(client, &definitions, enabled);
    tokio::task::yield_now().await;
    std::mem::forget(server_task);
    adapter
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
async fn cep_executor_adapter_retired_methods_return_method_not_found() {
    let adapter = adapter_over_registry(todo_registry(|params, _ctx| async move {
        Ok(TodoWriteResult {
            todos: params.todos,
        })
    }))
    .await;

    for method in ["observe.audit", "observe.audit_replay", "mutate.batch"] {
        let err = adapter
            .execute(&ToolCall {
                id: format!("call_{method}"),
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
async fn cep_executor_adapter_parse_error_maps_to_32700() {
    let adapter = adapter_over_registry(todo_registry(|params, _ctx| async move {
        Ok(TodoWriteResult {
            todos: params.todos,
        })
    }))
    .await;

    let err = adapter
        .execute(&ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: TodoWrite::NAME.to_string(),
                arguments: "{not-json".to_string(),
            },
        })
        .await
        .expect_err("malformed JSON must fail");

    assert_cep_payload(&err, jsonrpc::PARSE_ERROR);
}

#[tokio::test]
async fn cep_executor_adapter_allows_enabled_workspace_read_file() {
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<ObserveWorkspace, _, _>(|params, _ctx| async move {
            let ObserveWorkspaceParams::ReadFile(params) = params else {
                panic!("expected read_file mode");
            };
            Ok(ObserveWorkspaceResult::ReadFile(ReadFileResult {
                content: params.path,
                content_hash: None,
            }))
        })
        .unwrap();
    let (client, server) = CepTransport::pair(8);
    let server_task = tokio::spawn(async move {
        let _ = server
            .run_native(registry, ServerRunConfig::in_memory_for_tests())
            .await;
    });
    let definitions = lancet_cep::project_tool_definitions(catalog());
    let adapter =
        CepExecutorAdapter::new(client, &definitions, [ObserveWorkspace::NAME.to_string()]);
    tokio::task::yield_now().await;
    std::mem::forget(server_task);

    let result = adapter
        .execute(&ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: ObserveWorkspace::NAME.to_string(),
                arguments: r#"{"mode":"read_file","path":"src/lib.rs"}"#.to_string(),
            },
        })
        .await
        .expect("enabled tool should execute");
    assert!(result.contains("src/lib.rs"));
}

#[tokio::test]
async fn cep_executor_adapter_unknown_method_maps_to_32601() {
    let adapter = adapter_over_registry(MethodRegistry::new()).await;

    let err = adapter
        .execute(&ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "crafted.unknown".to_string(),
                arguments: "{}".to_string(),
            },
        })
        .await
        .expect_err("unknown method must fail before server dispatch");

    assert_cep_payload(&err, jsonrpc::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn cep_executor_adapter_invalid_params_maps_to_32602() {
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<ObserveWorkspace, _, _>(|params, _ctx| async move {
            let ObserveWorkspaceParams::ReadFile(params) = params else {
                panic!("expected read_file mode");
            };
            Ok(ObserveWorkspaceResult::ReadFile(ReadFileResult {
                content: params.path,
                content_hash: None,
            }))
        })
        .unwrap();
    let adapter = adapter_over_registry(registry).await;

    let err = adapter
        .execute(&ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: ObserveWorkspace::NAME.to_string(),
                arguments: r#"{"mode":"read_file"}"#.to_string(),
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
            let adapter = adapter_over_registry(todo_registry({
                let server_err = Arc::clone(&server_err);
                move |_params, _ctx| {
                    let err = Arc::clone(&server_err);
                    async move { Err((*err).clone()) }
                }
            }))
            .await;

            let err = adapter
                .execute(&ToolCall {
                    id: "call_1".to_string(),
                    function: FunctionCall {
                        name: TodoWrite::NAME.to_string(),
                        arguments: r#"{"todos":[]}"#.to_string(),
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
    cep_executor_adapter_validation_family_preserves_payload,
    CepErrorCode::SchemaMismatch,
    CepErrorData::Validation(ValidationErrorData {
        field: "path".into(),
        reason: "bad".into(),
    }),
    Retriability::Permanent
);

cep_family_test!(
    cep_executor_adapter_permission_family_preserves_payload,
    CepErrorCode::CapabilityNotGranted,
    CepErrorData::Permission(PermissionErrorData {
        required_capability: "read_path".into(),
        granted_capabilities: vec![],
    }),
    Retriability::Permanent
);

cep_family_test!(
    cep_executor_adapter_execution_family_preserves_payload,
    CepErrorCode::ExecutionFailed,
    CepErrorData::Execution(ExecutionErrorData {
        command: "ls".into(),
        exit_code: Some(1),
        stderr: Some("failed".into()),
    }),
    Retriability::Permanent
);

cep_family_test!(
    cep_executor_adapter_state_family_preserves_payload,
    CepErrorCode::StaleContext,
    CepErrorData::State(StateErrorData {
        resource: "session".into(),
        reason: "stale".into(),
    }),
    Retriability::Permanent
);

cep_family_test!(
    cep_executor_adapter_guard_family_preserves_payload,
    CepErrorCode::GuardDenied,
    CepErrorData::Guard(GuardErrorData {
        method: TodoWrite::NAME.into(),
        reason: "denied".into(),
    }),
    Retriability::Permanent
);

cep_family_test!(
    cep_executor_adapter_effect_family_preserves_payload,
    CepErrorCode::EffectViolation,
    CepErrorData::Effect(Box::new(EffectErrorData {
        method: ObserveWorkspace::NAME.into(),
        declared_effect: vec![ImpactKind::WorkspaceObservation],
        authorized_effect: vec![ImpactKind::WorkspaceObservation],
        observed_effects: vec![
            ImpactKind::WorkspaceObservation,
            ImpactKind::WorkspaceMutation,
        ],
        violations: vec!["observed workspace_mutation not in declared effect ceiling".into(),],
        cell_ref: None,
        capability: None,
    })),
    Retriability::Permanent
);

#[tokio::test]
async fn turn_runtime_constructs_one_adapter_per_turn() {
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
    // the real platform state directory.
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

    assert_eq!(deps.build_metrics.adapter_instances, 1);
    assert_eq!(deps.build_metrics.transport_pairs, 1);

    let deps2 = build_test_production_deps(
        dir.path().to_str().unwrap(),
        &["session.todo_write".to_string()],
        Arc::new(RealLlmClient),
        false,
        None,
    )
    .await;
    assert_eq!(deps2.build_metrics.adapter_instances, 1);
    assert_eq!(deps2.build_metrics.transport_pairs, 1);
}

#[cfg(test)]
mod cep_native_registry {
    use super::*;
    use lancet_catalog::methods::session::TodoWrite;
    use lancet_native::SEMANTIC_STAGE_NATIVE;
    use tempfile::TempDir;

    #[tokio::test]
    async fn cep_native_registry_production_runtime_uses_native_audit_stage() {
        use lattice_agent::LlmClient;
        use lattice_agent::testing::FakeStreamingLlmClient;

        use crate::turn::test_support::build_test_production_deps;

        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "native audit").unwrap();

        let llm: Arc<dyn LlmClient> = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(vec![lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                }])
                .build(),
        );

        let deps = build_test_production_deps(
            dir.path().to_str().unwrap(),
            &[TodoWrite::NAME.to_string()],
            llm,
            true,
            None,
        )
        .await;

        deps.cep_client
            .call::<TodoWrite>(lancet_catalog::methods::session::TodoWriteParams {
                todos: vec![lancet_catalog::methods::session::TodoItem {
                    content: "native audit".into(),
                    status: lancet_catalog::methods::session::TodoStatus::Pending,
                }],
            })
            .await
            .expect("native session.todo_write");

        let row = deps.audit.row_by_audit_id(1).unwrap();
        assert_eq!(row.semantic_stage, SEMANTIC_STAGE_NATIVE);
        assert!(row.policy_enforced);
        assert_eq!(row.impact_kind, "session_state_mutation");
        assert_eq!(row.model_id.as_deref(), Some("test-model"));
        assert_eq!(row.runtime_version.as_deref(), Some("0.0.0-test"));
    }
}

#[cfg(test)]
mod attestation_and_dispatch {
    use super::*;
    use crate::turn::cep::build_envelope_turn_deps_for_test;
    use lancet_api::store_turn_attestation;
    use lancet_api::{AuditReplayRequest, replay_audit};
    use lancet_catalog::methods::session::TodoWrite;
    use lancet_native::canonicalize_in_toto_statement;
    use lattice_agent::LlmClient;
    use lattice_agent::testing::FakeStreamingLlmClient;
    use tempfile::TempDir;

    #[tokio::test]
    async fn attestation_matches_replay_statement() {
        let _xdg = lancet_audit::ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "attest").unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(vec![lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                }])
                .build(),
        );
        let deps = build_envelope_turn_deps_for_test(
            dir.path().to_str().unwrap(),
            &[TodoWrite::NAME.to_string()],
            llm,
            false,
            None,
            "sonnet-attest",
            env!("CARGO_PKG_VERSION"),
        )
        .await
        .expect("envelope turn deps");

        let turn_id = "sess-1:root".to_string();
        {
            let mut ctx = deps.audit_context.write().await;
            ctx.turn_id = turn_id.clone();
            ctx.session_id = "sess-1".to_string();
            ctx.model_id = "sonnet-attest".to_string();
            ctx.runtime_version = env!("CARGO_PKG_VERSION").to_string();
        }

        deps.cep_client
            .call::<TodoWrite>(lancet_catalog::methods::session::TodoWriteParams {
                todos: vec![lancet_catalog::methods::session::TodoItem {
                    content: "attest".into(),
                    status: lancet_catalog::methods::session::TodoStatus::Pending,
                }],
            })
            .await
            .expect("todo write");

        deps.audit.await_pending().await;
        let ctx = deps.audit_context.read().await;
        store_turn_attestation(&deps.audit, &ctx, &turn_id).expect("store attestation");

        let replay = replay_audit(AuditReplayRequest::new(
            dir.path(),
            "sess-1",
            turn_id.clone(),
        ))
        .await
        .expect("replay");

        let stored = deps
            .audit
            .attestation_for_turn(&turn_id)
            .expect("lookup")
            .expect("stored row");
        let stored_stmt: lancet_protocol::provenance::InTotoStatement =
            serde_json::from_str(&stored.statement_json).expect("stored json");
        let replay_stmt = replay.statement.expect("replay statement");

        assert_eq!(
            canonicalize_in_toto_statement(&stored_stmt),
            canonicalize_in_toto_statement(&replay_stmt)
        );
    }

    #[tokio::test]
    async fn replay_without_stored_attestation() {
        let _xdg = lancet_audit::ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(vec![lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                }])
                .build(),
        );
        let deps = build_envelope_turn_deps_for_test(
            dir.path().to_str().unwrap(),
            &[TodoWrite::NAME.to_string()],
            llm,
            false,
            None,
            "sonnet-attest",
            env!("CARGO_PKG_VERSION"),
        )
        .await
        .expect("envelope turn deps");

        let turn_id = "sess-2:root".to_string();
        {
            let mut ctx = deps.audit_context.write().await;
            ctx.turn_id = turn_id.clone();
            ctx.session_id = "sess-2".to_string();
        }

        deps.cep_client
            .call::<TodoWrite>(lancet_catalog::methods::session::TodoWriteParams {
                todos: vec![lancet_catalog::methods::session::TodoItem {
                    content: "x".into(),
                    status: lancet_catalog::methods::session::TodoStatus::Pending,
                }],
            })
            .await
            .expect("todo write");

        deps.audit.await_pending().await;
        assert!(
            deps.audit
                .attestation_for_turn(&turn_id)
                .expect("lookup")
                .is_none()
        );

        let replay = replay_audit(AuditReplayRequest::new(
            dir.path(),
            "sess-2",
            turn_id.clone(),
        ))
        .await
        .expect("replay without stored attestation");
        assert!(replay.statement.is_some());
    }
}

/// D.6: `CepExecutorAdapter` can execute a Component Tool using a
/// descriptor-projected definition. This proves the LLM tool-call path
/// is reachable for Component Tools (D.5's `call_raw` dispatch + D.4's
/// descriptor projection).
mod component_adapter {
    use super::*;
    use lancet_native::{
        CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
        register_production_method_registry,
    };
    use lancet_runtime::ComponentRuntime;
    use std::sync::Arc;

    /// Spawn a native server with the no-op Component fixture, then build
    /// a `CepExecutorAdapter` from the projected Component descriptor.
    async fn adapter_with_noop_component() -> CepExecutorAdapter {
        let components =
            Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op composition"));
        // #28: admission is fail-closed by default, so the fixture's tool has
        // to be enabled for the adapter to reach it at all.
        let mut config = ServerRunConfig::in_memory_for_tests().with_capability_admission(
            lancet_protocol::CapabilityAdmissionPolicy {
                enabled_tools: std::collections::BTreeSet::from(["noop.echo".to_string()]),
                ..Default::default()
            },
        );
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
            .descriptors()
            .map(|d| {
                lancet_api::ToolDefinition::new(
                    d.name.clone(),
                    d.description.clone(),
                    d.params_schema.clone(),
                )
            })
            .collect();

        let adapter = CepExecutorAdapter::new(client, &definitions, ["noop.echo".to_string()]);
        tokio::task::yield_now().await;
        std::mem::forget(server_task);
        adapter
    }

    #[tokio::test]
    async fn cep_executor_adapter_executes_component_tool() {
        let adapter = adapter_with_noop_component().await;

        let result = adapter
            .execute(&ToolCall {
                id: "call_noop".to_string(),
                function: FunctionCall {
                    name: "noop.echo".to_string(),
                    arguments: r#"{"message":"adapter"}"#.to_string(),
                },
            })
            .await
            .expect("adapter executes component tool");

        assert!(
            result.contains("adapter"),
            "result must contain echoed message: {result}"
        );
    }

    #[tokio::test]
    async fn cep_executor_adapter_rejects_disabled_component_tool() {
        // The no-op descriptor is in the available set, but not in the
        // enabled set → method_not_found.
        let components = ComponentRuntime::noop_fixture().expect("fixture");
        let definitions: Vec<lancet_api::ToolDefinition> = components
            .descriptors()
            .map(|d| {
                lancet_api::ToolDefinition::new(
                    d.name.clone(),
                    d.description.clone(),
                    d.params_schema.clone(),
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
        let adapter = CepExecutorAdapter::new(
            client,
            &definitions,
            Vec::<String>::new(), // nothing enabled
        );
        tokio::task::yield_now().await;
        std::mem::forget(server_task);

        let err = adapter
            .execute(&ToolCall {
                id: "call_noop".to_string(),
                function: FunctionCall {
                    name: "noop.echo".to_string(),
                    arguments: r#"{"message":"x"}"#.to_string(),
                },
            })
            .await
            .expect_err("disabled tool must fail");
        assert_cep_payload(&err, jsonrpc::METHOD_NOT_FOUND);
    }
}
