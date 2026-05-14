use std::collections::HashMap;
use std::pin::Pin;
use std::sync::LazyLock;

use futures::{Stream, StreamExt};

use crate::catalog::ApiProtocol;
use crate::errors::LatticeError;
use crate::provider::{ChatRequest, ChatResponse};
use crate::streaming::StreamEvent;
use crate::transport::TransportDispatcher;
use crate::types::{FunctionCall, Message, ToolCall, ToolDefinition};
use crate::ResolvedModel;

static DISPATCHER: LazyLock<TransportDispatcher> = LazyLock::new(TransportDispatcher::new);

/// HTTP header names that are explicitly allowed to be injected via provider_specific.
/// All other headers are rejected to prevent sensitive header injection.
const ALLOWED_INJECTED_HEADERS: &[&str] = &[
    "x-request-id",
    "x-correlation-id",
    "x-trace-id",
    "x-custom-header",
    "content-type",
    "accept",
    "accept-language",
    "user-agent",
    "x-organization",
    "anthropic-version",
    "anthropic-beta",
    "openai-organization",
    "openai-beta",
    "x-goog-project-id",
];

pub(crate) fn validate_injected_header(header_name: &str) -> Result<(), LatticeError> {
    let lower = header_name.to_lowercase();
    if !ALLOWED_INJECTED_HEADERS.contains(&lower.as_str()) {
        return Err(LatticeError::Config {
            message: format!(
                "provider_specific header '{}' is not in the allowed header whitelist and cannot be injected",
                header_name
            ),
        });
    }
    Ok(())
}

/// Send a streaming HTTP request through the transport layer and return the
/// SSE event stream.
pub(crate) async fn send_streaming_request(
    transport: &dyn crate::transport::Transport,
    client: &reqwest::Client,
    resolved: &ResolvedModel,
    body: &serde_json::Value,
    extra_headers: &[(&str, &str)],
) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, LatticeError> {
    let base_url = resolved.base_url.trim_end_matches('/');
    let is_custom_endpoint = resolved.provider_specific.contains_key("chat_endpoint");
    let endpoint = if is_custom_endpoint {
        let custom = resolved
            .provider_specific
            .get("chat_endpoint")
            .map(|s| s.as_str())
            .unwrap();
        tracing::warn!(
            "Using custom chat_endpoint '{}' for provider '{}' - this deviates from the provider default",
            custom,
            resolved.provider
        );
        custom
    } else {
        transport.chat_endpoint()
    };
    let url = format!("{}{}", base_url, endpoint);

    if is_custom_endpoint {
        if let Ok(parsed) = url::Url::parse(&url) {
            if let Some(host) = parsed.host_str() {
                if crate::security::is_private_ip(host) {
                    return Err(LatticeError::Config {
                        message: format!(
                            "Custom chat_endpoint points to private IP '{}' in URL '{}' - rejected for SSRF protection",
                            host, url
                        ),
                    });
                }
            }
        }
    }

    let mut req = client.post(&url).json(body);
    for (name, value) in extra_headers {
        req = req.header(*name, *value);
    }

    for (key, value) in transport.extra_headers() {
        req = req.header(key.as_str(), value.as_str());
    }

    if let Some(ref api_key) = resolved.api_key {
        req = transport.apply_auth_to_request(req, api_key.as_str());
    }

    for (key, value) in &resolved.provider_specific {
        if let Some(header_name) = key.strip_prefix("header:") {
            validate_injected_header(header_name)?;
            req = req.header(header_name, value);
        }
    }

    let response = req.send().await.map_err(|e| LatticeError::Network {
        message: format!("HTTP request failed: {}", e),
        status: e.status().map(|s| s.as_u16()),
    })?;

    let status = response.status();
    if !status.is_success() {
        let retry_after_header = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let body_text = response.text().await.unwrap_or_default();
        return Err(crate::errors::ErrorClassifier::classify_with_retry_header(
            status.as_u16(),
            &body_text,
            &resolved.provider,
            retry_after_header.as_deref(),
        ));
    }

    let stream = crate::streaming::sse_from_bytes_stream(
        response.bytes_stream(),
        transport.create_sse_parser(),
    );
    Ok(Box::pin(stream))
}

/// Send messages to a resolved model and return a stream of [`StreamEvent`]s.
pub async fn chat(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, LatticeError> {
    chat_with_effort(resolved, messages, tools, None).await
}

pub async fn chat_with_effort(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
    effort_override: Option<&str>,
) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, LatticeError> {
    let (thinking, reasoning_effort) = if let Some(effort) = effort_override {
        match effort {
            "off" | "none" => (None, None),
            "low" | "medium" | "high" | "xhigh" | "max" => (
                Some(serde_json::json!({"type": "enabled"})),
                Some(effort.to_string()),
            ),
            _ => (None, None),
        }
    } else {
        match resolved.api_model_id.as_str() {
            "deepseek-v4-pro" | "deepseek-reasoner" | "deepseek/deepseek-v4-pro" => (
                Some(serde_json::json!({"type": "enabled"})),
                Some("high".to_string()),
            ),
            "deepseek-v4-flash" => (None, None),
            _ => (None, None),
        }
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
            send_streaming_request(transport, client, resolved, &body, &[]).await
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
pub async fn chat_complete(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<ChatResponse, LatticeError> {
    let mut stream = chat(resolved, messages, tools).await?;

    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls_map: HashMap<String, ToolCallBuilder> = HashMap::new();
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
            StreamEvent::ToolCallEnd { .. } => {}
            StreamEvent::Done {
                finish_reason: fr,
                usage: u,
            } => {
                finish_reason = fr;
                usage = u;
            }
            StreamEvent::Error { message: m } => {
                let has_content = !content.is_empty() || !tool_calls_map.is_empty();
                if m.contains("Stream ended") {
                    if has_content {
                        break;
                    }
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

struct ToolCallBuilder {
    name: String,
    arguments: String,
}

#[cfg(test)]
mod send_streaming_request_tests {
    use super::*;
    use crate::catalog::CredentialStatus;
    use crate::transport::Transport;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

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
            Err(crate::transport::TransportError::UnexpectedFormat(
                "denormalize_response should not be called in error path".into(),
            ))
        }

        fn chat_endpoint(&self) -> &str {
            "/chat/completions"
        }
    }

    async fn assert_streaming_error_classification(
        status_code: u16,
        body: &'static str,
        expected_variant: &str,
    ) {
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!("skipping mock server test: loopback bind denied");
                return;
            }
            Err(e) => panic!("failed to bind mock server: {e}"),
        };
        let port = listener.local_addr().unwrap().port();

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

#[cfg(test)]
mod validate_header_tests {
    use super::*;

    #[test]
    fn test_validate_injected_header_rejects_sensitive() {
        for sensitive in &[
            "authorization",
            "host",
            "cookie",
            "x-api-key",
            "x-goog-api-key",
            "set-cookie",
            "proxy-authorization",
        ] {
            let result = validate_injected_header(sensitive);
            assert!(result.is_err(), "should reject '{}'", sensitive);
        }
    }

    #[test]
    fn test_validate_injected_header_rejects_unknown() {
        for unknown in &["x-evil-header", "x-inject", "x-anything-goes"] {
            let result = validate_injected_header(unknown);
            assert!(result.is_err(), "should reject unknown '{}'", unknown);
        }
    }

    #[test]
    fn test_validate_injected_header_accepts_whitelisted() {
        for ok in &[
            "x-request-id",
            "content-type",
            "anthropic-version",
            "openai-beta",
        ] {
            let result = validate_injected_header(ok);
            assert!(result.is_ok(), "should accept '{}'", ok);
        }
    }
}
