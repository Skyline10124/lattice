use std::sync::Arc;

use lattice_agent::{ToolCallExecutor, ToolExecutionError};
use lattice_cep_catalog::catalog;
use lattice_cep_catalog::methods::command::Run;
use lattice_cep_catalog::methods::session::{TodoWrite, TodoWriteParams, TodoWriteResult};
use lattice_cep_catalog::methods::workspace::{ReadFile, ReadFileResult};
use lattice_cep_catalog::CepMethod;
use lattice_cep_native::{CepTransport, MethodRegistry, ServerRunConfig};
use lattice_cep_protocol::envelope::ImpactKind;
use lattice_cep_protocol::error::{
    jsonrpc, CepErrorData, EffectErrorData, ExecutionErrorData, GuardErrorData,
    PermissionErrorData, StateErrorData, ValidationErrorData,
};
use lattice_cep_protocol::{CepError, CepErrorCode, Retriability};
use lattice_llm::{FunctionCall, ToolCall};

use super::cep::CepExecutorAdapter;

fn todo_registry<F, Fut>(handler: F) -> MethodRegistry
where
    F: Fn(TodoWriteParams, lattice_cep_native::HandlerContext) -> Fut + Send + Sync + 'static,
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
            .run(registry, ServerRunConfig::in_memory_for_tests())
            .await;
    });
    let enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    let adapter = CepExecutorAdapter::new(client, enabled);
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
async fn cep_executor_adapter_rejects_profile_disabled_command_run() {
    let adapter =
        CepExecutorAdapter::new(CepTransport::pair(8).0, ["workspace.read_file".to_string()]);
    let err = adapter
        .execute(&ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: Run::NAME.to_string(),
                arguments: r#"{"argv":["echo","blocked"]}"#.to_string(),
            },
        })
        .await
        .expect_err("disabled catalog method must fail before server dispatch");

    assert_cep_payload(&err, jsonrpc::METHOD_NOT_FOUND);
}

#[tokio::test]
async fn cep_executor_adapter_allows_enabled_workspace_read_file() {
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<ReadFile, _, _>(|params, _ctx| async move {
            Ok(ReadFileResult {
                content: params.path,
                content_hash: None,
            })
        })
        .unwrap();
    let (client, server) = CepTransport::pair(8);
    let server_task = tokio::spawn(async move {
        let _ = server
            .run(registry, ServerRunConfig::in_memory_for_tests())
            .await;
    });
    let adapter = CepExecutorAdapter::new(client, [ReadFile::NAME.to_string()]);
    tokio::task::yield_now().await;
    std::mem::forget(server_task);

    let result = adapter
        .execute(&ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: ReadFile::NAME.to_string(),
                arguments: r#"{"path":"src/lib.rs"}"#.to_string(),
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
        .register_method::<ReadFile, _, _>(|params, _ctx| async move {
            Ok(ReadFileResult {
                content: params.path,
                content_hash: None,
            })
        })
        .unwrap();
    let adapter = adapter_over_registry(registry).await;

    let err = adapter
        .execute(&ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: ReadFile::NAME.to_string(),
                arguments: r#"{}"#.to_string(),
            },
        })
        .await
        .expect_err("schema mismatch must fail");

    assert_cep_payload(&err, jsonrpc::INVALID_PARAMS);
    let field = err
        .cep_error
        .as_ref()
        .unwrap()
        .data
        .get("field")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert_eq!(field, "path");
}

macro_rules! cep_family_test {
    ($name:ident, $code:expr, $data:expr, $retriability:expr) => {
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
        active_envelope_id: None,
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
        method: "workspace.read_file".into(),
        declared_effect: vec![ImpactKind::WorkspaceObservation],
        authorized_effect: vec![ImpactKind::WorkspaceObservation],
        observed_effects: vec![
            ImpactKind::WorkspaceObservation,
            ImpactKind::WorkspaceMutation,
        ],
        violations: vec!["observed workspace_mutation not in declared effect ceiling".into(),],
    })),
    Retriability::Permanent
);

#[tokio::test]
async fn turn_runtime_constructs_one_adapter_per_turn() {
    use tempfile::TempDir;

    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_agent::LlmClient;

    use super::cep::build_production_turn_deps;
    use super::llm::RealLlmClient;

    let dir = TempDir::new().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(vec![lattice_llm::LlmEvent::Done {
                finish_reason: "stop".to_string(),
                usage: None,
            }])
            .build(),
    );

    let deps = build_production_turn_deps(
        dir.path().to_str().unwrap(),
        &[],
        llm,
        true,
        None,
        "test-model",
        "0.0.0-test",
    )
    .expect("production runtime should build");

    assert_eq!(deps.build_metrics.adapter_instances, 1);
    assert_eq!(deps.build_metrics.transport_pairs, 1);

    let deps2 = build_production_turn_deps(
        dir.path().to_str().unwrap(),
        &["workspace.read_file".to_string()],
        Arc::new(RealLlmClient),
        false,
        None,
        "test-model",
        "0.0.0-test",
    )
    .expect("second runtime build");
    assert_eq!(deps2.build_metrics.adapter_instances, 1);
    assert_eq!(deps2.build_metrics.transport_pairs, 1);
}

#[cfg(test)]
mod cep_native_registry {
    use super::*;
    use lattice_cep_catalog::methods::envelope::{Declare, SubmitIntent};
    use lattice_cep_catalog::methods::external::Fetch;
    use lattice_cep_catalog::methods::session::TodoWrite;
    use lattice_cep_catalog::methods::workspace::ReadFile;
    use lattice_cep_native::SEMANTIC_STAGE_NATIVE;
    use lattice_cep_protocol::envelope::Capability;
    use lattice_profile::CepNetworkSection;
    use tempfile::TempDir;

    use crate::turn::cep::production_base_envelope;

    #[test]
    fn cep_native_registry_production_envelope_has_no_fetch_wildcard() {
        let dir = TempDir::new().unwrap();
        let enabled = vec![Fetch::NAME.to_string(), TodoWrite::NAME.to_string()];
        let envelope =
            production_base_envelope(dir.path().to_str().unwrap(), &enabled, None).unwrap();
        assert!(
            envelope
                .inner
                .capabilities
                .iter()
                .all(|cap| !matches!(cap, Capability::FetchUrl { host } if host == "*")),
            "production envelope must not grant FetchUrl(\"*\")"
        );
    }

    #[test]
    fn production_base_envelope_grants_read_audit() {
        let dir = TempDir::new().unwrap();
        let enabled = vec![ReadFile::NAME.to_string()];
        let envelope =
            production_base_envelope(dir.path().to_str().unwrap(), &enabled, None).unwrap();
        assert!(
            envelope
                .inner
                .capabilities
                .iter()
                .any(|c| matches!(c, Capability::ReadAudit)),
            "base envelope must grant ReadAudit for audit.query/replay"
        );
    }

    #[test]
    fn production_base_envelope_grants_session_state_for_envelope_methods() {
        let dir = TempDir::new().unwrap();
        let enabled = vec![Declare::NAME.to_string(), SubmitIntent::NAME.to_string()];
        let envelope =
            production_base_envelope(dir.path().to_str().unwrap(), &enabled, None).unwrap();
        assert!(
            envelope
                .inner
                .capabilities
                .iter()
                .any(|c| matches!(c, Capability::MutateSessionState)),
            "base envelope must grant MutateSessionState for envelope session-state methods"
        );
    }

    #[test]
    fn cep_native_registry_allowed_hosts_become_fetch_capabilities() {
        let dir = TempDir::new().unwrap();
        let cep = lattice_profile::CepSection {
            network: Some(CepNetworkSection {
                allowed_hosts: vec!["api.example.com".into()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let enabled = vec![Fetch::NAME.to_string()];
        let envelope =
            production_base_envelope(dir.path().to_str().unwrap(), &enabled, Some(&cep)).unwrap();
        assert!(envelope.inner.capabilities.iter().any(|cap| matches!(
            cap,
            Capability::FetchUrl { host } if host == "api.example.com"
        )));
    }

    #[tokio::test]
    async fn cep_native_registry_production_runtime_uses_native_audit_stage() {
        use lattice_agent::testing::FakeStreamingLlmClient;
        use lattice_agent::LlmClient;
        use lattice_cep_catalog::methods::workspace::ReadFileParams;

        use crate::turn::cep::build_production_turn_deps;

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

        let deps = build_production_turn_deps(
            dir.path().to_str().unwrap(),
            &[ReadFile::NAME.to_string()],
            llm,
            true,
            None,
            "sonnet-test",
            env!("CARGO_PKG_VERSION"),
        )
        .expect("production deps");

        deps.cep_client
            .call::<ReadFile>(ReadFileParams {
                path: "hello.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            })
            .await
            .expect("native read_file");

        let row = deps.audit.row_by_audit_id(1).unwrap();
        assert_eq!(row.semantic_stage, SEMANTIC_STAGE_NATIVE);
        assert!(row.policy_enforced);
        assert_eq!(row.impact_kind, "workspace_observation");
        assert_eq!(row.model_id.as_deref(), Some("sonnet-test"));
        assert_eq!(
            row.runtime_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }
}

#[cfg(test)]
mod attestation_and_dispatch {
    use super::*;
    use lattice_agent::testing::FakeStreamingLlmClient;
    use lattice_agent::LlmClient;
    use lattice_cep_catalog::methods::audit::{AuditReplay, AuditReplayParams};
    use lattice_cep_catalog::methods::envelope::{SubmitIntent, SubmitIntentParams};
    use lattice_cep_catalog::methods::workspace::{ReadFile, ReadFileParams};
    use lattice_cep_native::canonicalize_in_toto_statement;
    use lattice_llm::{FunctionCall, ToolCall};
    use tempfile::TempDir;

    use crate::turn::cep::{
        build_production_turn_deps, generate_and_store_attestation, CepExecutorAdapter,
    };

    #[tokio::test]
    async fn dispatch_typed_routes_submit_intent() {
        let dir = TempDir::new().unwrap();
        let llm: Arc<dyn LlmClient> = Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(vec![lattice_llm::LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                }])
                .build(),
        );
        let deps = build_production_turn_deps(
            dir.path().to_str().unwrap(),
            &[SubmitIntent::NAME.to_string()],
            llm,
            true,
            None,
            "test-model",
            "0.0.0-test",
        )
        .expect("deps");

        let adapter =
            CepExecutorAdapter::new(deps.cep_client.clone(), [SubmitIntent::NAME.to_string()]);
        let call = ToolCall {
            id: "call_submit".into(),
            function: FunctionCall {
                name: SubmitIntent::NAME.to_string(),
                arguments: serde_json::to_string(&SubmitIntentParams {
                    action: "write".into(),
                    target: "workspace.write_file".into(),
                    params: None,
                    supersede: false,
                    reason: None,
                })
                .unwrap(),
            },
        };
        let output = adapter
            .execute(&call)
            .await
            .expect("SubmitIntent must route through the typed CEP adapter");
        let result: serde_json::Value = serde_json::from_str(&output).expect("json result");
        assert_eq!(result["accepted"], true);
    }

    #[tokio::test]
    async fn attestation_matches_replay_statement() {
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
        let deps = build_production_turn_deps(
            dir.path().to_str().unwrap(),
            &[ReadFile::NAME.to_string()],
            llm,
            true,
            None,
            "sonnet-attest",
            env!("CARGO_PKG_VERSION"),
        )
        .expect("deps");

        let turn_id = "sess-1:root".to_string();
        {
            let mut ctx = deps.audit_context.write().await;
            ctx.turn_id = turn_id.clone();
            ctx.session_id = "sess-1".to_string();
            ctx.model_id = "sonnet-attest".to_string();
            ctx.runtime_version = env!("CARGO_PKG_VERSION").to_string();
        }

        deps.cep_client
            .call::<ReadFile>(ReadFileParams {
                path: "hello.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            })
            .await
            .expect("read");

        deps.audit.await_pending().await;
        let ctx = deps.audit_context.read().await;
        generate_and_store_attestation(&deps.audit, &ctx, &turn_id).expect("store attestation");

        let replay = deps
            .cep_client
            .call::<AuditReplay>(AuditReplayParams {
                turn_id: turn_id.clone(),
            })
            .await
            .expect("replay");

        let stored = deps
            .audit
            .attestation_for_turn(&turn_id)
            .expect("lookup")
            .expect("stored row");
        let stored_stmt: lattice_cep_protocol::provenance::InTotoStatement =
            serde_json::from_str(&stored.statement_json).expect("stored json");
        let replay_stmt = replay.statement.expect("replay statement");

        assert_eq!(
            canonicalize_in_toto_statement(&stored_stmt),
            canonicalize_in_toto_statement(&replay_stmt)
        );
    }

    #[tokio::test]
    async fn replay_without_stored_attestation() {
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
        let deps = build_production_turn_deps(
            dir.path().to_str().unwrap(),
            &[ReadFile::NAME.to_string()],
            llm,
            true,
            None,
            "m",
            "v",
        )
        .expect("deps");

        let turn_id = "sess-2:root".to_string();
        {
            let mut ctx = deps.audit_context.write().await;
            ctx.turn_id = turn_id.clone();
            ctx.session_id = "sess-2".to_string();
        }

        deps.cep_client
            .call::<ReadFile>(ReadFileParams {
                path: "a.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            })
            .await
            .expect("read");

        deps.audit.await_pending().await;
        assert!(deps
            .audit
            .attestation_for_turn(&turn_id)
            .expect("lookup")
            .is_none());

        let replay = deps
            .cep_client
            .call::<AuditReplay>(AuditReplayParams { turn_id })
            .await
            .expect("replay without stored attestation");
        assert!(replay.statement.is_some());
    }
}
