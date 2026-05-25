//! VER-28 impact matrix: one representative method per `ImpactKind` round-trips
//! from catalog-derived LLM tool shape through `LancetExecutorAdapter` and back.

use std::sync::Arc;

use lancet_executor::{
    register_phase20_legacy_backend, to_tool_definitions, LancetTransport, MethodRegistry,
    ServerRunConfig,
};
use lancet_protocol::catalog;
use lancet_protocol::envelope::ImpactKind;
use lancet_protocol::error::{jsonrpc, CepErrorData, ExecutionErrorData};
use lancet_protocol::methods::command::Run;
use lancet_protocol::methods::external::{Fetch, FetchParams, FetchResult};
use lancet_protocol::methods::session::{TodoWrite, TodoWriteParams};
use lancet_protocol::methods::workspace::{ReadFile, WriteFile};
use lancet_protocol::{CepError, CepErrorCode, CepMethod, Retriability};
use lattice_agent::{ToolCallExecutor, ToolExecutionError};
use lattice_api::LancetExecutorAdapter;
use lattice_llm::{FunctionCall, ToolCall};
use serde_json::Value;
use tempfile::TempDir;

struct ImpactRow {
    impact: ImpactKind,
    method: &'static str,
    arguments: Value,
    assert_result: fn(&Value),
}

fn test_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("http client")
}

async fn adapter_with_registry(registry: MethodRegistry) -> LancetExecutorAdapter {
    let (client, server) = LancetTransport::pair(8);
    let server_task = tokio::spawn(async move {
        let _ = server
            .run(registry, ServerRunConfig::in_memory_for_tests())
            .await;
    });
    let enabled: Vec<String> = catalog().iter().map(|d| d.name.to_string()).collect();
    let adapter = LancetExecutorAdapter::new(client, enabled);
    tokio::task::yield_now().await;
    std::mem::forget(server_task);
    adapter
}

async fn legacy_adapter(root: &str) -> LancetExecutorAdapter {
    let mut registry = MethodRegistry::new();
    register_phase20_legacy_backend(&mut registry, root, test_http_client())
        .expect("legacy backend registration");
    adapter_with_registry(registry).await
}

async fn fetch_only_adapter() -> LancetExecutorAdapter {
    let mut registry = MethodRegistry::new();
    registry
        .register_method::<Fetch, _, _>(|_params: FetchParams, _ctx| async move {
            Ok(FetchResult {
                status: 200,
                body: "external observation body".into(),
                content_type: Some("text/plain".into()),
            })
        })
        .expect("fetch handler");
    adapter_with_registry(registry).await
}

fn tool_definition_for(method: &str) -> lattice_llm::ToolDefinition {
    to_tool_definitions(catalog())
        .into_iter()
        .find(|tool| tool.name == method)
        .unwrap_or_else(|| panic!("catalog projection missing method: {method}"))
}

fn llm_tool_call(method: &str, arguments: Value) -> ToolCall {
    let _tool = tool_definition_for(method);
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

    let legacy = legacy_adapter(root.to_str().unwrap()).await;

    let rows = [
        ImpactRow {
            impact: ImpactKind::WorkspaceObservation,
            method: ReadFile::NAME,
            arguments: serde_json::json!({
                "path": "hello.txt",
                "offset": 1,
                "limit": 10
            }),
            assert_result: |value| {
                let content = value["content"].as_str().unwrap_or_default();
                assert!(content.contains("hello impact matrix"));
            },
        },
        ImpactRow {
            impact: ImpactKind::WorkspaceMutation,
            method: WriteFile::NAME,
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
                assert!(value["stdout"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("cep-round-trip"));
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
        let result_json = legacy
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
        .expect("web.fetch round-trip");
    let fetch_parsed: Value = serde_json::from_str(&fetch_result).expect("fetch result JSON");
    assert_eq!(
        fetch_parsed["body"].as_str(),
        Some("external observation body")
    );
    assert_eq!(fetch_parsed["status"].as_u64(), Some(200));
}

#[tokio::test]
async fn cep_round_trip_unknown_method_rejected_at_adapter() {
    let adapter = legacy_adapter(TempDir::new().unwrap().path().to_str().unwrap()).await;

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
    let adapter = legacy_adapter(TempDir::new().unwrap().path().to_str().unwrap()).await;

    let err = adapter
        .execute(&llm_tool_call(ReadFile::NAME, serde_json::json!({})))
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

#[test]
fn cep_round_trip_catalog_names_match_tool_definitions() {
    for descriptor in catalog() {
        let tool = tool_definition_for(descriptor.name);
        assert_eq!(tool.name, descriptor.name);
        assert!(tool.name.contains('.'));
    }
}
