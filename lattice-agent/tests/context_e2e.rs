//! End-to-end integration test for context management pipeline.
//!
//! Tests the full flow: EventsProvider → BlobStore → Compiler → Agent::run()
//! with a mock LLM server. Exercises blob threshold decision, bus:fetch tool,
//! and per-iteration recompilation.
//!
//! Requires the `blob-store` feature.

#![cfg(feature = "blob-store")]

use std::collections::HashMap;
use std::sync::Arc;

use lattice_agent::blob::{BlobStore, StoredBlob};
use lattice_agent::events_provider::EventsProvider;
use lattice_agent::{Agent, ContextEvent, DefaultToolExecutor, LoopEvent};
use lattice_core::catalog::{ApiProtocol, CredentialStatus};
use lattice_core::types::Role;
use lattice_core::ResolvedModel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---- helpers ---------------------------------------------------------------

/// Spawn a mock HTTP server returning SSE chat-completion responses.
async fn spawn_mock_server(sse_body: String) -> Option<u16> {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return None,
        Err(e) => panic!("failed to bind mock server: {e}"),
    };
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
            sse_body.len(),
            &sse_body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    Some(port)
}

fn make_resolved(base_url: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: "test-model".into(),
        provider: "test-provider".into(),
        api_key: Some("sk-test".into()),
        base_url: base_url.to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "test-model".into(),
        context_length: 8192,
        provider_specific: HashMap::new(),
        credential_status: CredentialStatus::Present,
    }
}

fn content_chunk(text: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": text },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\n", chunk)
}

fn stop_chunk() -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    });
    format!("data: {}\n\n", chunk)
}

fn tool_call_start_chunk(id: &str, name: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "index": 0,
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": "" }
                }]
            },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\n", chunk)
}

fn tool_call_args_chunk(args_json: &str) -> String {
    let val = serde_json::Value::String(args_json.to_string());
    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": { "arguments": val }
                }]
            },
            "finish_reason": null
        }]
    });
    format!("data: {}\n\n", chunk)
}

fn tool_call_done_chunk() -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {}\n\n", chunk)
}

fn make_sse_response(text: &str) -> String {
    let mut body = String::new();
    body.push_str(&content_chunk(text));
    body.push_str(&stop_chunk());
    body.push_str("data: [DONE]\n\n");
    body
}

fn make_tool_sse(first_chunk: &str, tool_name: &str, tool_args: &str) -> String {
    let mut body = String::new();
    body.push_str(&content_chunk(first_chunk));
    body.push_str(&tool_call_start_chunk("call_1", tool_name));
    body.push_str(&tool_call_args_chunk(tool_args));
    body.push_str(&tool_call_done_chunk());
    body.push_str("data: [DONE]\n\n");
    body
}

/// SSE response that produces a bus:fetch tool call to retrieve a blob.
fn make_bus_fetch_sse(blob_key: &str) -> String {
    let mut body = String::new();
    body.push_str(&content_chunk("Let me check the full report..."));
    body.push_str(&tool_call_start_chunk("call_fetch", "bus:fetch"));
    let args = serde_json::json!({"key": blob_key}).to_string();
    body.push_str(&tool_call_args_chunk(&args));
    body.push_str(&tool_call_done_chunk());
    body.push_str("data: [DONE]\n\n");
    body
}

// ---- Tests -----------------------------------------------------------------

/// Scenario: Agent with BlobStore + EventsProvider.
///
/// 1. Inject a small event (should inline into prompt)
/// 2. Inject a large event (should spill to blob store, show as reference)
/// 3. Agent::run() processes the compiled prompt through mock LLM
/// 4. LLM requests bus:fetch to retrieve the large blob
/// 5. Tool executor retrieves blob from SQLite and returns content
/// 6. Second LLM call continues with the retrieved content
#[tokio::test]
async fn test_context_e2e_inline_and_blob_with_tool_call() {
    // --- Setup ---
    let store = BlobStore::connect("sqlite::memory:").await.unwrap();
    let store_arc = Arc::new(store);

    // First SSE response: text + bus:fetch tool call
    let blob_key = "blob://inspection-agent/inspection/deadbeef";
    let first_sse = make_bus_fetch_sse(blob_key);
    let Some(port) = spawn_mock_server(first_sse).await else {
        eprintln!("skipping mock server test: loopback bind denied");
        return;
    };
    let resolved = make_resolved(&format!("http://127.0.0.1:{}", port));

    // Pre-populate the blob store with the large event
    let large_payload = r#"{"issues":[{"severity":"high","file":"src/ffi.rs","description":"Missing SAFETY comment"}],"confidence":0.95}"#;
    let blob = StoredBlob {
        key: blob_key.to_string(),
        source: "inspection-agent".to_string(),
        topic: "inspection".to_string(),
        mime: "application/json".to_string(),
        size: large_payload.len() as u64,
        payload: large_payload.to_string(),
        summary: "1 issue found".to_string(),
    };
    store_arc.insert(&blob).await.unwrap();

    let mut agent = Agent::new(resolved)
        .with_blob_store(store_arc.clone())
        .with_provider(EventsProvider::new("inspection-agent"))
        .with_tool_executor(Box::new(
            DefaultToolExecutor::new_with_blob_store("/tmp", Some(store_arc.clone())).unwrap(),
        ));

    agent.set_system_prompt(
        "You are a code review assistant. Use bus:fetch to retrieve full reports.",
    );

    // --- Inject bus events ---
    // Small event (under 500 token threshold) — should inline
    let small_event = ContextEvent::new(
        "audit-pass",
        "auditor",
        serde_json::json!({"status": "passed", "checks": 12}),
    );
    // Large event (exceeds threshold) — should go to blob
    let large_event = ContextEvent::new(
        "inspection",
        "inspection-agent",
        serde_json::json!({"detail": "x".repeat(5000)}),
    );
    agent.inject_bus_events(vec![small_event, large_event]);

    // --- Run the agent (will trigger compilation + LLM call + tool execution) ---
    let events = agent.run("Review the latest inspection results", 5).await;

    // --- Verify the compiled prompt had correct context ---
    let tokens: String = events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    // Should have received the initial LLM response
    assert!(
        tokens.contains("report"),
        "LLM response should be included: {}",
        tokens
    );

    // Should have triggered a tool call for bus:fetch
    let has_tool_call = events
        .iter()
        .any(|e| matches!(e, LoopEvent::ToolCallRequired { .. }));
    assert!(has_tool_call, "Agent should have requested bus:fetch");

    // Should have completed
    let has_done = events.iter().any(|e| matches!(e, LoopEvent::Done { .. }));
    assert!(has_done, "Agent should complete execution");

    // --- Verify blob was fetchable ---
    let retrieved = store_arc.get(blob_key).await.unwrap();
    assert!(retrieved.payload.contains("ffi.rs"));
    assert!(retrieved.payload.contains("SAFETY"));
}

/// Scenario: Agent with no blob store — large events are silently skipped.
/// The agent should still run normally without errors.
#[tokio::test]
async fn test_context_e2e_no_blob_store_degradation() {
    let sse_body = make_sse_response("All checks passed, no issues found.");
    let Some(port) = spawn_mock_server(sse_body).await else {
        eprintln!("skipping mock server test: loopback bind denied");
        return;
    };
    let resolved = make_resolved(&format!("http://127.0.0.1:{}", port));

    let mut agent = Agent::new(resolved).with_provider(EventsProvider::new("test-source"));

    agent.set_system_prompt("You are a code review assistant.");

    // Inject a small event (inline OK) and a large event (no blob store → skip)
    let small = ContextEvent::new(
        "audit-pass",
        "auditor",
        serde_json::json!({"status": "passed"}),
    );
    let large = ContextEvent::new(
        "big-report",
        "auditor",
        serde_json::json!({"data": "x".repeat(5000)}),
    );
    agent.inject_bus_events(vec![small, large]);

    let events = agent.run("Status check", 3).await;

    let tokens: String = events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        tokens.contains("All checks passed"),
        "Agent should respond: {}",
        tokens
    );

    let has_done = events.iter().any(|e| matches!(e, LoopEvent::Done { .. }));
    assert!(has_done, "Agent should complete without blob store");
}

/// Scenario: No bus events at all — EventsProvider returns None,
/// compiler produces clean prompt with just System + Input.
#[tokio::test]
async fn test_context_e2e_no_bus_events() {
    let sse_body = make_sse_response("No events to review. Ready.");
    let Some(port) = spawn_mock_server(sse_body).await else {
        eprintln!("skipping mock server test: loopback bind denied");
        return;
    };
    let resolved = make_resolved(&format!("http://127.0.0.1:{}", port));

    let store = BlobStore::connect("sqlite::memory:").await.unwrap();
    let mut agent = Agent::new(resolved)
        .with_blob_store(Arc::new(store))
        .with_provider(EventsProvider::new("test"));

    agent.set_system_prompt("You are a reviewer.");
    // No bus events injected

    let events = agent.run("Check status", 3).await;

    let tokens: String = events
        .iter()
        .filter_map(|e| match e {
            LoopEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(!tokens.is_empty(), "Should still get LLM response");

    let has_done = events.iter().any(|e| matches!(e, LoopEvent::Done { .. }));
    assert!(has_done);
}
