//! Ollama provider adapter — implements the [`Provider`] trait for local
//! Ollama instances exposing an OpenAI-compatible endpoint.
//!
//! Uses [`ChatCompletionsTransport`] for format normalization/denormalization
//! and makes direct HTTP calls via `reqwest`.

use async_trait::async_trait;

use crate::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use crate::streaming::EventStream;
use crate::transport::chat_completions::ChatCompletionsTransport;

/// Provider adapter for Ollama's OpenAI-compatible API.
///
/// Relies on the catalog to supply `resolved.base_url` (typically
/// `http://localhost:11434/v1`). No default base URL is hard-coded so that
/// users can point at any Ollama instance.
pub struct OllamaProvider {
    transport: ChatCompletionsTransport,
}

impl OllamaProvider {
    /// Create a new OllamaProvider with a default transport.
    pub fn new() -> Self {
        OllamaProvider {
            transport: ChatCompletionsTransport::with_base_url(""),
        }
    }

    /// Create an OllamaProvider with a custom transport (e.g. for proxied endpoints).
    pub fn with_transport(transport: ChatCompletionsTransport) -> Self {
        OllamaProvider { transport }
    }
}

impl Default for OllamaProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Provider for OllamaProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        let base_url = request.resolved.base_url.clone();
        super::openai_compat_chat(&self.transport, &request, &base_url).await
    }

    async fn chat_stream(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Stream(
            "SSE streaming not yet implemented for Ollama provider".into(),
        ))
    }

    fn name(&self) -> &str {
        "ollama"
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
            provider: "ollama".to_string(),
            api_key: None,
            base_url: "http://localhost:11434/v1".to_string(),
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
        let resolved = make_resolved("llama3");
        ChatRequest {
            messages,
            tools: vec![],
            model: "llama3".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(1024),
            stream: false,
            resolved,
        }
    }

    #[test]
    fn test_ollama_provider_name() {
        let p = OllamaProvider::new();
        assert_eq!(p.name(), "ollama");
        assert!(!p.supports_streaming());
        assert!(p.supports_tools());
    }

    #[test]
    fn test_default_implementation() {
        let p = OllamaProvider::default();
        assert_eq!(p.name(), "ollama");
    }

    #[test]
    fn test_with_custom_transport() {
        let transport = ChatCompletionsTransport::with_base_url("http://localhost:8080/v1");
        let p = OllamaProvider::with_transport(transport);
        assert_eq!(p.name(), "ollama");
    }

    #[test]
    fn test_normalize_request_through_transport() {
        let p = OllamaProvider::new();
        let request = make_simple_request();
        let body = p.transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "llama3");
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
        assert_eq!(request.resolved.api_model_id, "llama3");
        assert_eq!(request.model, "llama3");
    }

    #[test]
    fn test_request_uses_resolved_base_url() {
        let resolved = make_resolved("llama3");
        assert_eq!(resolved.base_url, "http://localhost:11434/v1");
    }

    #[test]
    fn test_streaming_returns_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let p = OllamaProvider::new();
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
