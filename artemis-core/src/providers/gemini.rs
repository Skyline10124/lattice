#![allow(deprecated)]
//! Gemini provider adapter — implements the [`Provider`] trait using the
//! Google Gemini generateContent API.
//!
//! Uses [`GeminiTransport`] for message format conversion
//! and makes direct HTTP calls via `reqwest`.

use async_trait::async_trait;

use crate::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use crate::streaming::EventStream;
use crate::transport::chat_completions::Transport;
use crate::transport::gemini::GeminiTransport;

/// Provider adapter for the Google Gemini generateContent API.
///
/// Handles HTTP communication with the Gemini API, using
/// [`GeminiTransport`] for message/tool/response format conversion.
pub struct GeminiProvider {
    transport: GeminiTransport,
}

impl GeminiProvider {
    /// Create a new GeminiProvider with a default transport.
    pub fn new() -> Self {
        GeminiProvider {
            transport: GeminiTransport::new(),
        }
    }

    /// Create a GeminiProvider with a custom transport (e.g. for proxied endpoints).
    pub fn with_transport(transport: GeminiTransport) -> Self {
        GeminiProvider { transport }
    }
}

impl Default for GeminiProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let resolved = &request.resolved;
        let api_model_id = if resolved.api_model_id.is_empty() {
            &request.model
        } else {
            &resolved.api_model_id
        };
        let base_url = if resolved.base_url.is_empty() {
            "https://generativelanguage.googleapis.com/v1beta".to_string()
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

        let client = reqwest::Client::new();
        let url = format!("{}/models/{}:generateContent", base_url, api_model_id);
        let mut req = client.post(&url).json(&body);

        // Auth: use x-goog-api-key header.
        if let Some(ref api_key) = resolved.api_key {
            req = req.header("x-goog-api-key", api_key);
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
            "SSE streaming not yet implemented for Gemini provider".into(),
        ))
    }

    fn name(&self) -> &str {
        "gemini"
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
            provider: "gemini".to_string(),
            api_key: Some("test-key".to_string()),
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
            api_protocol: ApiProtocol::GeminiGenerateContent,
            api_model_id: model_id.to_string(),
            context_length: 1048576,
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
        let resolved = make_resolved("gemini-2.5-flash");
        ChatRequest {
            messages,
            tools: vec![],
            model: "gemini-2.5-flash".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(1024),
            stream: false,
            resolved,
        }
    }

    // ── Basic trait implementation tests ──────────────────────────────

    #[test]
    fn test_gemini_provider_name() {
        let p = GeminiProvider::new();
        assert_eq!(p.name(), "gemini");
        assert!(p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn test_default_implementation() {
        let p = GeminiProvider::default();
        assert_eq!(p.name(), "gemini");
    }

    #[test]
    fn test_with_custom_transport() {
        let transport = GeminiTransport::with_base_url("http://localhost:8080/v1beta");
        let p = GeminiProvider::with_transport(transport);
        assert_eq!(p.name(), "gemini");
    }

    // ── Transport integration tests (no network) ──────────────────────

    #[test]
    fn test_normalize_request_through_transport() {
        let p = GeminiProvider::new();
        let request = make_simple_request();
        let body = p.transport.normalize_request(&request).unwrap();

        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");

        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "Hello!");

        let sys = &body["systemInstruction"];
        assert!(sys.is_object());
        let sys_parts = sys["parts"].as_array().unwrap();
        assert_eq!(sys_parts[0]["text"], "You are helpful.");

        assert_eq!(body["generationConfig"]["temperature"], 0.7);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 1024);
    }

    #[test]
    fn test_denormalize_response_through_transport() {
        let p = GeminiProvider::new();
        let response = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello! How can I help you?"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 8,
                "totalTokenCount": 18
            }
        });

        let result = p.transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("Hello! How can I help you?")
        );
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");

        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.total_tokens, 18);
    }

    // ── Request building from resolved config ─────────────────────────

    #[test]
    fn test_request_uses_resolved_api_model_id() {
        let request = make_simple_request();
        assert_eq!(request.resolved.api_model_id, "gemini-2.5-flash");
        assert_eq!(request.model, "gemini-2.5-flash");
    }

    #[test]
    fn test_request_uses_resolved_base_url() {
        let resolved = make_resolved("gemini-2.5-flash");
        assert_eq!(
            resolved.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn test_default_base_url_when_empty() {
        let p = GeminiProvider::new();
        assert_eq!(
            p.transport.base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    // ── Edge cases ────────────────────────────────────────────────────

    #[test]
    fn test_streaming_returns_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let p = GeminiProvider::new();
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
        let p = GeminiProvider::new();
        let response = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "get_weather", "args": {"city": "Tokyo"}}}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 15,
                "candidatesTokenCount": 7,
                "totalTokenCount": 22
            }
        });

        let result = p.transport.denormalize_response(&response).unwrap();
        assert!(result.content.is_none());
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].function.name, "get_weather");
        let args: serde_json::Value = serde_json::from_str(&tcs[0].function.arguments).unwrap();
        assert_eq!(args["city"], "Tokyo");
        assert_eq!(result.finish_reason, "tool_calls");
    }

    // ── Gemini-specific format tests ──────────────────────────────────

    #[test]
    fn test_model_role_used_for_assistant() {
        let p = GeminiProvider::new();
        let messages = vec![Message {
            role: Role::Assistant,
            content: "I think so.".to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let resolved = make_resolved("gemini-2.5-flash");
        let request = ChatRequest {
            messages,
            tools: vec![],
            model: "gemini-2.5-flash".to_string(),
            temperature: None,
            max_tokens: None,
            stream: false,
            resolved,
        };

        let body = p.transport.normalize_request(&request).unwrap();
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(contents[0]["parts"][0]["text"], "I think so.");
    }

    #[test]
    fn test_safety_finish_mapped_to_content_filter() {
        let p = GeminiProvider::new();
        let response = serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "I cannot help with that."}],
                    "role": "model"
                },
                "finishReason": "SAFETY"
            }]
        });

        let result = p.transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "content_filter");
    }
}
