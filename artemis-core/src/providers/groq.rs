//! Groq provider adapter — implements the [`Provider`] trait for Groq's
//! OpenAI-compatible Chat Completions API.
//!
//! Uses [`ChatCompletionsTransport`] for format normalization/denormalization
//! and makes direct HTTP calls via `reqwest`.

use async_trait::async_trait;

use crate::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use crate::streaming::EventStream;
use crate::transport::chat_completions::ChatCompletionsTransport;

/// Provider adapter for the Groq Chat Completions API.
///
/// Groq exposes an OpenAI-compatible endpoint at `https://api.groq.com/openai/v1`.
/// Authentication uses a standard Bearer token.
pub struct GroqProvider {
    transport: ChatCompletionsTransport,
}

impl GroqProvider {
    pub fn new() -> Self {
        GroqProvider {
            transport: ChatCompletionsTransport::with_base_url("https://api.groq.com/openai/v1"),
        }
    }

    pub fn with_transport(transport: ChatCompletionsTransport) -> Self {
        GroqProvider { transport }
    }
}

impl Default for GroqProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for GroqProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let base_url = if request.resolved.base_url.is_empty() {
            "https://api.groq.com/openai/v1".to_string()
        } else {
            request.resolved.base_url.clone()
        };
        super::openai_compat_chat(&self.transport, &request, &base_url).await
    }

    async fn chat_stream(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Stream(
            "SSE streaming not yet implemented for Groq provider".into(),
        ))
    }

    fn name(&self) -> &str {
        "groq"
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
    use crate::transport::Transport;
    use crate::types::{Message, Role};
    use std::collections::HashMap;

    fn make_resolved(model_id: &str) -> crate::catalog::ResolvedModel {
        crate::catalog::ResolvedModel {
            canonical_id: model_id.to_string(),
            provider: "groq".to_string(),
            api_key: Some("gsk_test".to_string()),
            base_url: "https://api.groq.com/openai/v1".to_string(),
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
        let resolved = make_resolved("llama-3.3-70b-versatile");
        ChatRequest {
            messages,
            tools: vec![],
            model: "llama-3.3-70b-versatile".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(1024),
            stream: false,
            resolved,
        }
    }

    #[test]
    fn test_groq_provider_name() {
        let p = GroqProvider::new();
        assert_eq!(p.name(), "groq");
        assert!(!p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn test_default_implementation() {
        let p = GroqProvider::default();
        assert_eq!(p.name(), "groq");
    }

    #[test]
    fn test_with_custom_transport() {
        let transport = ChatCompletionsTransport::with_base_url("http://localhost:8080/v1");
        let p = GroqProvider::with_transport(transport);
        assert_eq!(p.name(), "groq");
    }

    #[test]
    fn test_default_base_url() {
        let p = GroqProvider::new();
        assert_eq!(p.transport.base_url(), "https://api.groq.com/openai/v1");
    }

    #[test]
    fn test_normalize_request_through_transport() {
        let p = GroqProvider::new();
        let request = make_simple_request();
        let body = p.transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "llama-3.3-70b-versatile");
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
    fn test_request_uses_resolved_api_model_id() {
        let request = make_simple_request();
        assert_eq!(request.resolved.api_model_id, "llama-3.3-70b-versatile");
        assert_eq!(request.model, "llama-3.3-70b-versatile");
    }

    #[test]
    fn test_request_uses_resolved_base_url() {
        let resolved = make_resolved("llama-3.3-70b-versatile");
        assert_eq!(resolved.base_url, "https://api.groq.com/openai/v1");
    }

    #[test]
    fn test_streaming_returns_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let p = GroqProvider::new();
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
}
