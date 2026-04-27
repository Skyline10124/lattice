#![allow(deprecated)]
//! Transport dispatcher — maps [`TransportType`] to concrete [`Transport`] implementations.
//!
//! The dispatcher provides:
//! - **Explicit dispatch**: select a transport by [`TransportType`]
//! - **Auto-detection**: infer [`TransportType`] from a [`ProviderConfig`]'s `api_base` URL
//! - **Combined**: `dispatch_for_config` auto-detects then dispatches
//!
//! Default transports registered in [`TransportDispatcher::new()`]:
//! - [`ChatCompletionsTransport`] for `TransportType::ChatCompletions`
//! - [`AnthropicDispatchTransport`] for `TransportType::Anthropic`
//! - [`GeminiTransport`] for `TransportType::Gemini`

use std::collections::HashMap;

use crate::provider::{ChatRequest, ChatResponse};
use crate::transport::Transport as FormatTransport;
use crate::transport::anthropic::AnthropicTransport;
use crate::transport::chat_completions::{ChatCompletionsTransport, Transport, TransportError};
use crate::transport::gemini::GeminiTransport;
use crate::types::TransportType;
use crate::types::ProviderConfig;

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

/// Dispatcher that maps [`TransportType`] → concrete [`Transport`] implementation.
///
/// Supports explicit dispatch, auto-detection from [`ProviderConfig`], and
/// combined auto-detect-then-dispatch.
pub struct TransportDispatcher {
    transports: HashMap<TransportType, Box<dyn Transport>>,
}

impl TransportDispatcher {
    /// Create a dispatcher pre-loaded with the three default transports:
    /// ChatCompletions, Anthropic, and Gemini.
    pub fn new() -> Self {
        let mut dispatcher = Self {
            transports: HashMap::new(),
        };
        dispatcher.register(
            TransportType::ChatCompletions,
            Box::new(ChatCompletionsTransport::new()),
        );
        dispatcher.register(
            TransportType::Anthropic,
            Box::new(AnthropicDispatchTransport::new()),
        );
        dispatcher.register(
            TransportType::Gemini,
            Box::new(GeminiTransport::new()),
        );
        dispatcher
    }

    /// Register a custom transport for the given [`TransportType`].
    ///
    /// If a transport was already registered for this type, it is replaced.
    pub fn register(&mut self, transport_type: TransportType, transport: Box<dyn Transport>) {
        self.transports.insert(transport_type, transport);
    }

    /// Look up a transport by its [`TransportType`].
    ///
    /// Returns `Err` if no transport is registered for the given type.
    pub fn dispatch(&self, transport_type: &TransportType) -> Result<&dyn Transport, TransportError> {
        self.transports
            .get(transport_type)
            .map(|t| t.as_ref())
            .ok_or_else(|| {
                TransportError::UnexpectedFormat(format!(
                    "no transport registered for {:?}",
                    transport_type
                ))
            })
    }

    /// Auto-detect the [`TransportType`] from a [`ProviderConfig`]'s `api_base` URL.
    ///
    /// Detection rules (by host):
    /// - `api.anthropic.com` → `TransportType::Anthropic`
    /// - `generativelanguage.googleapis.com` → `TransportType::Gemini`
    /// - `localhost:11434` → `TransportType::ChatCompletions` (Ollama)
    /// - Everything else → `TransportType::ChatCompletions` (default)
    pub fn auto_detect(&self, config: &ProviderConfig) -> TransportType {
        let url = config.api_base.to_lowercase();

        if url.contains("api.anthropic.com") {
            TransportType::Anthropic
        } else if url.contains("generativelanguage.googleapis.com") {
            TransportType::Gemini
        } else if url.contains("localhost:11434") {
            TransportType::ChatCompletions
        } else {
            TransportType::ChatCompletions
        }
    }

    /// Auto-detect the transport type from a [`ProviderConfig`] and dispatch.
    ///
    /// Convenience method combining [`auto_detect`](Self::auto_detect) and
    /// [`dispatch`](Self::dispatch).
    pub fn dispatch_for_config(
        &self,
        config: &ProviderConfig,
    ) -> Result<&dyn Transport, TransportError> {
        let transport_type = self.auto_detect(config);
        self.dispatch(&transport_type)
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
    use crate::types::ProviderConfig;

    fn make_config(api_base: &str, transport: TransportType) -> ProviderConfig {
        ProviderConfig {
            name: "test".to_string(),
            api_base: api_base.to_string(),
            api_key: None,
            transport,
            extra_headers: None,
        }
    }

    #[test]
    fn test_dispatch_chat_completions() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&TransportType::ChatCompletions).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");
    }

    #[test]
    fn test_dispatch_anthropic() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&TransportType::Anthropic).unwrap();
        assert_eq!(transport.api_mode(), "anthropic");
    }

    #[test]
    fn test_dispatch_gemini() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&TransportType::Gemini).unwrap();
        assert_eq!(transport.api_mode(), "gemini");
    }

    #[test]
    fn test_dispatch_unregistered_returns_error() {
        let dispatcher = TransportDispatcher::new();
        let result = dispatcher.dispatch(&TransportType::Bedrock);
        assert!(result.is_err());
    }

    #[test]
    fn test_auto_detect_anthropic_url() {
        let dispatcher = TransportDispatcher::new();
        let config = make_config(
            "https://api.anthropic.com/v1/messages",
            TransportType::ChatCompletions, // intentionally wrong — auto_detect should override
        );
        assert_eq!(dispatcher.auto_detect(&config), TransportType::Anthropic);
    }

    #[test]
    fn test_auto_detect_gemini_url() {
        let dispatcher = TransportDispatcher::new();
        let config = make_config(
            "https://generativelanguage.googleapis.com/v1beta",
            TransportType::ChatCompletions,
        );
        assert_eq!(dispatcher.auto_detect(&config), TransportType::Gemini);
    }

    #[test]
    fn test_auto_detect_ollama_url() {
        let dispatcher = TransportDispatcher::new();
        let config = make_config(
            "http://localhost:11434/v1",
            TransportType::ChatCompletions,
        );
        assert_eq!(dispatcher.auto_detect(&config), TransportType::ChatCompletions);
    }

    #[test]
    fn test_auto_detect_default() {
        let dispatcher = TransportDispatcher::new();
        let config = make_config(
            "https://api.openai.com/v1",
            TransportType::ChatCompletions,
        );
        assert_eq!(dispatcher.auto_detect(&config), TransportType::ChatCompletions);
    }

    #[test]
    fn test_auto_detect_case_insensitive() {
        let dispatcher = TransportDispatcher::new();
        let config = make_config(
            "https://API.ANTHROPIC.COM/v1",
            TransportType::ChatCompletions,
        );
        assert_eq!(dispatcher.auto_detect(&config), TransportType::Anthropic);
    }

    #[test]
    fn test_dispatch_for_config() {
        let dispatcher = TransportDispatcher::new();
        let config = make_config(
            "https://api.anthropic.com/v1/messages",
            TransportType::ChatCompletions,
        );
        let transport = dispatcher.dispatch_for_config(&config).unwrap();
        assert_eq!(transport.api_mode(), "anthropic");
    }

    #[test]
    fn test_dispatch_for_config_default() {
        let dispatcher = TransportDispatcher::new();
        let config = make_config(
            "https://api.unknown-provider.com/v1",
            TransportType::ChatCompletions,
        );
        let transport = dispatcher.dispatch_for_config(&config).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");
    }

    #[test]
    fn test_register_custom_transport() {
        let mut dispatcher = TransportDispatcher::new();

        dispatcher.register(
            TransportType::Bedrock,
            Box::new(ChatCompletionsTransport::with_base_url("https://bedrock-runtime.us-east-1.amazonaws.com")),
        );

        let transport = dispatcher.dispatch(&TransportType::Bedrock).unwrap();
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
            TransportType::ChatCompletions,
            Box::new(ChatCompletionsTransport::with_base_url("http://custom:9999/v1")),
        );

        let transport = dispatcher.dispatch(&TransportType::ChatCompletions).unwrap();
        assert_eq!(transport.base_url(), "http://custom:9999/v1");
    }

    #[test]
    fn test_anthropic_normalize_request() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&TransportType::Anthropic).unwrap();

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
            provider_config: make_config(
                "https://api.anthropic.com",
                TransportType::Anthropic,
            ),
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
        let transport = dispatcher.dispatch(&TransportType::Anthropic).unwrap();

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
        assert!(dispatcher.dispatch(&TransportType::ChatCompletions).is_ok());
        assert!(dispatcher.dispatch(&TransportType::Anthropic).is_ok());
        assert!(dispatcher.dispatch(&TransportType::Gemini).is_ok());
    }
}
