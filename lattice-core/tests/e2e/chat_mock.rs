//! Integration test: `chat()` with a mock HTTP server.
//!
//! Tests the public resolve→chat→stream pipeline using a simulated
//! OpenAI-compatible endpoint. No real API keys or network calls needed.
//!
//! The mock server responds with valid OpenAI SSE events, and we verify
//! the stream is parsed correctly into tokens.

use std::collections::HashMap;

use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use lattice_core::catalog::{ApiProtocol, CredentialStatus};
use lattice_core::ResolvedModel;

/// Spawn a minimal HTTP server that returns OpenAI-formatted SSE data.
/// Returns the port it is listening on.
async fn spawn_mock_openai_server(body_sse: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        // Read the HTTP request (we don't care about its contents).
        let _ = stream.read(&mut buf).await;

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
            body_sse.len(),
            body_sse
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    port
}

fn make_resolved_model(base_url: &str) -> ResolvedModel {
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

fn make_user_message(content: &str) -> lattice_core::Message {
    lattice_core::Message {
        role: lattice_core::Role::User,
        content: content.to_string(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

#[tokio::test]
async fn test_chat_basic_streaming() {
    let sse_body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let port = spawn_mock_openai_server(sse_body).await;
    let resolved = make_resolved_model(&format!("http://127.0.0.1:{port}"));

    let messages = vec![make_user_message("Hello")];
    let mut stream = lattice_core::chat(&resolved, &messages, &[])
        .await
        .expect("chat() should succeed");

    let mut tokens = Vec::new();
    let mut done = false;

    while let Some(event) = stream.next().await {
        match event {
            lattice_core::StreamEvent::Token { content } => tokens.push(content),
            lattice_core::StreamEvent::Done { .. } => {
                done = true;
                break;
            }
            _ => {}
        }
    }

    assert!(done, "Should receive Done event");
    assert_eq!(tokens, vec!["Hello", " world"]);
}

#[tokio::test]
async fn test_chat_empty_stream_yields_no_tokens() {
    let sse_body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let port = spawn_mock_openai_server(sse_body).await;
    let resolved = make_resolved_model(&format!("http://127.0.0.1:{port}"));

    let messages = vec![make_user_message("test")];
    let mut stream = lattice_core::chat(&resolved, &messages, &[])
        .await
        .expect("chat() should succeed");

    let mut tokens = Vec::new();
    let mut done = false;

    while let Some(event) = stream.next().await {
        match event {
            lattice_core::StreamEvent::Token { content } => tokens.push(content),
            lattice_core::StreamEvent::Done { .. } => done = true,
            _ => {}
        }
    }

    assert!(done, "Should receive Done event");
    assert!(tokens.is_empty(), "Should have no tokens");
}

#[tokio::test]
async fn test_chat_with_tool_call_streaming() {
    // Build the SSE body piecewise to avoid deeply nested escape sequences.
    let tool_call_args = "{\"location\":\"NYC\"}";
    let escaped_args = serde_json::Value::String(tool_call_args.to_string());
    let inner_json = serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "function": { "arguments": escaped_args }
                }]
            },
            "finish_reason": null
        }]
    });

    // The final Done chunk
    let done_chunk = serde_json::json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
    });

    let sse_body = format!(
        "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"role\":\"assistant\",\"content\":null,\"tool_calls\":[{{\"index\":0,\"id\":\"call_abc\",\"type\":\"function\",\"function\":{{\"name\":\"get_weather\",\"arguments\":\"\"}}}}]}},\"finish_reason\":null}}]}}\n\n\
         data: {}\n\n\
         data: {}\n\n\
         data: [DONE]\n\n",
        inner_json,
        done_chunk,
    );

    let port = spawn_mock_openai_server(Box::leak(sse_body.into_boxed_str())).await;
    let resolved = make_resolved_model(&format!("http://127.0.0.1:{port}"));

    let messages = vec![make_user_message("What's the weather?")];
    let mut stream = lattice_core::chat(&resolved, &messages, &[])
        .await
        .expect("chat() should succeed");

    let mut tool_calls = Vec::new();
    let mut done = false;

    while let Some(event) = stream.next().await {
        match event {
            lattice_core::StreamEvent::ToolCallStart { id, name } => {
                tool_calls.push((id, name, String::new()));
            }
            lattice_core::StreamEvent::ToolCallDelta {
                id,
                arguments_delta,
            } => {
                if let Some(tc) = tool_calls.iter_mut().find(|(i, _, _)| *i == id) {
                    tc.2.push_str(&arguments_delta);
                }
            }
            lattice_core::StreamEvent::Done { .. } => done = true,
            _ => {}
        }
    }

    assert!(done, "Should receive Done event");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].0, "call_abc");
    assert_eq!(tool_calls[0].1, "get_weather");
    assert!(
        tool_calls[0].2.contains("NYC"),
        "arguments should contain NYC, got: {}",
        tool_calls[0].2
    );
}

#[tokio::test]
async fn test_chat_http_error_classification() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf).await;

        let body = r#"{"error": {"message": "Invalid API key"}}"#;
        let response = format!(
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    });

    let resolved = make_resolved_model(&format!("http://127.0.0.1:{port}"));
    let messages = vec![make_user_message("test")];
    let result = lattice_core::chat(&resolved, &messages, &[]).await;

    match result {
        Err(lattice_core::LatticeError::Authentication { provider, .. }) => {
            assert_eq!(provider, "test");
        }
        Err(other) => panic!("Expected Authentication error, got: {other:?}"),
        Ok(_) => panic!("Expected error, got Ok"),
    }
}
