//! Transport dispatcher — maps [`ApiProtocol`] to concrete [`Transport`] implementations.
//!
//! This module provides two ways to get a [`Transport`] for a given protocol:
//!
//! 1. **[`create_transport`]** — a standalone factory function that returns a
//!    `Box<dyn Transport>` for a given [`ApiProtocol`]. Prefer this for
//!    one-shot transport construction (e.g. in providers that need a single
//!    transport at startup).
//!
//! 2. **[`TransportDispatcher`]** — a HashMap-backed dispatcher that maps
//!    protocols to transports and supports runtime registration of custom
//!    transports. Useful in test infrastructure and any code that needs to
//!    dispatch to multiple transports through a single registry.
//!
//! Default transports:
//! - [`ChatCompletionsTransport`] for `ApiProtocol::OpenAiChat`
//! - [`AnthropicTransport`] for `ApiProtocol::AnthropicMessages`
//! - [`GeminiTransport`] for `ApiProtocol::GeminiGenerateContent`

use std::collections::HashMap;

use crate::catalog::{ApiProtocol, ResolvedModel};
use crate::transport::anthropic::AnthropicTransport;
use crate::transport::chat_completions::{ChatCompletionsTransport, Transport};
use crate::transport::gemini::GeminiTransport;

// ---------------------------------------------------------------------------
// Factory function
// ---------------------------------------------------------------------------

/// Create a `Box<dyn Transport>` for the given [`ApiProtocol`].
///
/// Returns `None` for unsupported protocols beyond OpenAiChat,
/// AnthropicMessages, and GeminiGenerateContent.
///
/// # Example
///
/// ```ignore
/// use artemis_core::transport::dispatcher::create_transport;
/// use artemis_core::catalog::ApiProtocol;
///
/// let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
/// assert_eq!(transport.api_mode(), "chat_completions");
/// ```
pub fn create_transport(protocol: &ApiProtocol) -> Option<Box<dyn Transport>> {
    match protocol {
        ApiProtocol::OpenAiChat => Some(Box::new(ChatCompletionsTransport::default())),
        ApiProtocol::AnthropicMessages => Some(Box::new(AnthropicTransport::new())),
        ApiProtocol::GeminiGenerateContent => Some(Box::new(GeminiTransport::new())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// TransportDispatcher
// ---------------------------------------------------------------------------

/// Dispatcher that maps [`ApiProtocol`] → concrete [`Transport`] implementation.
///
/// Uses the model catalog's [`ApiProtocol`] as the key, routing requests through
/// the appropriate transport for format normalization/denormalization.
///
/// Internally delegates to [`create_transport`] for the default protocols.
/// For one-shot transport construction without a HashMap registry, prefer
/// [`create_transport`] directly.
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
            create_transport(&ApiProtocol::OpenAiChat).unwrap(),
        );
        dispatcher.register(
            ApiProtocol::AnthropicMessages,
            create_transport(&ApiProtocol::AnthropicMessages).unwrap(),
        );
        dispatcher.register(
            ApiProtocol::GeminiGenerateContent,
            create_transport(&ApiProtocol::GeminiGenerateContent).unwrap(),
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
    use crate::provider::ChatRequest;
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
        let transport = dispatcher
            .dispatch(&ApiProtocol::AnthropicMessages)
            .unwrap();
        assert_eq!(transport.api_mode(), "anthropic");
    }

    #[test]
    fn test_dispatch_gemini() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .unwrap();
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
            Box::new(ChatCompletionsTransport::with_base_url(
                "http://custom:9999/v1",
            )),
        );

        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();
        assert_eq!(transport.base_url(), "http://custom:9999/v1");
    }

    #[test]
    fn test_anthropic_normalize_request() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::AnthropicMessages)
            .unwrap();

        let request = ChatRequest {
            messages: vec![
                crate::types::Message {
                    role: crate::types::Role::System,
                    content: "You are helpful.".into(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
                crate::types::Message {
                    role: crate::types::Role::User,
                    content: "Hello!".into(),
                    reasoning_content: None,
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
            thinking: None,
            reasoning_effort: None,
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
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[test]
    fn test_anthropic_denormalize_response() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::AnthropicMessages)
            .unwrap();

        let response = serde_json::json!({
            "content": [{"type": "text", "text": "Hi there!"}],
            "stop_reason": "end_turn",
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hi there!"));
        assert_eq!(result.finish_reason, "stop");
        assert!(result.usage.is_none());
        assert_eq!(result.model, "");
    }

    #[test]
    fn test_default_impl() {
        let dispatcher = TransportDispatcher::default();
        assert!(dispatcher.dispatch(&ApiProtocol::OpenAiChat).is_some());
        assert!(dispatcher
            .dispatch(&ApiProtocol::AnthropicMessages)
            .is_some());
        assert!(dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .is_some());
    }
}
