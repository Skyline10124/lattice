// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Impact matrix: one representative method per `ImpactKind` round-trips
//! from catalog-derived LLM tool shape through `CepExecutorAdapter` and back.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lancet_catalog::CepMethod;
use lancet_catalog::catalog;
use lancet_catalog::methods::command::Run;
use lancet_catalog::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_catalog::methods::session::{TodoWrite, TodoWriteParams};
use lancet_catalog::methods::workspace::{Create, ObserveWorkspace};
use lancet_native::{
    CepAuditStore, CepTransport, DirectoryEntry, ExecOutput, ExecRequest, ExecutionBackend,
    ExecutionBackendError, ExecutionRouting, FindRequest, GrepMatch, GrepRequest, MethodRegistry,
    PathFacts, PathIntent, PathKind, SEMANTIC_STAGE_NATIVE, ServerRunConfig, UpperContext,
    WorkspacePath, register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;

use lancet_protocol::error::{CepErrorData, ExecutionErrorData, jsonrpc};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use lattice_agent::{ToolCallExecutor, ToolExecutionError};
use lattice_api::CepExecutorAdapter;
use lattice_llm::{FunctionCall, ToolCall};
use serde_json::Value;
use tempfile::TempDir;

/// Minimal host-filesystem backend for integration tests.
struct TestFsBackend {
    root: std::path::PathBuf,
}

impl TestFsBackend {
    fn new(root: impl Into<std::path::PathBuf>) -> Self {
        let raw = root.into();
        Self {
            root: std::fs::canonicalize(&raw).unwrap_or(raw),
        }
    }
}

impl ExecutionBackend for TestFsBackend {
    fn backend_name(&self) -> &str {
        "test-fs"
    }

    fn resolve_path(
        &self,
        path: &WorkspacePath,
        _intent: PathIntent,
    ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        let path_str = path.as_str().to_string();
        let exists = full.exists();
        let kind = if full.is_dir() {
            PathKind::Directory
        } else {
            PathKind::File
        };
        Box::pin(async move {
            Ok(PathFacts {
                normalized_path: path_str,
                exists,
                kind: Some(kind),
                parent_exists: true,
                parent_kind: Some(PathKind::Directory),
                byte_size: None,
                canonical_under_workspace: true,
            })
        })
    }

    fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        Box::pin(async move {
            std::fs::read(&full).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn write_file(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        let content = content.to_vec();
        Box::pin(async move {
            std::fs::write(&full, &content).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn remove_file(
        &self,
        path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
    {
        let full = if path.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(path.as_str())
        };
        Box::pin(async move {
            std::fs::remove_file(&full).map_err(|e| ExecutionBackendError::Unavailable {
                message: e.to_string(),
            })
        })
    }

    fn list_dir(
        &self,
        _path: &WorkspacePath,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn grep(
        &self,
        _request: &GrepRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn find_files(
        &self,
        _request: &FindRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn exec(
        &self,
        request: &ExecRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ExecOutput, ExecutionBackendError>> + Send + '_>>
    {
        let cwd = if request.cwd.is_project_root() {
            self.root.clone()
        } else {
            self.root.join(request.cwd.as_str())
        };
        let argv = request.argv.clone();
        let env = request.env.clone();
        Box::pin(async move {
            let mut cmd = std::process::Command::new(&argv[0]);
            cmd.args(&argv[1..]);
            cmd.current_dir(&cwd);
            for (k, v) in &env {
                cmd.env(k, v);
            }
            let output = cmd
                .output()
                .map_err(|e| ExecutionBackendError::Unavailable {
                    message: e.to_string(),
                })?;
            Ok(ExecOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            })
        })
    }

    fn changed_paths(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>> + Send + '_>>
    {
        Box::pin(async { Ok(Vec::new()) })
    }
}

struct ImpactRow {
    impact: ImpactKind,
    method: &'static str,
    arguments: Value,
    assert_result: fn(&Value),
}

async fn adapter_with_registry(registry: MethodRegistry) -> CepExecutorAdapter {
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

async fn fetch_only_adapter() -> CepExecutorAdapter {
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
    adapter_with_registry(registry).await
}

fn assert_catalog_method(method: &str) {
    assert!(
        catalog().iter().any(|descriptor| descriptor.name == method),
        "catalog missing method: {method}"
    );
}

fn llm_tool_call(method: &str, arguments: Value) -> ToolCall {
    assert_catalog_method(method);
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

    let (adapter, _audit) = production_adapter(
        root.to_str().unwrap(),
        &[
            ObserveWorkspace::NAME,
            Create::NAME,
            Run::NAME,
            TodoWrite::NAME,
        ],
        false,
    )
    .await;

    let rows = [
        ImpactRow {
            impact: ImpactKind::WorkspaceObservation,
            method: ObserveWorkspace::NAME,
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
            method: Create::NAME,
            arguments: serde_json::json!({
                "path": "created.txt",
                "content": "written via adapter"
            }),
            assert_result: |value| {
                assert_eq!(value["path"].as_str(), Some("created.txt"));
                assert!(value["bytes_written"].as_u64().unwrap_or(0) > 0);
            },
        },
        ImpactRow {
            impact: ImpactKind::CommandExecution,
            method: Run::NAME,
            arguments: serde_json::json!({
                "argv": ["echo", "cep-round-trip"]
            }),
            assert_result: |value| {
                assert_eq!(value["exit_code"].as_i64(), Some(0));
                assert!(
                    value["stdout"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("cep-round-trip")
                );
            },
        },
        ImpactRow {
            impact: ImpactKind::SessionStateMutation,
            method: TodoWrite::NAME,
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
        let parsed: Value = serde_json::from_str(&result_json).expect("AgentLoop tool-result JSON");
        (row.assert_result)(&parsed);
    }

    let fetch_adapter = fetch_only_adapter().await;
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
async fn cep_round_trip_unknown_method_rejected_at_adapter() {
    let dir = TempDir::new().unwrap();
    let (adapter, _audit) = production_adapter(
        dir.path().to_str().unwrap(),
        &[ObserveWorkspace::NAME],
        false,
    )
    .await;

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
async fn cep_round_trip_schema_mismatch_preserves_field_path() {
    let dir = TempDir::new().unwrap();
    let (adapter, _audit) = production_adapter(
        dir.path().to_str().unwrap(),
        &[ObserveWorkspace::NAME],
        false,
    )
    .await;

    let err = adapter
        .execute(&llm_tool_call(
            ObserveWorkspace::NAME,
            serde_json::json!({ "mode": "read_file" }),
        ))
        .await
        .expect_err("missing required path must fail");

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

    let mut registry = MethodRegistry::new();
    registry
        .register_method::<TodoWrite, _, _>({
            let server_err = Arc::clone(&server_err);
            move |_params: TodoWriteParams, _ctx| {
                let err = Arc::clone(&server_err);
                async move { Err((*err).clone()) }
            }
        })
        .expect("todo handler");
    let adapter = adapter_with_registry(registry).await;

    let err = adapter
        .execute(&llm_tool_call(
            TodoWrite::NAME,
            serde_json::json!({ "todos": [] }),
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

async fn production_adapter(
    root: &str,
    enabled: &[&str],
    allow_localhost: bool,
) -> (CepExecutorAdapter, Arc<CepAuditStore>) {
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
    // Direct mode: allow writes under the project root and echo as a diagnostic command.
    policy_config.workspace = WorkspacePolicy {
        allowed_write_paths: vec![".".into()],
    };
    policy_config.command.allowed_diagnostic_commands.push(vec!["echo".into()]);

    let (client, server) = CepTransport::pair(16);
    let audit = Arc::new(CepAuditStore::in_memory().expect("audit"));
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = Arc::clone(&audit);
    config.cep_context = Arc::new(CepTurnContext::from_policy(policy));
    let default_hook = Arc::new(DefaultPolicyHook::new(policy_config));
    config.policy_hook = default_hook.clone();
    config.default_policy_hook = Some(default_hook);
    config.execution = Some(ExecutionRouting {
        backend: Arc::new(TestFsBackend::new(root)),
        upper: UpperContext {
            execution_backend: "test-fs".into(),
            upper_workspace_id: "test-upper".into(),
        },
    });

    let server_task = tokio::spawn(async move {
        let _ = server.run(registry, config).await;
    });
    let enabled_names: Vec<String> = enabled.iter().map(|s| (*s).to_string()).collect();
    let adapter = CepExecutorAdapter::new(client, enabled_names);
    tokio::task::yield_now().await;
    std::mem::forget(server_task);
    (adapter, audit)
}

#[tokio::test]
async fn web_fetch_and_todo_write_audit_without_workspace_envelope() {
    let dir = TempDir::new().expect("temp workspace");
    let root = dir.path();
    let fetch_url = spawn_echo_server("200 OK", "cep-body").await;

    let (adapter, audit) = production_adapter(
        root.to_str().unwrap(),
        &[Fetch::NAME, TodoWrite::NAME],
        true,
    )
    .await;

    let fetch_call = llm_tool_call(Fetch::NAME, serde_json::json!({ "url": fetch_url }));
    let fetch_result = adapter.execute(&fetch_call).await.expect("fetch.web");
    let fetch_parsed: Value = serde_json::from_str(&fetch_result).expect("fetch json");
    assert_eq!(fetch_parsed["body"].as_str(), Some("cep-body"));

    let todo_call = llm_tool_call(
        TodoWrite::NAME,
        serde_json::json!({
            "todos": [{ "content": "secret todo text", "status": "in_progress" }]
        }),
    );
    adapter.execute(&todo_call).await.expect("todo_write");

    let fetch_row = audit.row_by_audit_id(1).expect("fetch.web audit row");
    assert_eq!(fetch_row.method, Fetch::NAME);
    assert_eq!(fetch_row.impact_kind, "external_observation");
    assert_eq!(fetch_row.semantic_stage, SEMANTIC_STAGE_NATIVE);
    assert!(fetch_row.policy_enforced);
    let fetch_metadata = fetch_row.metadata_json.expect("fetch metadata");
    assert!(fetch_metadata.contains("response_size"));
    assert!(!fetch_metadata.contains("cep-body"));
    assert!(!fetch_metadata.contains("secret todo"));

    let todo_row = audit.row_by_audit_id(2).expect("todo audit row");
    assert_eq!(todo_row.method, TodoWrite::NAME);
    assert_eq!(todo_row.impact_kind, "session_state_mutation");
    assert_eq!(todo_row.semantic_stage, SEMANTIC_STAGE_NATIVE);
    let todo_metadata = todo_row.metadata_json.expect("todo metadata");
    assert!(todo_metadata.contains("op_count"));
    assert!(!todo_metadata.contains("secret todo"));
}
