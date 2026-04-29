pub mod catalog;
pub mod errors;
pub mod provider;
pub mod retry;
pub mod router;
pub mod streaming;
pub mod tokens;
pub mod transport;
pub mod types;

// Re-export key types for convenience
pub use catalog::ResolvedModel;
pub use errors::ArtemisError;
pub use streaming::StreamEvent;
pub use types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

use router::ModelRouter;

/// Resolve a model name (or alias, e.g. "sonnet") to provider connection details.
///
/// This is a stateless convenience — each call creates a fresh router.
/// For custom model registrations, use [`ModelRouter`] directly.
///
/// Credentials are resolved from environment variables.
pub fn resolve(model: &str) -> Result<ResolvedModel, ArtemisError> {
    ModelRouter::new().resolve(model, None)
}

// ---------------------------------------------------------------------------
// chat() — streaming chat
// ---------------------------------------------------------------------------

use std::pin::Pin;
use std::sync::LazyLock;

use futures::{Stream, StreamExt};
use reqwest_eventsource::RequestBuilderExt;

use crate::catalog::ApiProtocol;
use crate::provider::{ChatRequest, ChatResponse};
use crate::streaming::EventStream;
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
/// let resolved = artemis_core::resolve("gpt-4o")?;
/// let messages = vec![Message::new(Role::User, "Hello!".into(), None, None, None)];
/// let stream = artemis_core::chat(&resolved, &messages).await?;
/// ```
pub async fn chat(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, ArtemisError> {
    // Auto-configure DeepSeek thinking mode based on model name.
    let (thinking, reasoning_effort) = match resolved.api_model_id.as_str() {
        "deepseek-v4-pro" | "deepseek-reasoner" | "deepseek/deepseek-v4-pro" => (
            Some(serde_json::json!({"type": "enabled"})),
            Some("high".to_string()),
        ),
        "deepseek-v4-flash" => (None, None), // thinking OFF for flash
        _ => (None, None), // other providers don't use these params
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
                .ok_or_else(|| ArtemisError::Config {
                    message: "OpenAiChat transport not registered".into(),
                })?;

            let mut body =
                transport
                    .normalize_request(&request)
                    .map_err(|e| ArtemisError::Streaming {
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
                req = req.header(
                    transport.auth_header_name(),
                    transport.auth_header_value(api_key),
                );
            }

            let event_source = req.eventsource().map_err(|e| ArtemisError::Network {
                message: format!("Failed to create event source: {}", e),
                status: None,
            })?;

            let stream = EventStream::new(event_source, transport.create_sse_parser());
            Ok(Box::pin(stream))
        }

        ApiProtocol::AnthropicMessages => {
            let transport = &DISPATCHER
                .dispatch(&ApiProtocol::AnthropicMessages)
                .ok_or_else(|| ArtemisError::Config {
                    message: "AnthropicMessages transport not registered".into(),
                })?;

            let body =
                transport
                    .normalize_request(&request)
                    .map_err(|e| ArtemisError::Streaming {
                        message: e.to_string(),
                    })?;

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

            let event_source = req.eventsource().map_err(|e| ArtemisError::Network {
                message: format!("Failed to create event source: {}", e),
                status: None,
            })?;

            let stream = EventStream::new(event_source, transport.create_sse_parser());
            Ok(Box::pin(stream))
        }

        _ => Err(ArtemisError::Config {
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
/// let resolved = artemis_core::resolve("gpt-4o")?;
/// let messages = vec![Message::new(Role::User, "Hello!".into(), None, None, None)];
/// let response = artemis_core::chat_complete(&resolved, &messages).await?;
/// println!("{:?}", response.content);
/// ```
pub async fn chat_complete(
    resolved: &ResolvedModel,
    messages: &[Message],
    tools: &[ToolDefinition],
) -> Result<ChatResponse, ArtemisError> {
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
                // "Stream ended" is a normal SSE connection close after all events
                // have been delivered. If we received content or tool calls, treat
                // it as a successful stream end. finish_reason was already set by
                // any prior Done event, or remains "unknown" if no Done was received.
                if m.contains("Stream ended") && (!content.is_empty() || !tool_calls_map.is_empty()) {
                    break;
                }
                return Err(ArtemisError::Streaming { message: m });
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

    #[test]
    fn test_resolve_sonnet_alias() {
        let result = resolve("sonnet");
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.canonical_id, "claude-sonnet-4-6");
    }

    #[test]
    fn test_resolve_gpt4o() {
        let result = resolve("gpt-4o");
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.api_protocol, catalog::ApiProtocol::OpenAiChat);
    }

    #[test]
    fn test_resolve_nonexistent_model() {
        let result = resolve("nonexistent-model-xyz-12345");
        assert!(result.is_err());
    }
}
