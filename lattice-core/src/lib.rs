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
pub use catalog::ResolvedModel;
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
            let transport = &DISPATCHER
                .dispatch(&ApiProtocol::OpenAiChat)
                .ok_or_else(|| LatticeError::Config {
                    message: "OpenAiChat transport not registered".into(),
                })?;

            let mut body =
                transport
                    .normalize_request(&request)
                    .map_err(|e| LatticeError::Streaming {
                        message: e.to_string(),
                    })?;
            body["stream"] = serde_json::Value::Bool(true);

            let base_url = resolved.base_url.trim_end_matches('/');
            let endpoint = resolved
                .provider_specific
                .get("chat_endpoint")
                .map(|s| s.as_str())
                .unwrap_or_else(|| transport.chat_endpoint());
            let url = format!("{}{}", base_url, endpoint);

            let mut req = client.post(&url).json(&body);
            if let Some(ref api_key) = resolved.api_key {
                match resolved
                    .provider_specific
                    .get("auth_type")
                    .map(|s| s.as_str())
                {
                    Some("x-goog-api-key") => {
                        req = req.header("x-goog-api-key", api_key.as_str());
                    }
                    _ => {
                        req = req.header(
                            transport.auth_header_name(),
                            transport.auth_header_value(api_key),
                        );
                    }
                }
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
                return Err(LatticeError::ProviderUnavailable {
                    provider: resolved.provider.clone(),
                    reason: format!("HTTP {}: {}", status.as_u16(), body_text),
                });
            }

            let stream = crate::streaming::sse_from_bytes_stream(
                response.bytes_stream(),
                transport.create_sse_parser(),
            );
            Ok(Box::pin(stream))
        }

        ApiProtocol::AnthropicMessages => {
            let transport = &DISPATCHER
                .dispatch(&ApiProtocol::AnthropicMessages)
                .ok_or_else(|| LatticeError::Config {
                    message: "AnthropicMessages transport not registered".into(),
                })?;

            let mut body =
                transport
                    .normalize_request(&request)
                    .map_err(|e| LatticeError::Streaming {
                        message: e.to_string(),
                    })?;
            body["stream"] = serde_json::Value::Bool(true);

            let base_url = resolved.base_url.trim_end_matches('/');
            let endpoint = resolved
                .provider_specific
                .get("chat_endpoint")
                .map(|s| s.as_str())
                .unwrap_or_else(|| transport.chat_endpoint());
            let url = format!("{}{}", base_url, endpoint);

            let mut req = client
                .post(&url)
                .header("anthropic-version", "2023-06-01")
                .json(&body);
            if let Some(ref api_key) = resolved.api_key {
                req = req.header(
                    transport.auth_header_name(),
                    transport.auth_header_value(api_key),
                );
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
                return Err(LatticeError::ProviderUnavailable {
                    provider: resolved.provider.clone(),
                    reason: format!("HTTP {}: {}", status.as_u16(), body_text),
                });
            }

            let stream = crate::streaming::sse_from_bytes_stream(
                response.bytes_stream(),
                transport.create_sse_parser(),
            );
            Ok(Box::pin(stream))
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
