pub mod catalog;
pub mod errors;
pub mod logging;
pub mod provider;
pub mod retry;
pub mod router;
pub mod streaming;
pub mod tokens;
pub mod transport;
pub mod types;

// Re-export key types for convenience
pub use catalog::{CredentialStatus, ResolvedModel};
pub use errors::LatticeError;
pub use logging::{init_debug_logging, init_logging};
pub use streaming::StreamEvent;
pub use types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

use router::ModelRouter;

/// Resolve a model name (or alias, e.g. "sonnet") to provider connection details.
///
/// This is a stateless convenience — each call creates a fresh router.
/// For custom model registrations, use [`ModelRouter`] directly.
///
/// Credentials are resolved from environment variables.
pub fn resolve(model: &str) -> Result<ResolvedModel, LatticeError> {
    ModelRouter::new().resolve(model, None)
}

// ---------------------------------------------------------------------------
// chat() — streaming chat
// ---------------------------------------------------------------------------

use std::pin::Pin;
use std::sync::LazyLock;

use futures::{Stream, StreamExt};

use crate::catalog::ApiProtocol;
use crate::provider::{ChatRequest, ChatResponse};
use crate::transport::TransportDispatcher;

static DISPATCHER: LazyLock<TransportDispatcher> = LazyLock::new(TransportDispatcher::new);

/// Send a streaming HTTP request through the transport layer and return the
/// SSE event stream.
///
/// Unifies the URL construction, auth/header setup, HTTP send, error mapping,
/// and SSE stream creation that was previously duplicated across protocol
/// branches in [`chat()`].
async fn send_streaming_request(
    transport: &dyn crate::transport::Transport,
    client: &reqwest::Client,
    resolved: &ResolvedModel,
    body: &serde_json::Value,
    extra_headers: &[(&str, &str)],
) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, LatticeError> {
    let base_url = resolved.base_url.trim_end_matches('/');
    let endpoint = resolved
        .provider_specific
        .get("chat_endpoint")
        .map(|s| s.as_str())
        .unwrap_or_else(|| transport.chat_endpoint());
    let url = format!("{}{}", base_url, endpoint);

    let mut req = client.post(&url).json(body);
    for (name, value) in extra_headers {
        req = req.header(*name, *value);
    }

    if let Some(ref api_key) = resolved.api_key {
        req = transport.apply_auth_to_request(req, api_key.as_str());
    }

    for (key, value) in &resolved.provider_specific {
        if let Some(header_name) = key.strip_prefix("header:") {
            req = req.header(header_name, value);
        }
    }

    let response = req.send().await.map_err(|e| LatticeError::Network {
        message: format!("HTTP request failed: {}", e),
        status: e.status().map(|s| s.as_u16()),
    })?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(crate::errors::ErrorClassifier::classify(
            status.as_u16(),
            &body_text,
            &resolved.provider,
        ));
    }

    let stream = crate::streaming::sse_from_bytes_stream(
        response.bytes_stream(),
        transport.create_sse_parser(),
    );
    Ok(Box::pin(stream))
}

/// Send messages to a resolved model and return a stream of [`StreamEvent`]s.
///
/// This function handles the full HTTP+SSE pipeline:
/// 1. Normalizes the request using the protocol-appropriate transport
/// 2. POSTs to the provider's streaming endpoint with auth
/// 3. Parses the SSE stream into [`StreamEvent`]s
///
/// # Example
///
/// ```ignore
/// let resolved = lattice_core::resolve("gpt-4o")?;
/// let messages = vec![Message::new(Role::User, "Hello!".into(), None, None, None)];
/// let stream = lattice_core::chat(&resolved, &messages).await?;
/// ```
pub async fn chat(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, LatticeError> {
    // Auto-configure DeepSeek thinking mode based on model name.
    let (thinking, reasoning_effort) = match resolved.api_model_id.as_str() {
        "deepseek-v4-pro" | "deepseek-reasoner" | "deepseek/deepseek-v4-pro" => (
            Some(serde_json::json!({"type": "enabled"})),
            Some("high".to_string()),
        ),
        "deepseek-v4-flash" => (None, None), // thinking OFF for flash
        _ => (None, None),                   // other providers don't use these params
    };

    let request = ChatRequest {
        messages: messages.to_vec(),
        tools: tools.to_vec(),
        model: resolved.api_model_id.clone(),
        temperature: None,
        max_tokens: None,
        stream: true,
        resolved: resolved.clone(),
        thinking,
        reasoning_effort,
    };

    let client = crate::provider::shared_http_client();

    match &resolved.api_protocol {
        ApiProtocol::OpenAiChat => {
            let transport = DISPATCHER
                .dispatch(&ApiProtocol::OpenAiChat)
                .ok_or_else(|| LatticeError::Config {
                    message: "OpenAiChat transport not registered".into(),
                })?;

            let body =
                transport
                    .normalize_request(&request)
                    .map_err(|e| LatticeError::Streaming {
                        message: e.to_string(),
                    })?;

            send_streaming_request(transport, client, resolved, &body, &[]).await
        }

        ApiProtocol::AnthropicMessages => {
            let transport = DISPATCHER
                .dispatch(&ApiProtocol::AnthropicMessages)
                .ok_or_else(|| LatticeError::Config {
                    message: "AnthropicMessages transport not registered".into(),
                })?;

            let body =
                transport
                    .normalize_request(&request)
                    .map_err(|e| LatticeError::Streaming {
                        message: e.to_string(),
                    })?;

            send_streaming_request(
                transport,
                client,
                resolved,
                &body,
                &[("anthropic-version", "2023-06-01")],
            )
            .await
        }

        ApiProtocol::GeminiGenerateContent => {
            let transport = DISPATCHER
                .dispatch(&ApiProtocol::GeminiGenerateContent)
                .ok_or_else(|| LatticeError::Config {
                    message: "GeminiGenerateContent transport not registered".into(),
                })?;

            let body =
                transport
                    .normalize_request(&request)
                    .map_err(|e| LatticeError::Streaming {
                        message: e.to_string(),
                    })?;

            crate::transport::gemini::send_gemini_nonstreaming_request(
                transport, client, resolved, &body,
            )
            .await
        }

        _ => Err(LatticeError::Config {
            message: format!(
                "Streaming not yet supported for protocol {:?}",
                resolved.api_protocol
            ),
        }),
    }
}

/// Send messages to a resolved model and collect the full [`ChatResponse`].
///
/// Internally calls [`chat()`] and accumulates the stream events into
/// a complete response with content, tool calls, usage, and finish reason.
///
/// # Example
///
/// ```ignore
/// let resolved = lattice_core::resolve("gpt-4o")?;
/// let messages = vec![Message::new(Role::User, "Hello!".into(), None, None, None)];
/// let response = lattice_core::chat_complete(&resolved, &messages).await?;
/// println!("{:?}", response.content);
/// ```
pub async fn chat_complete(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<ChatResponse, LatticeError> {
    let mut stream = chat(resolved, messages, tools).await?;

    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls_map: std::collections::HashMap<String, ToolCallBuilder> =
        std::collections::HashMap::new();
    // Default finish_reason when no Done event is received before stream ends.
    // If a Done event arrives (see below), finish_reason is overwritten with the
    // provider-reported value.
    let mut finish_reason = String::from("unknown");
    let mut usage = None;

    while let Some(event) = stream.next().await {
        match event {
            StreamEvent::Token { content: c } => {
                content.push_str(&c);
            }
            StreamEvent::Reasoning { content: r } => {
                reasoning_content.push_str(&r);
            }
            StreamEvent::ToolCallStart { id, name } => {
                tool_calls_map.insert(
                    id,
                    ToolCallBuilder {
                        name,
                        arguments: String::new(),
                    },
                );
            }
            StreamEvent::ToolCallDelta {
                id,
                arguments_delta,
            } => {
                if let Some(tc) = tool_calls_map.get_mut(&id) {
                    tc.arguments.push_str(&arguments_delta);
                }
            }
            StreamEvent::ToolCallEnd { .. } => {
                // Tool call argument stream ends; already accumulated.
            }
            StreamEvent::Done {
                finish_reason: fr,
                usage: u,
            } => {
                finish_reason = fr;
                usage = u;
            }
            StreamEvent::Error { message: m } => {
                // If we already received content or tool calls, a stream error
                // is non-fatal — return the partial response. The caller can
                // decide whether to retry based on what was received.
                let has_content = !content.is_empty() || !tool_calls_map.is_empty();

                // "Stream ended" is a normal SSE close — always non-fatal.
                if m.contains("Stream ended") {
                    if has_content {
                        break;
                    }
                    // Empty stream ended: the provider accepted the request but
                    // sent nothing useful. Classify as transient.
                    return Err(LatticeError::ProviderUnavailable {
                        provider: resolved.provider.clone(),
                        reason: m,
                    });
                }

                if has_content {
                    if finish_reason == "unknown" {
                        finish_reason = String::from("stream_lost");
                    }
                    break;
                }

                // No content and not a normal stream close — propagate as typed error.
                // Classify common transport errors so the Agent can retry appropriately.
                if m.contains("error sending request")
                    || m.contains("connection")
                    || m.contains("timeout")
                    || m.contains("reset")
                {
                    return Err(LatticeError::ProviderUnavailable {
                        provider: resolved.provider.clone(),
                        reason: m,
                    });
                }

                return Err(LatticeError::Streaming { message: m });
            }
        }
    }

    let tool_calls = if tool_calls_map.is_empty() {
        None
    } else {
        Some(
            tool_calls_map
                .into_iter()
                .map(|(id, tc)| ToolCall {
                    id,
                    function: FunctionCall {
                        name: tc.name,
                        arguments: tc.arguments,
                    },
                })
                .collect(),
        )
    };

    Ok(ChatResponse {
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        reasoning_content: if reasoning_content.is_empty() {
            None
        } else {
            Some(reasoning_content)
        },
        tool_calls,
        usage,
        finish_reason,
        model: resolved.api_model_id.clone(),
    })
}

/// Internal helper for building tool calls during stream collection.
struct ToolCallBuilder {
    name: String,
    arguments: String,
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use crate::router::test_support::{restore_all, save_and_clear_all, ENV_MUTEX};

    #[test]
    fn test_resolve_sonnet_alias_missing_credential() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();
        let result = resolve("sonnet");
        match result {
            Ok(r) => panic!(
                "unexpected Ok: provider={}, api_key={:?}",
                r.provider, r.api_key
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("API_KEY") || msg.contains("requires"),
                    "error should mention missing credential, got: {}",
                    msg
                );
            }
        }
        restore_all(&saved);
    }

    #[test]
    fn test_resolve_sonnet_alias_with_credential() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();
        std::env::set_var("ANTHROPIC_API_KEY", "sk-test");
        let result = resolve("sonnet");
        assert!(result.is_ok());
        if let Ok(r) = result {
            assert_eq!(r.canonical_id, "claude-sonnet-4-6");
        }
        restore_all(&saved);
    }

    #[test]
    fn resolve_gpt4o_missing_credential_errors() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();
        let result = resolve("gpt-4o");
        match result {
            Ok(r) => panic!(
                "unexpected Ok: provider={}, api_key={:?}",
                r.provider, r.api_key
            ),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("API_KEY") || msg.contains("requires"),
                    "error should mention missing credential, got: {}",
                    msg
                );
            }
        }
        restore_all(&saved);
    }

    #[test]
    fn test_resolve_gpt4o_with_key_ok() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();
        std::env::set_var("OPENAI_API_KEY", "sk-test");
        let result = resolve("gpt-4o");
        assert!(result.is_ok());
        if let Ok(r) = result {
            assert_eq!(r.api_protocol, catalog::ApiProtocol::OpenAiChat);
        }
        restore_all(&saved);
    }

    #[test]
    fn test_resolve_nonexistent_model() {
        let result = resolve("nonexistent-model-xyz-12345");
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod send_streaming_request_tests {
    use super::*;
    use crate::catalog::CredentialStatus;
    use crate::transport::Transport;
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Minimal mock transport for testing error classification.
    struct MockTransport {
        base_url: String,
    }

    impl Transport for MockTransport {
        fn base_url(&self) -> &str {
            &self.base_url
        }

        fn extra_headers(&self) -> &HashMap<String, String> {
            static EMPTY: std::sync::LazyLock<HashMap<String, String>> =
                std::sync::LazyLock::new(HashMap::new);
            &EMPTY
        }

        fn api_mode(&self) -> &str {
            "mock"
        }

        fn normalize_request(
            &self,
            _request: &ChatRequest,
        ) -> Result<serde_json::Value, crate::transport::TransportError> {
            Ok(serde_json::json!({"test": true}))
        }

        fn denormalize_response(
            &self,
            _response: &serde_json::Value,
        ) -> Result<ChatResponse, crate::transport::TransportError> {
            unimplemented!("denormalize_response should not be called in error path")
        }

        fn chat_endpoint(&self) -> &str {
            "/chat/completions"
        }
    }

    /// Helper: spawn a minimal TCP server, call send_streaming_request,
    /// and verify the returned error variant matches expectations.
    async fn assert_streaming_error_classification(
        status_code: u16,
        body: &'static str,
        expected_variant: &str,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let port = addr.port();

        let body_bytes = body.as_bytes();
        let reason_phrase = match status_code {
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            _ => "Error",
        };
        let response_bytes = format!(
            "HTTP/1.1 {status_code} {reason_phrase}\r\nContent-Length: {}\r\n\r\n",
            body_bytes.len()
        )
        .into_bytes()
        .into_iter()
        .chain(body_bytes.iter().copied())
        .collect::<Vec<_>>();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            // Read the HTTP request before responding
            let _ = stream.read(&mut buf).await;
            stream.write_all(&response_bytes).await.unwrap();
        });

        let transport = MockTransport {
            base_url: format!("http://127.0.0.1:{port}"),
        };
        let client = reqwest::Client::new();

        let resolved = ResolvedModel {
            canonical_id: "test-model".into(),
            provider: "test-provider".into(),
            api_key: Some("sk-test".into()),
            base_url: format!("http://127.0.0.1:{port}"),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test-model".into(),
            context_length: 0,
            provider_specific: HashMap::new(),
            credential_status: CredentialStatus::Present,
        };

        let result = send_streaming_request(
            &transport,
            &client,
            &resolved,
            &serde_json::json!({"model": "test"}),
            &[],
        )
        .await;

        match result {
            Err(err) => {
                let variant = lattice_error_variant_name(&err);
                assert_eq!(
                    variant, expected_variant,
                    "For status {status_code}: expected {expected_variant}, got {variant}: {err:?}"
                );
            }
            Ok(_) => panic!("Expected Err for status {status_code}, got Ok"),
        }
    }

    fn lattice_error_variant_name(err: &LatticeError) -> &'static str {
        match err {
            LatticeError::RateLimit { .. } => "RateLimit",
            LatticeError::Authentication { .. } => "Authentication",
            LatticeError::ModelNotFound { .. } => "ModelNotFound",
            LatticeError::ProviderUnavailable { .. } => "ProviderUnavailable",
            LatticeError::ContextWindowExceeded { .. } => "ContextWindowExceeded",
            LatticeError::ToolExecution { .. } => "ToolExecution",
            LatticeError::Streaming { .. } => "Streaming",
            LatticeError::Config { .. } => "Config",
            LatticeError::Network { .. } => "Network",
        }
    }

    #[tokio::test]
    async fn test_streaming_error_429_rate_limit() {
        assert_streaming_error_classification(429, r#"{"error": "rate limit"}"#, "RateLimit").await;
    }

    #[tokio::test]
    async fn test_streaming_error_401_authentication() {
        assert_streaming_error_classification(401, "unauthorized", "Authentication").await;
    }

    #[tokio::test]
    async fn test_streaming_error_403_authentication() {
        assert_streaming_error_classification(403, "forbidden", "Authentication").await;
    }

    #[tokio::test]
    async fn test_streaming_error_404_model_not_found() {
        assert_streaming_error_classification(404, r#"{"model": "gpt-5"}"#, "ModelNotFound").await;
    }

    #[tokio::test]
    async fn test_streaming_error_500_provider_unavailable() {
        assert_streaming_error_classification(500, "internal error", "ProviderUnavailable").await;
    }

    #[tokio::test]
    async fn test_streaming_error_503_provider_unavailable() {
        assert_streaming_error_classification(503, "service overloaded", "ProviderUnavailable")
            .await;
    }

    #[tokio::test]
    async fn test_streaming_error_418_network() {
        assert_streaming_error_classification(418, "teapot", "Network").await;
    }

    #[tokio::test]
    async fn test_streaming_error_400_context_window_exceeded() {
        assert_streaming_error_classification(
            400,
            r#"{"error": {"code": "context_length_exceeded"}}"#,
            "ContextWindowExceeded",
        )
        .await;
    }
}
