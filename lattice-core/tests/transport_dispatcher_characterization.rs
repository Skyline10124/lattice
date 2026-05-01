//! Characterization tests for TransportDispatcher.
//!
//! These tests capture the CURRENT behavior of the dispatcher and its registered
//! transports before any architecture changes (T16, T21) are made. They are
//! intentionally redundant with some unit tests in `dispatcher.rs` and integration
//! tests in `transport_integration.rs` — the goal is to lock down observed
//! behavior as a safety net for future refactoring.
//!
//! # What is characterized
//!
//! 1. **Dispatcher construction**: `TransportDispatcher::new()` registers exactly
//!    3 protocols (OpenAiChat, AnthropicMessages, GeminiGenerateContent).
//! 2. **`dispatch()` routing**: each registered `ApiProtocol` maps to the correct
//!    transport, identified by `api_mode()`.
//! 3. **`dispatch_for_resolved()`**: convenience method delegates to `dispatch()`
//!    using `ResolvedModel.api_protocol`.
//! 4. **Unregistered protocols**: `CodexResponses` returns
//!    `None` from `dispatch()`.
//! 5. **AnthropicDispatchTransport adapter**: the private adapter bridges the
//!    `FormatTransport` trait (AnthropicTransport) to the `Transport` trait
//!    used by the dispatcher. Key behavioral quirks:
//!    - `denormalize_response` always sets `usage: None`
//!    - `denormalize_response` always sets `model: ""` (empty string)
//!    - `normalize_request` extracts system prompt to top-level `"system"` key
//!    - `normalize_request` defaults `max_tokens` to 4096 when `None`
//! 6. **ChatCompletionsTransport via dispatcher**: verify base_url, api_mode,
//!    and normalize/denormalize roundtrip.
//! 7. **GeminiTransport via dispatcher**: verify base_url, api_mode,
//!    and normalize/denormalize roundtrip.

use lattice_core::catalog::{ApiProtocol, CredentialStatus, ResolvedModel};
use lattice_core::provider::ChatRequest;
use lattice_core::transport::chat_completions::ChatCompletionsTransport;
use lattice_core::transport::dispatcher::TransportDispatcher;
use lattice_core::transport::Transport;
use lattice_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════
// Helpers (mirrors transport_integration.rs)
// ═══════════════════════════════════════════════════════════════════════════

fn make_resolved(provider: &str, api_protocol: ApiProtocol, base_url: &str) -> ResolvedModel {
    ResolvedModel {
        canonical_id: "test-model".to_string(),
        provider: provider.to_string(),
        api_key: Some("sk-test".to_string()),
        base_url: base_url.to_string(),
        api_protocol,
        api_model_id: "test-model".to_string(),
        context_length: 131072,
        provider_specific: HashMap::new(),
        credential_status: CredentialStatus::Present,
    }
}

fn make_chat_request(messages: Vec<Message>, tools: Vec<ToolDefinition>) -> ChatRequest {
    ChatRequest::new(
        messages,
        tools,
        make_resolved("test", ApiProtocol::OpenAiChat, "https://api.test.com/v1"),
    )
}

fn sample_messages() -> Vec<Message> {
    vec![
        Message {
            role: Role::System,
            content: "You are a helpful assistant.".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::User,
            content: "What's the weather in Tokyo?".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::Assistant,
            content: "Let me check the weather.".to_string(),
            reasoning_content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_abc".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city": "Tokyo"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::Tool,
            content: r#"{"temp": 22, "condition": "sunny"}"#.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_abc".to_string()),
            name: Some("get_weather".to_string()),
        },
    ]
}

fn sample_tools() -> Vec<ToolDefinition> {
    vec![ToolDefinition {
        name: "get_weather".to_string(),
        description: "Get the current weather for a city".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"}
            },
            "required": ["city"]
        }),
    }]
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. Dispatcher construction characterization
// ═══════════════════════════════════════════════════════════════════════════

mod dispatcher_construction {
    use super::*;

    /// CHARACTERIZATION: TransportDispatcher::new() registers exactly 3 protocols.
    /// The three registered protocols are OpenAiChat, AnthropicMessages, and
    /// GeminiGenerateContent. All other ApiProtocol variants return None.
    #[test]
    fn new_registers_three_protocols() {
        let dispatcher = TransportDispatcher::new();

        // All three registered protocols should return Some
        assert!(
            dispatcher.dispatch(&ApiProtocol::OpenAiChat).is_some(),
            "OpenAiChat should be registered"
        );
        assert!(
            dispatcher
                .dispatch(&ApiProtocol::AnthropicMessages)
                .is_some(),
            "AnthropicMessages should be registered"
        );
        assert!(
            dispatcher
                .dispatch(&ApiProtocol::GeminiGenerateContent)
                .is_some(),
            "GeminiGenerateContent should be registered"
        );
    }

    /// CHARACTERIZATION: Default trait impl calls new() — same 3 protocols registered.
    #[test]
    fn default_same_as_new() {
        let from_new = TransportDispatcher::new();
        let from_default = TransportDispatcher::default();

        // Both should have the same registered protocols
        assert!(from_new.dispatch(&ApiProtocol::OpenAiChat).is_some());
        assert!(from_default.dispatch(&ApiProtocol::OpenAiChat).is_some());

        assert!(from_new.dispatch(&ApiProtocol::AnthropicMessages).is_some());
        assert!(from_default
            .dispatch(&ApiProtocol::AnthropicMessages)
            .is_some());

        assert!(from_new
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .is_some());
        assert!(from_default
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .is_some());
    }

    /// CHARACTERIZATION: Registering a new protocol adds it, registering an
    /// existing protocol replaces it.
    #[test]
    fn register_adds_and_replaces() {
        let mut dispatcher = TransportDispatcher::new();

        // CodexResponses is not registered by default
        assert!(dispatcher.dispatch(&ApiProtocol::CodexResponses).is_none());

        // Register it
        dispatcher.register(
            ApiProtocol::CodexResponses,
            Box::new(ChatCompletionsTransport::with_base_url(
                "https://api.openai.com/v1",
            )),
        );
        assert!(dispatcher.dispatch(&ApiProtocol::CodexResponses).is_some());

        // Replace existing OpenAiChat with custom base URL
        let custom_url = "http://custom:9999/v1";
        dispatcher.register(
            ApiProtocol::OpenAiChat,
            Box::new(ChatCompletionsTransport::with_base_url(custom_url)),
        );
        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();
        assert_eq!(transport.base_url(), custom_url);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. dispatch() routing characterization
// ═══════════════════════════════════════════════════════════════════════════

mod dispatch_routing {
    use super::*;

    /// CHARACTERIZATION: ApiProtocol::OpenAiChat → ChatCompletionsTransport
    /// with api_mode "chat_completions" and base_url "https://api.openai.com/v1".
    #[test]
    fn openai_chat_returns_chat_completions_transport() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();

        assert_eq!(transport.api_mode(), "chat_completions");
        assert_eq!(transport.base_url(), "https://api.openai.com/v1");
        assert!(transport.extra_headers().is_empty());
    }

    /// CHARACTERIZATION: ApiProtocol::AnthropicMessages → AnthropicDispatchTransport
    /// (private adapter) with api_mode "anthropic" and base_url
    /// "https://api.anthropic.com".
    #[test]
    fn anthropic_messages_returns_anthropic_dispatch_transport() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::AnthropicMessages)
            .unwrap();

        assert_eq!(transport.api_mode(), "anthropic");
        assert_eq!(transport.base_url(), "https://api.anthropic.com");
        assert!(transport.extra_headers().is_empty());
    }

    /// CHARACTERIZATION: ApiProtocol::GeminiGenerateContent → GeminiTransport
    /// with api_mode "gemini" and base_url
    /// "https://generativelanguage.googleapis.com/v1beta".
    #[test]
    fn gemini_returns_gemini_transport() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .unwrap();

        assert_eq!(transport.api_mode(), "gemini");
        assert_eq!(
            transport.base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert!(transport.extra_headers().is_empty());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. dispatch_for_resolved() characterization
// ═══════════════════════════════════════════════════════════════════════════

mod dispatch_for_resolved {
    use super::*;

    /// CHARACTERIZATION: dispatch_for_resolved with ApiProtocol::OpenAiChat
    /// returns a transport with api_mode "chat_completions".
    #[test]
    fn openai_chat_resolved_model() {
        let dispatcher = TransportDispatcher::new();
        let resolved = make_resolved(
            "openai",
            ApiProtocol::OpenAiChat,
            "https://api.openai.com/v1",
        );
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "chat_completions");
    }

    /// CHARACTERIZATION: dispatch_for_resolved with ApiProtocol::AnthropicMessages
    /// returns a transport with api_mode "anthropic".
    #[test]
    fn anthropic_messages_resolved_model() {
        let dispatcher = TransportDispatcher::new();
        let resolved = make_resolved(
            "anthropic",
            ApiProtocol::AnthropicMessages,
            "https://api.anthropic.com",
        );
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "anthropic");
    }

    /// CHARACTERIZATION: dispatch_for_resolved with ApiProtocol::GeminiGenerateContent
    /// returns a transport with api_mode "gemini".
    #[test]
    fn gemini_resolved_model() {
        let dispatcher = TransportDispatcher::new();
        let resolved = make_resolved(
            "google",
            ApiProtocol::GeminiGenerateContent,
            "https://generativelanguage.googleapis.com",
        );
        let transport = dispatcher.dispatch_for_resolved(&resolved).unwrap();
        assert_eq!(transport.api_mode(), "gemini");
    }

    /// CHARACTERIZATION: dispatch_for_resolved with ApiProtocol::CodexResponses
    /// returns None — Codex is not a registered transport.
    #[test]
    fn codex_responses_resolved_model_returns_none() {
        let dispatcher = TransportDispatcher::new();
        let resolved = make_resolved(
            "codex",
            ApiProtocol::CodexResponses,
            "https://api.openai.com/v1",
        );
        assert!(dispatcher.dispatch_for_resolved(&resolved).is_none());
    }

    /// CHARACTERIZATION: dispatch_for_resolved with a Custom protocol returns None.
    #[test]
    fn custom_protocol_resolved_model_returns_none() {
        let dispatcher = TransportDispatcher::new();
        let resolved = make_resolved(
            "custom",
            ApiProtocol::Custom("some_protocol".into()),
            "https://api.example.com",
        );
        assert!(dispatcher.dispatch_for_resolved(&resolved).is_none());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Unregistered protocol characterization
// ═══════════════════════════════════════════════════════════════════════════

mod unregistered_protocols {
    use super::*;

    /// CHARACTERIZATION: CodexResponses is not registered in the default
    /// dispatcher.
    #[test]
    fn codex_responses_not_registered() {
        let dispatcher = TransportDispatcher::new();
        assert!(
            dispatcher.dispatch(&ApiProtocol::CodexResponses).is_none(),
            "CodexResponses should NOT be registered by default"
        );
    }

    /// CHARACTERIZATION: Custom protocols are never registered by default.
    #[test]
    fn custom_protocol_not_registered() {
        let dispatcher = TransportDispatcher::new();
        assert!(
            dispatcher
                .dispatch(&ApiProtocol::Custom("anything".into()))
                .is_none(),
            "Custom protocols should NOT be registered by default"
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. AnthropicDispatchTransport adapter characterization
// ═══════════════════════════════════════════════════════════════════════════

mod anthropic_dispatch_adapter {
    use super::*;

    /// Helper: get the Anthropic dispatch transport from the default dispatcher.
    fn get_anthropic_transport() -> &'static dyn Transport {
        // We use a leaked Box to get a 'static reference for test ergonomics.
        // This is test-only code.
        let dispatcher = Box::leak(Box::new(TransportDispatcher::new()));
        dispatcher
            .dispatch(&ApiProtocol::AnthropicMessages)
            .unwrap()
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport.normalize_request extracts
    /// the system message to a top-level "system" key and removes it from the
    /// messages array. The remaining messages contain only user/assistant/tool.
    #[test]
    fn normalize_request_extracts_system_to_top_level() {
        let transport = get_anthropic_transport();
        let request = ChatRequest {
            messages: sample_messages(),
            tools: vec![],
            model: "claude-3-opus".to_string(),
            temperature: None,
            max_tokens: Some(200),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "anthropic",
                ApiProtocol::AnthropicMessages,
                "https://api.anthropic.com",
            ),
        };

        let body = transport.normalize_request(&request).unwrap();

        // System extracted to top-level key
        assert_eq!(body["system"], "You are a helpful assistant.");

        // System message NOT in messages array
        let messages = body["messages"].as_array().unwrap();
        for msg in messages {
            assert_ne!(msg["role"], "system");
        }
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport.normalize_request defaults
    /// max_tokens to 4096 when ChatRequest.max_tokens is None.
    #[test]
    fn normalize_request_defaults_max_tokens_to_4096() {
        let transport = get_anthropic_transport();
        let request = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hello".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "claude-3-opus".to_string(),
            temperature: None,
            max_tokens: None, // ← None, should default to 4096
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "anthropic",
                ApiProtocol::AnthropicMessages,
                "https://api.anthropic.com",
            ),
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["max_tokens"], 4096);
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport.normalize_request includes
    /// temperature when Some, omits it when None.
    #[test]
    fn normalize_request_temperature_handling() {
        let transport = get_anthropic_transport();

        // With temperature
        let request_with_temp = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hello".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "claude-3-opus".to_string(),
            temperature: Some(0.7),
            max_tokens: Some(100),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "anthropic",
                ApiProtocol::AnthropicMessages,
                "https://api.anthropic.com",
            ),
        };
        let body = transport.normalize_request(&request_with_temp).unwrap();
        assert_eq!(body["temperature"], 0.7);

        // Without temperature — key should not be present
        let request_no_temp = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hello".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "claude-3-opus".to_string(),
            temperature: None,
            max_tokens: Some(100),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "anthropic",
                ApiProtocol::AnthropicMessages,
                "https://api.anthropic.com",
            ),
        };
        let body = transport.normalize_request(&request_no_temp).unwrap();
        assert!(
            body.get("temperature").is_none(),
            "temperature should be absent when None"
        );
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport.normalize_request sets
    /// stream: true when ChatRequest.stream is true, omits it when false.
    #[test]
    fn normalize_request_stream_flag() {
        let transport = get_anthropic_transport();

        // stream = true
        let request_streaming = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hello".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "claude-3-opus".to_string(),
            temperature: None,
            max_tokens: Some(100),
            stream: true,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "anthropic",
                ApiProtocol::AnthropicMessages,
                "https://api.anthropic.com",
            ),
        };
        let body = transport.normalize_request(&request_streaming).unwrap();
        assert_eq!(body["stream"], true);

        // stream = false
        let request_non_streaming = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hello".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "claude-3-opus".to_string(),
            temperature: None,
            max_tokens: Some(100),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "anthropic",
                ApiProtocol::AnthropicMessages,
                "https://api.anthropic.com",
            ),
        };
        let body = transport.normalize_request(&request_non_streaming).unwrap();
        // When stream is false, the adapter does NOT include the field
        assert!(
            body.get("stream").is_none() || body["stream"] == false,
            "stream should be absent or false when ChatRequest.stream is false"
        );
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport.normalize_request normalizes
    /// tools using AnthropicTransport's normalize_tools (input_schema instead
    /// of parameters).
    #[test]
    fn normalize_request_normalizes_tools_with_input_schema() {
        let transport = get_anthropic_transport();
        let request = ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "Hello".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: sample_tools(),
            model: "claude-3-opus".to_string(),
            temperature: None,
            max_tokens: Some(100),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "anthropic",
                ApiProtocol::AnthropicMessages,
                "https://api.anthropic.com",
            ),
        };

        let body = transport.normalize_request(&request).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "get_weather");
        // Anthropic uses "input_schema" instead of "parameters"
        assert!(
            tools[0].get("input_schema").is_some(),
            "Anthropic tools should use input_schema"
        );
        assert!(
            tools[0].get("parameters").is_none(),
            "Anthropic tools should NOT use parameters"
        );
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport.denormalize_response for
    /// a text-only response. Notable behavior:
    /// - usage is always None (extracted at HTTP layer, not here)
    /// - model is always "" (empty string, not populated by the adapter)
    /// - stop_reason "end_turn" maps to finish_reason "stop"
    #[test]
    fn denormalize_response_text_content() {
        let transport = get_anthropic_transport();
        let response = json!({
            "content": [{"type": "text", "text": "Hello from Claude!"}],
            "stop_reason": "end_turn",
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hello from Claude!"));
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");

        // KEY BEHAVIOR: usage is always None
        assert!(
            result.usage.is_none(),
            "AnthropicDispatchTransport always returns usage: None"
        );
        // KEY BEHAVIOR: model is always empty string
        assert_eq!(
            result.model, "",
            "AnthropicDispatchTransport always returns model: \"\""
        );
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport.denormalize_response for
    /// a tool_use response. The adapter delegates to AnthropicTransport's
    /// denormalize_response which maps stop_reason "tool_use" → "tool_calls"
    /// and serializes tool_use input objects to JSON argument strings.
    #[test]
    fn denormalize_response_tool_use() {
        let transport = get_anthropic_transport();
        let response = json!({
            "content": [
                {"type": "text", "text": "Let me check."},
                {"type": "tool_use", "id": "toolu_01", "name": "get_weather", "input": {"city": "Paris"}},
            ],
            "stop_reason": "tool_use",
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Let me check."));
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].id, "toolu_01");
        assert_eq!(tcs[0].function.name, "get_weather");
        // Arguments should be a JSON string of the input object
        let args: Value = serde_json::from_str(&tcs[0].function.arguments).unwrap();
        assert_eq!(args["city"], "Paris");

        assert_eq!(result.finish_reason, "tool_calls");
        // KEY BEHAVIOR: usage always None, model always ""
        assert!(result.usage.is_none());
        assert_eq!(result.model, "");
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport normalize_request →
    /// denormalize_response roundtrip. Verifies that the adapter preserves
    /// all response fields correctly when used through the Transport trait.
    #[test]
    fn normalize_request_then_denormalize_response_roundtrip() {
        let transport = get_anthropic_transport();

        // Normalize a request with system, user, tools, temperature
        let request = ChatRequest {
            messages: sample_messages(),
            tools: sample_tools(),
            model: "claude-3-opus".to_string(),
            temperature: Some(0.5),
            max_tokens: Some(200),
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: make_resolved(
                "anthropic",
                ApiProtocol::AnthropicMessages,
                "https://api.anthropic.com",
            ),
        };

        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "claude-3-opus");
        assert_eq!(body["system"], "You are a helpful assistant.");
        assert_eq!(body["max_tokens"], 200);
        assert_eq!(body["temperature"], 0.5);
        assert!(body["messages"].is_array());
        assert!(body["tools"].is_array());

        // Denormalize a response that matches the conversation
        let response = json!({
            "content": [
                {"type": "text", "text": "The weather in Tokyo is sunny, 22°C."},
                {"type": "tool_use", "id": "toolu_roundtrip", "name": "get_weather",
                 "input": {"city": "Tokyo"}},
            ],
            "stop_reason": "tool_use",
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(
            result.content.as_deref(),
            Some("The weather in Tokyo is sunny, 22°C.")
        );
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].id, "toolu_roundtrip");
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(result.finish_reason, "tool_calls");

        // Adapter-specific behavior
        assert!(result.usage.is_none());
        assert_eq!(result.model, "");
    }

    /// CHARACTERIZATION: AnthropicDispatchTransport.denormalize_response maps
    /// Anthropic stop_reasons correctly:
    /// - "end_turn" → "stop"
    /// - "tool_use" → "tool_calls"
    /// - "max_tokens" → "length"
    /// - "stop_sequence" → "stop"
    #[test]
    fn denormalize_response_stop_reason_mapping() {
        let transport = get_anthropic_transport();

        // end_turn → stop
        let response = json!({
            "content": [{"type": "text", "text": "Done."}],
            "stop_reason": "end_turn",
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "stop");

        // max_tokens → length
        let response = json!({
            "content": [{"type": "text", "text": "Truncated..."}],
            "stop_reason": "max_tokens",
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "length");

        // stop_sequence → stop
        let response = json!({
            "content": [{"type": "text", "text": "Stopped."}],
            "stop_reason": "stop_sequence",
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "stop");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. ChatCompletionsTransport via dispatcher characterization
// ═══════════════════════════════════════════════════════════════════════════

mod chat_completions_via_dispatcher {
    use super::*;

    /// CHARACTERIZATION: ChatCompletionsTransport obtained via dispatcher
    /// normalizes messages with all roles preserved (system, user, assistant
    /// with tool_calls, tool).
    #[test]
    fn normalize_preserves_all_message_roles() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();
        let request = make_chat_request(sample_messages(), sample_tools());
        let body = transport.normalize_request(&request).unwrap();

        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[3]["role"], "tool");
    }

    /// CHARACTERIZATION: ChatCompletionsTransport obtained via dispatcher
    /// denormalizes a text response with model, usage, and finish_reason.
    #[test]
    fn denormalize_text_response_with_usage_and_model() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();

        let response = json!({
            "id": "chatcmpl-char-test",
            "model": "gpt-4o",
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hello!"));
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
        assert_eq!(result.model, "gpt-4o");
        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    /// CHARACTERIZATION: ChatCompletionsTransport via dispatcher roundtrip.
    #[test]
    fn roundtrip_normalize_then_denormalize() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();

        let request = make_chat_request(sample_messages(), sample_tools());
        let body = transport.normalize_request(&request).unwrap();
        assert_eq!(body["model"], "test-model");

        let response = json!({
            "id": "chatcmpl-char-roundtrip",
            "model": "test-model",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Weather: sunny, 22°C.",
                    "tool_calls": [{
                        "id": "call_char1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\": \"Tokyo\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 30, "completion_tokens": 10, "total_tokens": 40}
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Weather: sunny, 22°C."));
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(result.finish_reason, "tool_calls");
        assert_eq!(result.model, "test-model");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. GeminiTransport via dispatcher characterization
// ═══════════════════════════════════════════════════════════════════════════

mod gemini_via_dispatcher {
    use super::*;

    /// CHARACTERIZATION: GeminiTransport obtained via dispatcher extracts
    /// system messages into systemInstruction and converts roles
    /// (assistant → model, user stays user).
    #[test]
    fn normalize_extracts_system_and_maps_roles() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .unwrap();

        let request = make_chat_request(sample_messages(), vec![]);
        let body = transport.normalize_request(&request).unwrap();

        // System in systemInstruction
        assert!(body.get("systemInstruction").is_some());
        let sys = &body["systemInstruction"];
        assert_eq!(sys["parts"][0]["text"], "You are a helpful assistant.");

        // No "system" role in contents
        let contents = body["contents"].as_array().unwrap();
        for entry in contents {
            assert_ne!(entry["role"], "system");
        }

        // assistant → model role mapping
        let has_model = contents.iter().any(|c| c["role"] == "model");
        assert!(has_model, "assistant messages should map to 'model' role");
    }

    /// CHARACTERIZATION: GeminiTransport via dispatcher denormalizes a text
    /// response with usage metadata.
    #[test]
    fn denormalize_text_response_with_usage() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .unwrap();

        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello from Gemini!"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 20,
                "candidatesTokenCount": 8,
                "totalTokenCount": 28
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("Hello from Gemini!"));
        assert!(result.tool_calls.is_none());
        assert_eq!(result.finish_reason, "stop");
        let usage = result.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 20);
        assert_eq!(usage.completion_tokens, 8);
        assert_eq!(usage.total_tokens, 28);
    }

    /// CHARACTERIZATION: GeminiTransport via dispatcher roundtrip with tools.
    #[test]
    fn roundtrip_with_tools() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .unwrap();

        let request = make_chat_request(sample_messages(), sample_tools());
        let body = transport.normalize_request(&request).unwrap();

        assert!(body["contents"].is_array());
        assert!(body.get("systemInstruction").is_some());
        assert!(body["tools"].is_array());

        let response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "The weather is sunny."},
                        {"functionCall": {"name": "get_weather", "args": {"city": "Tokyo"}}}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 40,
                "candidatesTokenCount": 15,
                "totalTokenCount": 55
            }
        });

        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.content.as_deref(), Some("The weather is sunny."));
        let tcs = result.tool_calls.unwrap();
        assert_eq!(tcs[0].function.name, "get_weather");
        assert_eq!(result.finish_reason, "tool_calls");
    }

    /// CHARACTERIZATION: GeminiTransport maps finish reasons:
    /// STOP → stop, MAX_TOKENS → length, SAFETY → content_filter.
    #[test]
    fn finish_reason_mapping() {
        let dispatcher = TransportDispatcher::new();
        let transport = dispatcher
            .dispatch(&ApiProtocol::GeminiGenerateContent)
            .unwrap();

        // STOP → stop
        let response = json!({
            "candidates": [{
                "content": {"parts": [{"text": "Done"}], "role": "model"},
                "finishReason": "STOP"
            }]
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "stop");

        // MAX_TOKENS → length
        let response = json!({
            "candidates": [{
                "content": {"parts": [{"text": "Truncated"}], "role": "model"},
                "finishReason": "MAX_TOKENS"
            }]
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "length");

        // SAFETY → content_filter
        let response = json!({
            "candidates": [{
                "content": {"parts": [{"text": "Filtered"}], "role": "model"},
                "finishReason": "SAFETY"
            }]
        });
        let result = transport.denormalize_response(&response).unwrap();
        assert_eq!(result.finish_reason, "content_filter");
    }
}
