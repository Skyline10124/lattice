//! DeepSeek provider adapter — implements the [`Provider`] trait using the
//! DeepSeek Chat Completions API.
//!
//! Uses [`ChatCompletionsTransport`] for format normalization/denormalization
//! and makes direct HTTP calls via `reqwest`.

use async_trait::async_trait;

use crate::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use crate::streaming::EventStream;
use crate::transport::chat_completions::{ChatCompletionsTransport, Transport};

/// Provider adapter for the DeepSeek Chat Completions API.
///
/// Handles HTTP communication with the DeepSeek API, using
/// [`ChatCompletionsTransport`] for message/tool/response format conversion.
pub struct DeepSeekProvider {
    transport: ChatCompletionsTransport,
}

impl DeepSeekProvider {
    /// Create a new DeepSeekProvider with a default transport.
    pub fn new() -> Self {
        DeepSeekProvider {
            transport: ChatCompletionsTransport::new(),
        }
    }

    /// Create a DeepSeekProvider with a custom transport (e.g. for proxied endpoints).
    pub fn with_transport(transport: ChatCompletionsTransport) -> Self {
        DeepSeekProvider { transport }
    }
}

impl Default for DeepSeekProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for DeepSeekProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let resolved = &request.resolved;
        let base_url = if resolved.base_url.is_empty() {
            "https://api.deepseek.com/v1".to_string()
        } else {
            resolved.base_url.clone()
        };

        // Build the request body using the transport's normalize_request.
        let mut body = self
            .transport
            .normalize_request(&request)
            .map_err(|e| ProviderError::General(e.to_string()))?;

        // Ensure stream is explicitly false for non-streaming chat.
        body["stream"] = serde_json::Value::Bool(false);

        let client = crate::provider::shared_http_client();
        let mut req = client
            .post(format!("{}/chat/completions", base_url))
            .json(&body);

        if let Some(ref api_key) = resolved.api_key {
            req = req.header("Authorization", format!("Bearer {}", api_key));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| ProviderError::General(format!("HTTP request failed: {}", e)))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::General(format!("Failed to read response body: {}", e)))?;

        if !status.is_success() {
            return Err(ProviderError::Api(text));
        }

        let json: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::General(format!("Failed to parse response JSON: {}", e)))?;

        let response = self
            .transport
            .denormalize_response(&json)
            .map_err(|e| ProviderError::General(e.to_string()))?;

        Ok(response)
    }

    async fn chat_stream(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Stream(
            "SSE streaming not yet implemented for DeepSeek provider".into(),
        ))
    }

    fn name(&self) -> &str {
        "deepseek"
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ApiProtocol;
    use crate::types::{Message, Role};
    use std::collections::HashMap;

    fn make_resolved(model_id: &str) -> crate::catalog::ResolvedModel {
        crate::catalog::ResolvedModel {
            canonical_id: model_id.to_string(),
            provider: "deepseek".to_string(),
            api_key: Some("sk-test".to_string()),
            base_url: "https://api.deepseek.com/v1".to_string(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: model_id.to_string(),
            context_length: 128000,
            provider_specific: HashMap::new(),
        }
    }

    fn make_simple_request() -> ChatRequest {
        let messages = vec![
            Message {
                role: Role::System,
                content: "You are helpful.".to_string(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: "Hello!".to_string(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];
        let resolved = make_resolved("deepseek-chat");
        ChatRequest {
            messages,
            tools: vec![],
            model: "deepseek-chat".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(1024),
            stream: false,
            resolved,
        }
    }

    // ── Basic trait implementation tests ──────────────────────────────

    #[test]
    fn test_deepseek_provider_name() {
        let p = DeepSeekProvider::new();
        assert_eq!(p.name(), "deepseek");
        assert!(p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn test_default_implementation() {
        let p = DeepSeekProvider::default();
        assert_eq!(p.name(), "deepseek");
    }

    #[test]
    fn test_with_custom_transport() {
        let transport = ChatCompletionsTransport::with_base_url("http://localhost:8080/v1");
        let p = DeepSeekProvider::with_transport(transport);
        assert_eq!(p.name(), "deepseek");
    }

    // ── Transport integration tests (no network) ──────────────────────

    #[test]
    fn test_normalize_request_through_transport() {
        let p = DeepSeekProvider::new();
        let request = make_simple_request();
        let body = p.transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "deepseek-chat");
        assert_eq!(body["temperature"], 0.7);
        assert_eq!(body["max_tokens"], 1024);

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello!");
    }

    #[test]
    fn test_denormalize_response_through_transport() {
        let p = DeepSeekProvider::new();
        let response = serde_json::json!({
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello! How can I help you?"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 8,
                "total_tokens": 18
            }
        });

        let result = p.transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("Hello! How can I help you?")
        );
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
        assert_eq!(result.model, "deepseek-chat");

        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.total_tokens, 18);
    }

    // ── Request building from resolved config ─────────────────────────

    #[test]
    fn test_request_uses_resolved_api_model_id() {
        let request = make_simple_request();
        assert_eq!(request.resolved.api_model_id, "deepseek-chat");
        assert_eq!(request.model, "deepseek-chat");
    }

    #[test]
    fn test_request_uses_resolved_base_url() {
        let resolved = make_resolved("deepseek-chat");
        assert_eq!(resolved.base_url, "https://api.deepseek.com/v1");
    }

    // ── Edge cases ────────────────────────────────────────────────────

    #[test]
    fn test_streaming_returns_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let p = DeepSeekProvider::new();
        let request = make_simple_request();
        let result = rt.block_on(p.chat_stream(request));
        assert!(result.is_err());
        match result {
            Err(ProviderError::Stream(msg)) => {
                assert!(msg.contains("not yet implemented"));
            }
            _ => panic!("expected Stream error"),
        }
    }

    #[test]
    fn test_transport_extracts_tool_calls() {
        let p = DeepSeekProvider::new();
        let response = serde_json::json!({
            "id": "chatcmpl-456",
            "object": "chat.completion",
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"Tokyo\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let result = p.transport.denormalize_response(&response).unwrap();
        assert!(result.content.is_none());
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "call_abc");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(tcs[0].function.arguments, r#"{"city": "Tokyo"}"#);
        assert_eq!(result.finish_reason, "tool_calls");
    }
}
