//! Transport dispatcher — maps [`ApiProtocol`] to concrete [`Transport`] implementations.
//!
//! The dispatcher provides protocol-driven routing: given a [`ResolvedModel`]
//! (which carries an [`ApiProtocol`]), the dispatcher returns the appropriate
//! transport for format normalization/denormalization.
//!
//! Default transports registered in [`TransportDispatcher::new()`]:
//! - [`ChatCompletionsTransport`] for `ApiProtocol::OpenAiChat`
//! - [`AnthropicDispatchTransport`] for `ApiProtocol::AnthropicMessages`
//! - [`GeminiTransport`] for `ApiProtocol::GeminiGenerateContent`

use std::collections::HashMap;

use crate::catalog::{ApiProtocol, ResolvedModel};
use crate::provider::{ChatRequest, ChatResponse};
use crate::transport::Transport as FormatTransport;
use crate::transport::anthropic::AnthropicTransport;
use crate::transport::chat_completions::{ChatCompletionsTransport, Transport, TransportError};
use crate::transport::gemini::GeminiTransport;

// ---------------------------------------------------------------------------
// Anthropic adapter — bridges AnthropicTransport to the Transport trait
// ---------------------------------------------------------------------------

/// Adapter that wraps [`AnthropicTransport`] and implements the [`Transport`] trait.
///
/// `AnthropicTransport` natively implements a different interface
/// (`normalize_messages` / `normalize_tools` / `denormalize_response` returning
/// `NormalizedResponse`). This adapter bridges that gap so the dispatcher can
/// treat all transports uniformly.
struct AnthropicDispatchTransport {
    base_url: String,
    extra_headers: HashMap<String, String>,
    inner: AnthropicTransport,
}

impl AnthropicDispatchTransport {
    fn new() -> Self {
        Self {
            base_url: "https://api.anthropic.com".to_string(),
            extra_headers: HashMap::new(),
            inner: AnthropicTransport,
        }
    }
}

impl Transport for AnthropicDispatchTransport {
    fn base_url(&self) -> &str {
        &self.base_url
    }

    fn extra_headers(&self) -> &HashMap<String, String> {
        &self.extra_headers
    }

    fn api_mode(&self) -> &str {
        "anthropic"
    }

    fn normalize_request(&self, request: &ChatRequest) -> Result<serde_json::Value, TransportError> {
        let normalized = self.inner.normalize_messages(&request.messages);
        let mut body = serde_json::json!({
            "model": request.model,
            "messages": normalized.messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
        });

        if let Some(system) = normalized.system {
            body["system"] = serde_json::Value::String(system);
        }

        if !request.tools.is_empty() {
            let tools = self.inner.normalize_tools(&request.tools);
            body["tools"] = serde_json::Value::Array(tools);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::Value::Number(
                serde_json::Number::from_f64(temp)
                    .unwrap_or_else(|| serde_json::Number::from(0)),
            );
        }

        if request.stream {
            body["stream"] = serde_json::Value::Bool(true);
        }

        Ok(body)
    }

    fn denormalize_response(
        &self,
        response: &serde_json::Value,
    ) -> Result<ChatResponse, TransportError> {
        let normalized = self.inner.denormalize_response(response);
        Ok(ChatResponse {
            content: normalized.content,
            tool_calls: normalized.tool_calls,
            usage: None, // Anthropic usage extraction happens at the HTTP layer
            finish_reason: normalized.finish_reason,
            model: String::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// TransportDispatcher
// ---------------------------------------------------------------------------

/// Dispatcher that maps [`ApiProtocol`] → concrete [`Transport`] implementation.
///
/// Uses the model catalog's [`ApiProtocol`] as the key, routing requests through
/// the appropriate transport for format normalization/denormalization.
pub struct TransportDispatcher {
    transports: HashMap<ApiProtocol, Box<dyn Transport>>,
}

impl TransportDispatcher {
    /// Create a dispatcher pre-loaded with the three default transports:
    /// OpenAiChat (ChatCompletions), AnthropicMessages, and GeminiGenerateContent.
    pub fn new() -> Self {
        let mut dispatcher = Self {
            transports: HashMap::new(),
        };
        dispatcher.register(
            ApiProtocol::OpenAiChat,
            Box::new(ChatCompletionsTransport::default()),
        );
        dispatcher.register(
            ApiProtocol::AnthropicMessages,
            Box::new(AnthropicDispatchTransport::new()),
        );
        dispatcher.register(
            ApiProtocol::GeminiGenerateContent,
            Box::new(GeminiTransport::new()),
        );
        dispatcher
    }

    /// Register a custom transport for the given [`ApiProtocol`].
    ///
    /// If a transport was already registered for this protocol, it is replaced.
    pub fn register(&mut self, protocol: ApiProtocol, transport: Box<dyn Transport>) {
        self.transports.insert(protocol, transport);
    }

    /// Look up a transport by its [`ApiProtocol`].
    ///
    /// Returns `None` if no transport is registered for the given protocol.
    pub fn dispatch(&self, protocol: &ApiProtocol) -> Option<&dyn Transport> {
        self.transports.get(protocol).map(|t| t.as_ref())
    }

    /// Convenience method that dispatches from a [`ResolvedModel`]'s `api_protocol`.
    ///
    /// This is the primary entry point for protocol-driven routing:
    /// the catalog resolves a canonical model ID to a [`ResolvedModel`],
    /// and the dispatcher routes to the correct transport based on the
    /// resolved protocol.
    pub fn dispatch_for_resolved(&self, resolved: &ResolvedModel) -> Option<&dyn Transport> {
        self.dispatch(&resolved.api_protocol)
    }
}

impl Default for TransportDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_dispatch_chat_completions() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");
    }

    #[test]
    fn test_dispatch_anthropic() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::AnthropicMessages).unwrap();
        assert_eq!(transport.api_mode(), "anthropic");
    }

    #[test]
    fn test_dispatch_gemini() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::GeminiGenerateContent).unwrap();
        assert_eq!(transport.api_mode(), "gemini");
    }

    #[test]
    fn test_dispatch_unregistered_returns_none() {
        let dispatcher = TransportDispatcher::new();
        let result = dispatcher.dispatch(&ApiProtocol::BedrockConverse);
        assert!(result.is_none());
    }

    #[test]
    fn test_dispatch_for_resolved() {
        let dispatcher = TransportDispatcher::new();
        let resolved = ResolvedModel {
            canonical_id: "claude-3-opus".into(),
            provider: "anthropic".into(),
            api_key: None,
            base_url: "https://api.anthropic.com".into(),
            api_protocol: ApiProtocol::AnthropicMessages,
            api_model_id: "claude-3-opus".into(),
            context_length: 200000,
            provider_specific: HashMap::new(),
        };
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "anthropic");
    }

    #[test]
    fn test_dispatch_for_resolved_chat_completions() {
        let dispatcher = TransportDispatcher::new();
        let resolved = ResolvedModel {
            canonical_id: "gpt-4o".into(),
            provider: "openai".into(),
            api_key: None,
            base_url: "https://api.openai.com/v1".into(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "gpt-4o".into(),
            context_length: 128000,
            provider_specific: HashMap::new(),
        };
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");
    }

    #[test]
    fn test_dispatch_for_resolved_gemini() {
        let dispatcher = TransportDispatcher::new();
        let resolved = ResolvedModel {
            canonical_id: "gemini-2.0-flash".into(),
            provider: "google".into(),
            api_key: None,
            base_url: "https://generativelanguage.googleapis.com".into(),
            api_protocol: ApiProtocol::GeminiGenerateContent,
            api_model_id: "gemini-2.0-flash".into(),
            context_length: 1048576,
            provider_specific: HashMap::new(),
        };
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "gemini");
    }

    #[test]
    fn test_dispatch_for_resolved_unregistered() {
        let dispatcher = TransportDispatcher::new();
        let resolved = ResolvedModel {
            canonical_id: "unknown-model".into(),
            provider: "test".into(),
            api_key: None,
            base_url: "https://api.example.com".into(),
            api_protocol: ApiProtocol::Custom("unregistered".into()),
            api_model_id: "unknown-model".into(),
            context_length: 0,
            provider_specific: HashMap::new(),
        };
        let result = dispatcher.dispatch_for_resolved(&resolved);
        assert!(result.is_none());
    }

    #[test]
    fn test_register_custom_transport() {
        let mut dispatcher = TransportDispatcher::new();

        dispatcher.register(
            ApiProtocol::BedrockConverse,
            Box::new(ChatCompletionsTransport::with_base_url(
                "https://bedrock-runtime.us-east-1.amazonaws.com",
            )),
        );

        let transport = dispatcher.dispatch(&ApiProtocol::BedrockConverse).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");
        assert_eq!(
            transport.base_url(),
            "https://bedrock-runtime.us-east-1.amazonaws.com"
        );
    }

    #[test]
    fn test_register_replaces_existing() {
        let mut dispatcher = TransportDispatcher::new();

        dispatcher.register(
            ApiProtocol::OpenAiChat,
            Box::new(ChatCompletionsTransport::with_base_url("http://custom:9999/v1")),
        );

        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();
        assert_eq!(transport.base_url(), "http://custom:9999/v1");
    }

    #[test]
    fn test_anthropic_normalize_request() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::AnthropicMessages).unwrap();

        let request = ChatRequest {
            messages: vec![
                crate::types::Message {
                    role: crate::types::Role::System,
                    content: "You are helpful.".into(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                crate::types::Message {
                    role: crate::types::Role::User,
                    content: "Hello!".into(),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            tools: vec![],
            model: "claude-3-opus".into(),
            temperature: Some(0.5),
            max_tokens: Some(200),
            stream: false,
            resolved: ResolvedModel {
                canonical_id: "claude-3-opus".into(),
                provider: "anthropic".into(),
                api_key: None,
                base_url: "https://api.anthropic.com".into(),
                api_protocol: ApiProtocol::AnthropicMessages,
                api_model_id: "claude-3-opus".into(),
                context_length: 200000,
                provider_specific: HashMap::new(),
            },
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "claude-3-opus");
        assert_eq!(body["system"], "You are helpful.");
        assert_eq!(body["max_tokens"], 200);
        assert_eq!(body["temperature"], 0.5);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1); // system extracted separately
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn test_anthropic_denormalize_response() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::AnthropicMessages).unwrap();

        let response = serde_json::json!({
            "content": [{"type": "text", "text": "Hi there!"}],
            "stop_reason": "end_turn",
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hi there!"));
        assert_eq!(result.finish_reason, "stop");
    }

    #[test]
    fn test_default_impl() {
        let dispatcher = TransportDispatcher::default();
        assert!(dispatcher.dispatch(&ApiProtocol::OpenAiChat).is_some());
        assert!(dispatcher.dispatch(&ApiProtocol::AnthropicMessages).is_some());
        assert!(dispatcher.dispatch(&ApiProtocol::GeminiGenerateContent).is_some());
    }
}
