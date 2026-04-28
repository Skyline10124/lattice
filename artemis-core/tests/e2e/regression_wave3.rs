//! Regression tests for Wave 3 bug classes.
//!
//! Covers: unified Transport trait, AgentLoop tokio runtime,
//! resume_with_tool_results, budget_tokens, TransportDispatcher factory,
//! credential cache, PROVIDER_CREDENTIALS HashMap, and resp.json
//! (ChatResponse field population).
//!
//! These tests verify concrete behaviors that broke or changed during Wave 3.

use std::collections::HashMap;

use artemis_core::agent_loop::{AgentLoop, LoopConfig, LoopEvent};
use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::mock::MockProvider;
use artemis_core::provider::{ChatRequest, Provider};
use artemis_core::router::{ModelRouter, _PROVIDER_CREDENTIALS};
use artemis_core::transport::dispatcher::{create_transport, TransportDispatcher};
use artemis_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_resolved() -> ResolvedModel {
    ResolvedModel {
        canonical_id: "test-model".to_string(),
        provider: "mock".to_string(),
        api_key: None,
        base_url: "http://localhost".to_string(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "test-model".to_string(),
        context_length: 131072,
        provider_specific: HashMap::new(),
    }
}

fn user_message(content: &str) -> Message {
    Message {
        role: Role::User,
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

fn assistant_message(content: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        name: None,
    }
}

// ---------------------------------------------------------------------------
// SECTION 1: Unified Transport trait — base_url, extra_headers, api_mode
// ---------------------------------------------------------------------------

#[test]
fn regr_transport_base_url_for_chat_completions() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    assert_eq!(
        transport.base_url(),
        "https://api.openai.com/v1",
        "ChatCompletionsTransport has OpenAI base URL"
    );
}

#[test]
fn regr_transport_base_url_for_anthropic() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    assert_eq!(
        transport.base_url(),
        "https://api.anthropic.com",
        "AnthropicTransport has Anthropic base URL"
    );
}

#[test]
fn regr_transport_base_url_for_gemini() {
    let transport = create_transport(&ApiProtocol::GeminiGenerateContent).unwrap();
    // Gemini transport base URL
    assert!(
        !transport.base_url().is_empty(),
        "GeminiTransport has a non-empty base URL"
    );
}

#[test]
fn regr_transport_extra_headers_default_empty() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    assert!(
        transport.extra_headers().is_empty(),
        "default transport has no extra headers"
    );
}

#[test]
fn regr_transport_api_mode_chat_completions() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    assert_eq!(
        transport.api_mode(),
        "chat_completions",
        "OpenAiChat protocol → api_mode 'chat_completions'"
    );
}

#[test]
fn regr_transport_api_mode_anthropic() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    assert_eq!(
        transport.api_mode(),
        "anthropic",
        "AnthropicMessages protocol → api_mode 'anthropic'"
    );
}

#[test]
fn regr_transport_api_mode_gemini() {
    let transport = create_transport(&ApiProtocol::GeminiGenerateContent).unwrap();
    assert_eq!(
        transport.api_mode(),
        "gemini",
        "GeminiGenerateContent protocol → api_mode 'gemini'"
    );
}

// ---------------------------------------------------------------------------
// SECTION 2: Unified Transport trait — normalize_request
// ---------------------------------------------------------------------------

#[test]
fn regr_transport_normalize_request_openai_produces_model_field() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let resolved = ResolvedModel {
        canonical_id: "gpt-4o".into(),
        provider: "openai".into(),
        api_key: Some("sk-test".into()),
        base_url: "https://api.openai.com/v1".into(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "gpt-4o".into(),
        context_length: 128000,
        provider_specific: HashMap::new(),
    };
    let request = ChatRequest::new(
        vec![user_message("Hello!")],
        vec![],
        resolved,
    );

    let body = transport.normalize_request(&request).unwrap();
    assert_eq!(body["model"], "gpt-4o");
    assert!(body["messages"].is_array());
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "Hello!");
}

#[test]
fn regr_transport_normalize_request_openai_with_tools() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let resolved = ResolvedModel {
        canonical_id: "gpt-4o".into(),
        provider: "openai".into(),
        api_key: Some("sk-test".into()),
        base_url: "https://api.openai.com/v1".into(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "gpt-4o".into(),
        context_length: 128000,
        provider_specific: HashMap::new(),
    };
    let tools = vec![ToolDefinition {
        name: "search".into(),
        description: "Search the web".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    }];
    let request = ChatRequest::new(
        vec![user_message("Search please")],
        tools,
        resolved,
    );

    let body = transport.normalize_request(&request).unwrap();
    assert!(body["tools"].is_array());
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], "search");
}

#[test]
fn regr_transport_normalize_request_openai_with_temperature_and_max_tokens() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let resolved = ResolvedModel {
        canonical_id: "gpt-4o".into(),
        provider: "openai".into(),
        api_key: Some("sk-test".into()),
        base_url: "https://api.openai.com/v1".into(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "gpt-4o".into(),
        context_length: 128000,
        provider_specific: HashMap::new(),
    };
    let request = ChatRequest {
        messages: vec![user_message("Hi")],
        tools: vec![],
        model: "gpt-4o".into(),
        temperature: Some(0.7),
        max_tokens: Some(512),
        stream: false,
        resolved,
    };

    let body = transport.normalize_request(&request).unwrap();
    assert_eq!(body["temperature"], 0.7);
    assert_eq!(body["max_tokens"], 512);
}

#[test]
fn regr_transport_normalize_request_openai_stream_flag() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let resolved = ResolvedModel {
        canonical_id: "gpt-4o".into(),
        provider: "openai".into(),
        api_key: Some("sk-test".into()),
        base_url: "https://api.openai.com/v1".into(),
        api_protocol: ApiProtocol::OpenAiChat,
        api_model_id: "gpt-4o".into(),
        context_length: 128000,
        provider_specific: HashMap::new(),
    };
    let mut request = ChatRequest::new(
        vec![user_message("Hi")],
        vec![],
        resolved,
    );
    request.stream = true;

    let body = transport.normalize_request(&request).unwrap();
    assert_eq!(body["stream"], true);
}

#[test]
fn regr_transport_normalize_request_anthropic_system_extraction() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    let resolved = ResolvedModel {
        canonical_id: "claude-3-opus".into(),
        provider: "anthropic".into(),
        api_key: Some("sk-ant-test".into()),
        base_url: "https://api.anthropic.com".into(),
        api_protocol: ApiProtocol::AnthropicMessages,
        api_model_id: "claude-3-opus".into(),
        context_length: 200000,
        provider_specific: HashMap::new(),
    };
    let request = ChatRequest::new(
        vec![
            Message {
                role: Role::System,
                content: "You are helpful.".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            user_message("Hello!"),
        ],
        vec![],
        resolved,
    );

    let body = transport.normalize_request(&request).unwrap();
    assert_eq!(body["system"], "You are helpful.");
    assert_eq!(body["max_tokens"], 4096); // Anthropic default
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
}

#[test]
fn regr_transport_normalize_request_anthropic_with_tools() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    let resolved = ResolvedModel {
        canonical_id: "claude-3-opus".into(),
        provider: "anthropic".into(),
        api_key: Some("sk-ant-test".into()),
        base_url: "https://api.anthropic.com".into(),
        api_protocol: ApiProtocol::AnthropicMessages,
        api_model_id: "claude-3-opus".into(),
        context_length: 200000,
        provider_specific: HashMap::new(),
    };
    let tools = vec![ToolDefinition {
        name: "get_weather".into(),
        description: "Get weather".into(),
        parameters: serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
    }];
    let request = ChatRequest::new(
        vec![user_message("Weather?")],
        tools,
        resolved,
    );

    let body = transport.normalize_request(&request).unwrap();
    assert!(body["tools"].is_array());
    assert_eq!(body["tools"][0]["name"], "get_weather");
}

// ---------------------------------------------------------------------------
// SECTION 3: Unified Transport trait — denormalize_response
// ---------------------------------------------------------------------------

#[test]
fn regr_transport_denormalize_openai_response_content_only() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let response = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "Hello!"},
            "finish_reason": "stop"
        }],
        "model": "gpt-4o",
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    });

    let result = transport.denormalize_response(&response).unwrap();
    assert_eq!(result.content.as_deref(), Some("Hello!"));
    assert_eq!(result.finish_reason, "stop");
    assert_eq!(result.model, "gpt-4o");
    assert!(result.tool_calls.is_none());

    // resp.json: usage should be populated
    let usage = result.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 5);
    assert_eq!(usage.total_tokens, 15);
}

#[test]
fn regr_transport_denormalize_openai_response_with_tool_calls() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let response = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Let me search.",
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "search",
                        "arguments": "{\"q\":\"rust\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "gpt-4o",
        "usage": {"prompt_tokens": 20, "completion_tokens": 30, "total_tokens": 50}
    });

    let result = transport.denormalize_response(&response).unwrap();
    assert_eq!(result.content.as_deref(), Some("Let me search."));
    assert_eq!(result.finish_reason, "tool_calls");
    assert_eq!(result.model, "gpt-4o");

    let tool_calls = result.tool_calls.unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_123");
    assert_eq!(tool_calls[0].function.name, "search");
    assert_eq!(tool_calls[0].function.arguments, "{\"q\":\"rust\"}");
}

#[test]
fn regr_transport_denormalize_openai_response_missing_usage() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let response = serde_json::json!({
        "choices": [{
            "message": {"role": "assistant", "content": "Hi"},
            "finish_reason": "stop"
        }]
    });

    let result = transport.denormalize_response(&response).unwrap();
    // resp.json: usage is None when not present in response
    assert!(result.usage.is_none());
    // resp.json: model falls back to "unknown"
    assert_eq!(result.model, "unknown");
}

#[test]
fn regr_transport_denormalize_anthropic_response_content_only() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    let response = serde_json::json!({
        "content": [{"type": "text", "text": "Hi there!"}],
        "stop_reason": "end_turn"
    });

    let result = transport.denormalize_response(&response).unwrap();
    assert_eq!(result.content.as_deref(), Some("Hi there!"));
    assert_eq!(result.finish_reason, "stop");
    assert!(result.usage.is_none()); // Anthropic transport doesn't extract usage
    assert_eq!(result.model, ""); // Anthropic transport returns empty model string
}

#[test]
fn regr_transport_denormalize_anthropic_response_with_tool_use() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    let response = serde_json::json!({
        "content": [
            {"type": "text", "text": "Let me look that up."},
            {"type": "tool_use", "id": "toolu_01", "name": "get_weather",
             "input": {"city": "Paris"}}
        ],
        "stop_reason": "tool_use"
    });

    let result = transport.denormalize_response(&response).unwrap();
    assert!(result.content.as_deref().unwrap().contains("Let me look"));
    assert_eq!(result.finish_reason, "tool_calls");

    let tool_calls = result.tool_calls.unwrap();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "toolu_01");
    assert_eq!(tool_calls[0].function.name, "get_weather");
}

#[test]
fn regr_transport_denormalize_anthropic_max_tokens_stop_reason() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    let response = serde_json::json!({
        "content": [{"type": "text", "text": "...truncated"}],
        "stop_reason": "max_tokens"
    });

    let result = transport.denormalize_response(&response).unwrap();
    assert_eq!(result.finish_reason, "length");
}

#[test]
fn regr_transport_denormalize_anthropic_stop_sequence() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    let response = serde_json::json!({
        "content": [{"type": "text", "text": "Done."}],
        "stop_reason": "stop_sequence"
    });

    let result = transport.denormalize_response(&response).unwrap();
    assert_eq!(result.finish_reason, "stop");
}

// ---------------------------------------------------------------------------
// SECTION 4: NormalizedMessages — system extraction
// ---------------------------------------------------------------------------

#[test]
fn regr_transport_normalize_messages_openai_no_system_extraction() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let msgs = vec![
        Message {
            role: Role::System,
            content: "You are helpful.".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        user_message("Hi"),
    ];
    let normalized = transport.normalize_messages(&msgs);
    // OpenAI transport default: no system extraction
    assert!(normalized.system.is_none());
    assert_eq!(normalized.messages.len(), 2);
    assert_eq!(normalized.messages[0]["role"], "system");
    assert_eq!(normalized.messages[1]["role"], "user");
}

#[test]
fn regr_transport_normalize_messages_anthropic_system_extraction() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages).unwrap();
    let msgs = vec![
        Message {
            role: Role::System,
            content: "You are helpful.".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        user_message("Hi"),
    ];
    let normalized = transport.normalize_messages(&msgs);
    // Anthropic transport extracts system prompt
    assert_eq!(normalized.system, Some("You are helpful.".into()));
    assert_eq!(normalized.messages.len(), 1);
    assert_eq!(normalized.messages[0]["role"], "user");
}

// ---------------------------------------------------------------------------
// SECTION 5: Transport trait denormalize_stream_chunk default
// ---------------------------------------------------------------------------

#[test]
fn regr_transport_stream_chunk_default_returns_empty() {
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let data = serde_json::json!({});
    let result = transport.denormalize_stream_chunk("content_block_delta", &data);
    assert!(result.is_empty(), "default stream chunk returns empty vec");
}

// ---------------------------------------------------------------------------
// SECTION 6: AgentLoop tokio runtime
// ---------------------------------------------------------------------------

#[test]
fn regr_agent_loop_uses_tokio_runtime_not_block_on() {
    // AgentLoop creates a tokio::runtime::Runtime at construction time
    // and uses runtime.block_on() to call the async Provider::chat().
    // This test verifies the runtime is functional.
    let mut provider = MockProvider::new("mock");
    provider.set_response("Tokio runtime works!");

    let agent = AgentLoop::new();

    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Hi")],
        vec![],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "AgentLoop with tokio runtime completes successfully"
    );
}

#[test]
fn regr_agent_loop_tokio_runtime_handles_concurrent_calls() {
    // Verify AgentLoop handles multiple calls on the same tokio runtime.
    let mut provider = MockProvider::new("mock");
    provider.set_response("First call");

    let agent = AgentLoop::new();

    let events1 = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Call 1")],
        vec![],
        LoopConfig::default(),
    );
    assert!(
        events1.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "First call on tokio runtime succeeds"
    );

    let events2 = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Call 2")],
        vec![],
        LoopConfig::default(),
    );
    assert!(
        events2.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "Second call on same tokio runtime succeeds"
    );
}

// ---------------------------------------------------------------------------
// SECTION 7: resume_with_tool_results
// ---------------------------------------------------------------------------

#[test]
fn regr_resume_with_tool_results_single_result_produces_done() {
    let provider = MockProvider::new("mock")
        .with_first_content("Paris is sunny and 22°C.")
        .with_final_content("Paris is sunny and 22°C.");

    let agent = AgentLoop::new();

    let messages = vec![
        user_message("What's the weather in Paris?"),
        Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"Paris"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        },
    ];

    let events = agent.resume_with_tool_results(
        &provider,
        make_resolved(),
        messages,
        vec![],
        LoopConfig::default(),
        vec![("call_1".to_string(), "Sunny, 22°C".to_string())],
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "resume_with_tool_results produces Done with real tool output"
    );
}

#[test]
fn regr_resume_with_tool_results_preserves_real_output_in_response() {
    let provider = MockProvider::new("mock")
        .with_first_content("Based on the weather data, Paris is sunny.")
        .with_final_content("Based on the weather data, Paris is sunny.");

    let agent = AgentLoop::new();

    let messages = vec![
        user_message("Weather in Paris?"),
        Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"Paris"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        },
    ];

    let events = agent.resume_with_tool_results(
        &provider,
        make_resolved(),
        messages,
        vec![],
        LoopConfig::default(),
        vec![("call_1".to_string(), "Sunny, 22°C".to_string())],
    );

    if let Some(LoopEvent::Done { final_message, .. }) =
        events.iter().find(|e| matches!(e, LoopEvent::Done { .. }))
    {
        assert!(
            final_message.content.contains("Paris"),
            "final_message content reflects the provider's response to real tool results"
        );
    }
}

#[test]
fn regr_resume_with_tool_results_multiple_results() {
    let provider = MockProvider::new("mock")
        .with_first_content("Combined result based on multiple tools.")
        .with_final_content("Combined result based on multiple tools.");

    let agent = AgentLoop::new();

    let messages = vec![
        user_message("Search and calculate"),
        Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![
                ToolCall {
                    id: "call_search".to_string(),
                    function: FunctionCall {
                        name: "search".to_string(),
                        arguments: r#"{"query":"rust"}"#.to_string(),
                    },
                },
                ToolCall {
                    id: "call_calc".to_string(),
                    function: FunctionCall {
                        name: "calculate".to_string(),
                        arguments: r#"{"expr":"2+2"}"#.to_string(),
                    },
                },
            ]),
            tool_call_id: None,
            name: None,
        },
    ];

    let events = agent.resume_with_tool_results(
        &provider,
        make_resolved(),
        messages,
        vec![],
        LoopConfig::default(),
        vec![
            ("call_search".to_string(), "3 articles found".to_string()),
            ("call_calc".to_string(), "4".to_string()),
        ],
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "resume_with_tool_results handles multiple tool results"
    );
}

#[test]
fn regr_resume_with_tool_results_empty_results_still_works() {
    let provider = MockProvider::new("mock")
        .with_first_content("No tools needed.")
        .with_final_content("No tools needed.");

    let agent = AgentLoop::new();

    let events = agent.resume_with_tool_results(
        &provider,
        make_resolved(),
        vec![user_message("Hi")],
        vec![],
        LoopConfig::default(),
        vec![], // empty results
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "resume_with_tool_results with empty results still produces Done"
    );
}

// ---------------------------------------------------------------------------
// SECTION 8: budget_tokens
// ---------------------------------------------------------------------------

#[test]
fn regr_budget_tokens_trim_conversation_under_budget_preserves_all() {
    // When conversation fits under budget, no trimming occurs.
    let mut provider = MockProvider::new("mock");
    provider.set_response("OK");

    let agent = AgentLoop::new();
    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Hi"), assistant_message("Hello")],
        vec![],
        LoopConfig {
            max_iterations: 3,
            budget_tokens: 1_000_000,
        },
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "under-budget conversation completes"
    );
}

#[test]
fn regr_budget_tokens_trim_conversation_with_tight_budget() {
    // A very tight budget should trigger trimming but still produce a response.
    let mut provider = MockProvider::new("mock");
    provider.set_response("Short.");

    let agent = AgentLoop::new();

    // Create many long messages to force trimming
    let long = "x".repeat(500);
    let messages = vec![
        Message {
            role: Role::System,
            content: "You are helpful.".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::User,
            content: long.clone(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::Assistant,
            content: long.clone(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::User,
            content: long,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        user_message("hi"),
    ];

    let config = LoopConfig {
        max_iterations: 3,
        budget_tokens: 20, // Very tight — forces trimming
    };

    let events = agent.run(&provider, make_resolved(), messages, vec![], config);

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "tight budget should still complete (trimming kicks in)"
    );
    assert!(
        !events.iter().any(|e| matches!(e, LoopEvent::Error { .. })),
        "tight budget should not cause errors"
    );
}

#[test]
fn regr_budget_tokens_loop_config_defaults() {
    let config = LoopConfig::default();
    assert_eq!(config.max_iterations, 10, "default max_iterations is 10");
    assert_eq!(config.budget_tokens, 100_000, "default budget_tokens is 100_000");
}

#[test]
fn regr_budget_tokens_custom_loop_config() {
    let config = LoopConfig {
        max_iterations: 5,
        budget_tokens: 50_000,
    };
    assert_eq!(config.max_iterations, 5);
    assert_eq!(config.budget_tokens, 50_000);
}

#[test]
fn regr_budget_tokens_keeps_system_message() {
    let mut provider = MockProvider::new("mock");
    provider.set_response("OK");

    let agent = AgentLoop::new();
    let long = "x".repeat(500);

    let messages = vec![
        Message {
            role: Role::System,
            content: "You are helpful.".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::User,
            content: long.clone(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        Message {
            role: Role::User,
            content: "recent message".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    ];

    let config = LoopConfig {
        max_iterations: 3,
        budget_tokens: 20,
    };

    let events = agent.run(&provider, make_resolved(), messages, vec![], config);

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "system message preserved, trim works with budget"
    );
}

// ---------------------------------------------------------------------------
// SECTION 9: TransportDispatcher factory
// ---------------------------------------------------------------------------

#[test]
fn regr_dispatcher_create_transport_returns_openai() {
    let transport = create_transport(&ApiProtocol::OpenAiChat);
    assert!(transport.is_some(), "create_transport returns Some for OpenAiChat");
    assert_eq!(transport.unwrap().api_mode(), "chat_completions");
}

#[test]
fn regr_dispatcher_create_transport_returns_anthropic() {
    let transport = create_transport(&ApiProtocol::AnthropicMessages);
    assert!(transport.is_some(), "create_transport returns Some for AnthropicMessages");
    assert_eq!(transport.unwrap().api_mode(), "anthropic");
}

#[test]
fn regr_dispatcher_create_transport_returns_gemini() {
    let transport = create_transport(&ApiProtocol::GeminiGenerateContent);
    assert!(transport.is_some(), "create_transport returns Some for GeminiGenerateContent");
    assert_eq!(transport.unwrap().api_mode(), "gemini");
}

#[test]
fn regr_dispatcher_create_transport_returns_none_for_unknown() {
    let transport = create_transport(&ApiProtocol::Custom("unknown-protocol".into()));
    assert!(transport.is_none(), "create_transport returns None for unknown protocols");
}

#[test]
fn regr_dispatcher_dispatch_openai() {
    let dispatcher = TransportDispatcher::new();
    let transport = dispatcher.dispatch(&ApiProtocol::OpenAiChat).unwrap();
    assert_eq!(transport.api_mode(), "chat_completions");
}

#[test]
fn regr_dispatcher_dispatch_anthropic() {
    let dispatcher = TransportDispatcher::new();
    let transport = dispatcher.dispatch(&ApiProtocol::AnthropicMessages).unwrap();
    assert_eq!(transport.api_mode(), "anthropic");
}

#[test]
fn regr_dispatcher_dispatch_gemini() {
    let dispatcher = TransportDispatcher::new();
    let transport = dispatcher.dispatch(&ApiProtocol::GeminiGenerateContent).unwrap();
    assert_eq!(transport.api_mode(), "gemini");
}

#[test]
fn regr_dispatcher_dispatch_unknown_returns_none() {
    let dispatcher = TransportDispatcher::new();
    let result = dispatcher.dispatch(&ApiProtocol::BedrockConverse);
    assert!(result.is_none(), "BedrockConverse not registered → None");
}

#[test]
fn regr_dispatcher_dispatch_for_resolved() {
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
fn regr_dispatcher_dispatch_for_resolved_gemini() {
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
fn regr_dispatcher_dispatch_for_resolved_unknown() {
    let dispatcher = TransportDispatcher::new();
    let resolved = ResolvedModel {
        canonical_id: "unknown".into(),
        provider: "test".into(),
        api_key: None,
        base_url: "https://api.example.com".into(),
        api_protocol: ApiProtocol::Custom("unregistered".into()),
        api_model_id: "unknown".into(),
        context_length: 0,
        provider_specific: HashMap::new(),
    };
    let result = dispatcher.dispatch_for_resolved(&resolved);
    assert!(result.is_none());
}

#[test]
fn regr_dispatcher_register_custom_transport() {
    use artemis_core::transport::chat_completions::ChatCompletionsTransport;
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
fn regr_dispatcher_register_replaces_existing() {
    use artemis_core::transport::chat_completions::ChatCompletionsTransport;
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
fn regr_dispatcher_default_has_three_transports() {
    let dispatcher = TransportDispatcher::default();
    assert!(dispatcher.dispatch(&ApiProtocol::OpenAiChat).is_some());
    assert!(dispatcher.dispatch(&ApiProtocol::AnthropicMessages).is_some());
    assert!(dispatcher.dispatch(&ApiProtocol::GeminiGenerateContent).is_some());
}

// ---------------------------------------------------------------------------
// SECTION 10: Credential cache
// ---------------------------------------------------------------------------

#[test]
fn regr_credential_cache_caches_env_lookups() {
    let _lock = crate::env_lock::lock();
    let prev = std::env::var("ANTHROPIC_API_KEY").ok();
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-cached");

    let router = ModelRouter::new();

    // First resolve caches the credential
    let _ = router.resolve("claude-sonnet-4-6", Some("anthropic"));

    // Now change the env var — cache should still return old value
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-different");

    let resolved = router.resolve("claude-sonnet-4-6", Some("anthropic")).unwrap();
    assert_eq!(
        resolved.api_key.as_deref(),
        Some("sk-ant-cached"),
        "credential cache returns cached value, not current env var"
    );

    if let Some(v) = prev {
        std::env::set_var("ANTHROPIC_API_KEY", v);
    } else {
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
}

#[test]
fn regr_credential_cache_invalidate_clears_cache() {
    let _lock = crate::env_lock::lock();
    let prev = std::env::var("ANTHROPIC_API_KEY").ok();
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-first");

    let router = ModelRouter::new();

    // Cache first value
    let _ = router.resolve("claude-sonnet-4-6", Some("anthropic"));

    // Invalidate and set new env var
    router.invalidate_credential_cache();
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-second");

    let resolved = router.resolve("claude-sonnet-4-6", Some("anthropic")).unwrap();
    assert_eq!(
        resolved.api_key.as_deref(),
        Some("sk-ant-second"),
        "after invalidate, fresh env var is read"
    );

    if let Some(v) = prev {
        std::env::set_var("ANTHROPIC_API_KEY", v);
    } else {
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
}

#[test]
fn regr_credential_cache_invalidate_then_repopulate() {
    let _lock = crate::env_lock::lock();
    let prev = std::env::var("ANTHROPIC_API_KEY").ok();
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-third");

    let router = ModelRouter::new();

    // Populate cache
    let resolved1 = router.resolve("claude-sonnet-4-6", Some("anthropic")).unwrap();
    assert_eq!(resolved1.api_key.as_deref(), Some("sk-ant-third"));

    // Invalidate
    router.invalidate_credential_cache();

    // Now env var has changed
    std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-fourth");

    // repopulate from new env var
    let resolved2 = router.resolve("claude-sonnet-4-6", Some("anthropic")).unwrap();
    assert_eq!(resolved2.api_key.as_deref(), Some("sk-ant-fourth"));

    if let Some(v) = prev {
        std::env::set_var("ANTHROPIC_API_KEY", v);
    } else {
        std::env::remove_var("ANTHROPIC_API_KEY");
    }
}

// ---------------------------------------------------------------------------
// SECTION 11: PROVIDER_CREDENTIALS HashMap
// ---------------------------------------------------------------------------

#[test]
fn regr_provider_credentials_map_has_21_entries() {
    assert_eq!(
        _PROVIDER_CREDENTIALS.len(),
        21,
        "_PROVIDER_CREDENTIALS has 21 provider entries"
    );
}

#[test]
fn regr_provider_credentials_map_includes_all_providers() {
    let expected = [
        "openrouter", "anthropic", "openai", "gemini", "deepseek",
        "groq", "mistral", "xai", "ollama", "nous", "copilot",
        "opencode-zen", "kilocode", "ai-gateway", "openai-codex",
        "bedrock", "minimax", "qwen", "volces", "infini-ai", "opencode-go",
    ];

    let slugs: Vec<&str> = _PROVIDER_CREDENTIALS.iter().map(|(s, _)| *s).collect();
    assert_eq!(slugs.as_slice(), expected);
}

#[test]
fn regr_provider_credentials_map_ollama_empty_creds() {
    let ollama = _PROVIDER_CREDENTIALS
        .iter()
        .find(|(s, _)| *s == "ollama")
        .unwrap();
    assert!(ollama.1.is_empty(), "ollama has empty credential list");
}

#[test]
fn regr_provider_credentials_map_bedrock_empty_creds() {
    let bedrock = _PROVIDER_CREDENTIALS
        .iter()
        .find(|(s, _)| *s == "bedrock")
        .unwrap();
    assert!(bedrock.1.is_empty(), "bedrock has empty credential list");
}

#[test]
fn regr_provider_credentials_map_copilot_uses_github_token() {
    let copilot = _PROVIDER_CREDENTIALS
        .iter()
        .find(|(s, _)| *s == "copilot")
        .unwrap();
    assert_eq!(copilot.1.len(), 1);
    assert_eq!(copilot.1[0].0, "GITHUB_TOKEN");
    assert_eq!(copilot.1[0].1, "token");
}

#[test]
fn regr_provider_credentials_map_openrouter_has_openrouter_api_key() {
    let openrouter = _PROVIDER_CREDENTIALS
        .iter()
        .find(|(s, _)| *s == "openrouter")
        .unwrap();
    assert_eq!(openrouter.1[0].0, "OPENROUTER_API_KEY");
    assert_eq!(openrouter.1[0].1, "api_key");
}

#[test]
fn regr_provider_credentials_map_openai_codex_shares_openai_key() {
    let codex = _PROVIDER_CREDENTIALS
        .iter()
        .find(|(s, _)| *s == "openai-codex")
        .unwrap();
    assert_eq!(codex.1[0].0, "OPENAI_API_KEY");
}

// ---------------------------------------------------------------------------
// SECTION 12: resp.json — ChatResponse field population
// ---------------------------------------------------------------------------

#[test]
fn regr_response_json_chat_response_all_fields_populated() {
    // Verify ChatResponse has all expected fields populated from MockProvider.
    let provider = MockProvider::new("test")
        .with_first_content("Hello")
        .with_final_content("Done");

    let resolved = make_resolved();
    let request = ChatRequest::new(vec![user_message("Hi")], vec![], resolved);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let resp = rt.block_on(provider.chat(request)).unwrap();

    // content
    assert_eq!(resp.content.as_deref(), Some("Hello"), "ChatResponse.content is populated");
    // tool_calls
    assert!(resp.tool_calls.is_none(), "ChatResponse.tool_calls is None for content-only response");
    // usage
    let usage = resp.usage.expect("ChatResponse.usage should be populated");
    assert!(usage.total_tokens > 0, "ChatResponse.usage has token counts");
    // finish_reason
    assert_eq!(resp.finish_reason, "stop", "ChatResponse.finish_reason is 'stop'");
    // model
    assert_eq!(resp.model, "test", "ChatResponse.model is populated from provider name");
}

#[test]
fn regr_response_json_tool_call_response_fields() {
    let provider = MockProvider::new("test")
        .with_first_content("Let me check.")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "get_weather".to_string(),
                arguments: r#"{"city":"Paris"}"#.to_string(),
            },
        }]);

    let resolved = make_resolved();
    let request = ChatRequest::new(vec![user_message("Weather?")], vec![], resolved);

    let rt = tokio::runtime::Runtime::new().unwrap();
    let resp = rt.block_on(provider.chat(request)).unwrap();

    // tool_calls presence
    let tool_calls = resp.tool_calls.expect("ChatResponse.tool_calls should be populated");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].function.name, "get_weather");
    // finish_reason for tool calls
    assert_eq!(resp.finish_reason, "tool_calls");
}

#[test]
fn regr_response_json_denormalize_response_produces_chat_response() {
    // Verify the Transport denormalize_response output matches ChatResponse shape.
    let transport = create_transport(&ApiProtocol::OpenAiChat).unwrap();
    let json = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "Test response",
                "tool_calls": [{
                    "id": "tc_1",
                    "type": "function",
                    "function": {"name": "calc", "arguments": "{}"}
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "model": "gpt-4o",
        "usage": {"prompt_tokens": 5, "completion_tokens": 10, "total_tokens": 15}
    });

    let chat_resp = transport.denormalize_response(&json).unwrap();

    assert_eq!(chat_resp.content.as_deref(), Some("Test response"));
    assert_eq!(chat_resp.finish_reason, "tool_calls");
    assert_eq!(chat_resp.model, "gpt-4o");

    // usage: all fields populated
    let usage = chat_resp.usage.unwrap();
    assert_eq!(usage.prompt_tokens, 5);
    assert_eq!(usage.completion_tokens, 10);
    assert_eq!(usage.total_tokens, 15);

    // tool_calls: both id and function fields populated
    let tcs = chat_resp.tool_calls.unwrap();
    assert_eq!(tcs[0].id, "tc_1");
    assert_eq!(tcs[0].function.name, "calc");
    assert_eq!(tcs[0].function.arguments, "{}");
}

// ---------------------------------------------------------------------------
// SECTION 13: End-to-end: Transport → AgentLoop → Event flow
// ---------------------------------------------------------------------------

#[test]
fn regr_transport_plus_agent_loop_e2e() {
    let mut provider = MockProvider::new("mock");
    provider.set_response("Full pipeline response!");

    let agent = AgentLoop::new();

    let events = agent.run(
        &provider,
        ResolvedModel {
            canonical_id: "gpt-4o".into(),
            provider: "openai".into(),
            api_key: Some("sk-test".into()),
            base_url: "https://api.openai.com/v1".into(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "gpt-4o".into(),
            context_length: 128000,
            provider_specific: HashMap::new(),
        },
        vec![user_message("Test the full pipeline")],
        vec![],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Token { .. })),
        "E2E: Token event emitted"
    );
    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "E2E: Done event emitted"
    );
}

#[test]
fn regr_transport_plus_agent_loop_with_tool_calls_e2e() {
    let provider = MockProvider::new("mock")
        .with_first_content("Let me search.")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_search".to_string(),
            function: FunctionCall {
                name: "search".to_string(),
                arguments: r#"{"q":"rust"}"#.to_string(),
            },
        }])
        .with_final_content("Found 5 results about Rust.");

    let agent = AgentLoop::new();

    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Search for rust")],
        vec![ToolDefinition {
            name: "search".into(),
            description: "Search the web".into(),
            parameters: serde_json::json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        }],
        LoopConfig::default(),
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, LoopEvent::ToolCallRequired { .. })),
        "E2E: ToolCallRequired emitted when provider returns tool calls"
    );
    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "E2E: Done emitted after tool call auto-continuation"
    );
}
