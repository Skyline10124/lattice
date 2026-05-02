//! Integration tests for `Agent` with a mock HTTP server.
//!
//! Tests `send_message()` and `run()` against a simulated OpenAI-compatible
//! endpoint. No real API keys needed.

use std::collections::HashMap;

use lattice_agent::Agent;
use lattice_core::catalog::{ApiProtocol, CredentialStatus};
use lattice_core::ResolvedModel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ---- helpers ---------------------------------------------------------------

/// Spawn a minimal HTTP server returning OpenAI SSE data.
/// Returns the listening port.
async fn spawn_mock_server(sse_body: String) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
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

    port
}

fn make_resolved(base_url: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: "test-model".into(),
        provider: "test".into(),
        api_key: Some("sk-test".into()),
        base_url: base_url.to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "test-model".into(),
        context_length: 8192,
        provider_specific: HashMap::new(),
        credential_status: CredentialStatus::Present,
    }
}

/// Build an SSE data line for an OpenAI content chunk.
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

/// Build the final SSE data line with finish_reason=stop.
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

/// Build SSE data for a tool call.
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

fn make_sse_response(content_parts: &[&str]) -> String {
    let mut body = String::new();
    for part in content_parts {
        body.push_str(&content_chunk(part));
    }
    body.push_str(&stop_chunk());
    body.push_str("data: [DONE]\n\n");
    body
}

// ---- tests -----------------------------------------------------------------

/// Use `send_message` (async via rt.block_on).
#[test]
fn test_agent_send_message_basic() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let sse_body = make_sse_response(&["Hi there"]);
    let port = rt.block_on(spawn_mock_server(sse_body));
    let resolved = make_resolved(&format!("http://127.0.0.1:{port}"));

    let mut agent = Agent::new(resolved);
    let events = rt.block_on(agent.send_message("Hello"));

    let tokens: String = events
        .iter()
        .filter_map(|e| match e {
            lattice_agent::LoopEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(tokens, "Hi there", "Should receive the streamed token");

    let has_done = events
        .iter()
        .any(|e| matches!(e, lattice_agent::LoopEvent::Done { .. }));
    assert!(has_done, "Should receive Done event");
}

/// Test `run()` (the method with tool-loop support).
#[test]
fn test_agent_run_basic() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let sse_body = make_sse_response(&["Hello from run"]);
    let port = rt.block_on(spawn_mock_server(sse_body));
    let resolved = make_resolved(&format!("http://127.0.0.1:{port}"));

    let mut agent = Agent::new(resolved);
    let events = rt.block_on(agent.run("Test run", 3));

    let tokens: String = events
        .iter()
        .filter_map(|e| match e {
            lattice_agent::LoopEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(tokens, "Hello from run", "Should receive tokens from run()");

    let has_done = events
        .iter()
        .any(|e| matches!(e, lattice_agent::LoopEvent::Done { .. }));
    assert!(has_done, "run() should produce Done event");
}

/// Test that `run()` handles a tool call response without a tool executor.
/// Without an executor, Agent stops after one turn (doesn't spin).
#[test]
fn test_agent_run_with_tool_call_no_executor() {
    let mut sse_body = String::new();
    sse_body.push_str(&tool_call_start_chunk("call_1", "get_weather"));
    sse_body.push_str(&tool_call_args_chunk(r#"{"loc":"NYC"}"#));
    sse_body.push_str(&tool_call_done_chunk());
    sse_body.push_str("data: [DONE]\n\n");

    let rt = tokio::runtime::Runtime::new().unwrap();
    let port = rt.block_on(spawn_mock_server(sse_body));
    let resolved = make_resolved(&format!("http://127.0.0.1:{port}"));

    let mut agent = Agent::new(resolved);
    let events = rt.block_on(agent.run("What's the weather?", 5));

    let has_tool_call = events
        .iter()
        .any(|e| matches!(e, lattice_agent::LoopEvent::ToolCallRequired { .. }));
    assert!(
        has_tool_call,
        "Should have ToolCallRequired when model requests tools"
    );

    // Without a tool executor, Agent stops after detecting tool calls.
    let has_done = events
        .iter()
        .any(|e| matches!(e, lattice_agent::LoopEvent::Done { .. }));
    assert!(has_done, "Should eventually produce Done event");
}

/// Test `send_message` (the async method).
#[tokio::test]
async fn test_agent_send_message_async_basic() {
    let sse_body = make_sse_response(&["Async hello"]);
    let port = spawn_mock_server(sse_body).await;
    let resolved = make_resolved(&format!("http://127.0.0.1:{port}"));

    let mut agent = Agent::new(resolved);
    let events = agent.send_message("Hello").await;

    let tokens: String = events
        .iter()
        .filter_map(|e| match e {
            lattice_agent::LoopEvent::Token { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(tokens, "Async hello");
    let has_done = events
        .iter()
        .any(|e| matches!(e, lattice_agent::LoopEvent::Done { .. }));
    assert!(has_done);
}
