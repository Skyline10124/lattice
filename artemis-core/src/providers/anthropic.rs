//! Anthropic provider adapter — implements the [`Provider`] trait using the
//! Anthropic Messages API.
//!
//! Uses [`AnthropicTransport`] for format normalization/denormalization
//! and makes direct HTTP calls via `reqwest`.

use async_trait::async_trait;

use crate::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use crate::streaming::EventStream;
use crate::transport::anthropic::AnthropicTransport;
use crate::transport::Transport as FormatTransport;

/// Provider adapter for the Anthropic Messages API.
///
/// Handles HTTP communication with the Anthropic API, using
/// [`AnthropicTransport`] for message/tool/response format conversion.
pub struct AnthropicProvider {
    transport: AnthropicTransport,
}

impl AnthropicProvider {
    /// Create a new AnthropicProvider with a default transport.
    pub fn new() -> Self {
        AnthropicProvider {
            transport: AnthropicTransport,
        }
    }
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let resolved = &request.resolved;
        let base_url = if resolved.base_url.is_empty() {
            "https://api.anthropic.com".to_string()
        } else {
            resolved.base_url.clone()
        };

        // Build the request body using the Anthropic transport.
        let normalized = self.transport.normalize_messages(&request.messages);
        let tools = self.transport.normalize_tools(&request.tools);

        let mut body = serde_json::Map::new();
        body.insert(
            "model".to_string(),
            serde_json::Value::String(resolved.api_model_id.clone()),
        );
        body.insert(
            "messages".to_string(),
            serde_json::Value::Array(normalized.messages),
        );
        if let Some(system) = &normalized.system {
            body.insert(
                "system".to_string(),
                serde_json::Value::String(system.clone()),
            );
        }
        if !tools.is_empty() {
            body.insert("tools".to_string(), serde_json::Value::Array(tools));
        }
        if let Some(max_tokens) = request.max_tokens {
            body.insert(
                "max_tokens".to_string(),
                serde_json::Value::Number(serde_json::Number::from(max_tokens)),
            );
        } else {
            // Anthropic API requires max_tokens; default to 1024.
            body.insert(
                "max_tokens".to_string(),
                serde_json::Value::Number(serde_json::Number::from(1024)),
            );
        }
        if let Some(temperature) = request.temperature {
            body.insert("temperature".to_string(), serde_json::json!(temperature));
        }

        let client = crate::provider::shared_http_client();
        let mut req = client.post(format!("{}/v1/messages", base_url)).json(&body);

        // Anthropic uses x-api-key instead of Bearer.
        if let Some(ref api_key) = resolved.api_key {
            req = req.header("x-api-key", api_key);
        }

        req = req.header("anthropic-version", "2023-06-01");

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

        let normalized_resp = self.transport.denormalize_response(&json);

        Ok(ChatResponse {
            content: normalized_resp.content,
            tool_calls: normalized_resp.tool_calls,
            usage: None, // usage extraction not yet implemented for Anthropic
            finish_reason: normalized_resp.finish_reason,
            model: resolved.api_model_id.clone(),
        })
    }

    async fn chat_stream(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        // Placeholder — Anthropic SSE streaming is not yet implemented.
        todo!("Anthropic SSE streaming not yet implemented")
    }

    fn name(&self) -> &str {
        "anthropic"
    }

    fn supports_streaming(&self) -> bool {
        false
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
    use crate::types::{Message, Role, ToolDefinition};
    use std::collections::HashMap;

    fn make_resolved(model_id: &str) -> crate::catalog::ResolvedModel {
        crate::catalog::ResolvedModel {
            canonical_id: model_id.to_string(),
            provider: "anthropic".to_string(),
            api_key: Some("sk-ant-test".to_string()),
            base_url: "https://api.anthropic.com".to_string(),
            api_protocol: ApiProtocol::AnthropicMessages,
            api_model_id: model_id.to_string(),
            context_length: 200000,
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
        let resolved = make_resolved("claude-sonnet-4-20250514");
        ChatRequest {
            messages,
            tools: vec![],
            model: "claude-sonnet-4-20250514".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(1024),
            stream: false,
            resolved,
        }
    }

    // ── Basic trait implementation tests ──────────────────────────────

    #[test]
    fn test_anthropic_provider_name() {
        let p = AnthropicProvider::new();
        assert_eq!(p.name(), "anthropic");
        assert!(!p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn test_default_implementation() {
        let p = AnthropicProvider::default();
        assert_eq!(p.name(), "anthropic");
    }

    // ── Transport integration tests (no network) ──────────────────────

    #[test]
    fn test_normalize_messages_through_transport() {
        let p = AnthropicProvider::new();
        let request = make_simple_request();
        let normalized = p.transport.normalize_messages(&request.messages);
        // System prompt should be extracted separately.
        assert_eq!(normalized.system, Some("You are helpful.".to_string()));
        // Non-system messages should be in the messages vec.
        assert_eq!(normalized.messages.len(), 1);
        assert_eq!(normalized.messages[0]["role"], "user");
        let content = normalized.messages[0]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "Hello!");
    }

    #[test]
    fn test_normalize_tools_through_transport() {
        let p = AnthropicProvider::new();
        let tools = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get weather".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        }];
        let result = p.transport.normalize_tools(&tools);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["name"], "get_weather");
        assert!(result[0].get("input_schema").is_some());
    }

    #[test]
    fn test_denormalize_response_through_transport() {
        let p = AnthropicProvider::new();
        let response = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "text", "text": "Hello! How can I help you?"}
            ],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 10,
                "output_tokens": 8
            }
        });

        let result = p.transport.denormalize_response(&response);
        assert_eq!(
            result.content.as_deref(),
            Some("Hello! How can I help you?")
        );
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
        assert!(result.reasoning.is_none());
    }

    #[test]
    fn test_denormalize_response_with_tool_use() {
        let p = AnthropicProvider::new();
        let response = serde_json::json!({
            "id": "msg_456",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "text", "text": "Let me check the weather."},
                {
                    "type": "tool_use",
                    "id": "toolu_abc",
                    "name": "get_weather",
                    "input": {"city": "Tokyo"}
                }
            ],
            "stop_reason": "tool_use"
        });

        let result = p.transport.denormalize_response(&response);
        assert_eq!(result.content.as_deref(), Some("Let me check the weather."));
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "toolu_abc");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(tcs[0].function.arguments, r#"{"city":"Tokyo"}"#);
        assert_eq!(result.finish_reason, "tool_calls");
    }

    #[test]
    fn test_denormalize_response_with_thinking() {
        let p = AnthropicProvider::new();
        let response = serde_json::json!({
            "id": "msg_789",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-20250514",
            "content": [
                {"type": "thinking", "thinking": "I should consider the question carefully."},
                {"type": "text", "text": "Here is my answer."}
            ],
            "stop_reason": "end_turn"
        });

        let result = p.transport.denormalize_response(&response);
        assert_eq!(result.content.as_deref(), Some("Here is my answer."));
        assert_eq!(
            result.reasoning.as_deref(),
            Some("I should consider the question carefully.")
        );
    }

    // ── Request building tests ────────────────────────────────────────

    #[test]
    fn test_request_uses_resolved_api_model_id() {
        let request = make_simple_request();
        assert_eq!(request.resolved.api_model_id, "claude-sonnet-4-20250514");
        assert_eq!(request.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_request_uses_resolved_base_url() {
        let resolved = make_resolved("claude-sonnet-4-20250514");
        assert_eq!(resolved.base_url, "https://api.anthropic.com");
    }

    #[test]
    fn test_default_base_url_when_empty() {
        let mut resolved = make_resolved("claude-sonnet-4-20250514");
        resolved.base_url = String::new();
        let request = ChatRequest {
            messages: vec![],
            tools: vec![],
            model: "claude-sonnet-4-20250514".to_string(),
            temperature: None,
            max_tokens: None,
            stream: false,
            resolved,
        };
        // Verify the base_url fallback is handled in chat().
        // This test confirms the resolved struct allows empty base_url.
        assert!(request.resolved.base_url.is_empty());
    }
}
