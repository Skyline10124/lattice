use artemis_core::agent_loop::{AgentLoop, LoopConfig, LoopEvent};
use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::mock::MockProvider;
use artemis_core::provider::ChatRequest;
use artemis_core::router::ModelRouter;
use artemis_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};
use serde_json::json;
use std::collections::HashMap;

fn make_resolved(
    provider: &str,
    model: &str,
    protocol: ApiProtocol,
    base_url: &str,
) -> ResolvedModel {
    ResolvedModel {
        canonical_id: model.to_string(),
        provider: provider.to_string(),
        api_key: Some("sk-test-e2e".to_string()),
        base_url: base_url.to_string(),
        api_protocol: protocol,
        api_model_id: model.to_string(),
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

fn weather_tool() -> ToolDefinition {
    ToolDefinition {
        name: "get_weather".to_string(),
        description: "Get current weather".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
    }
}

#[test]
fn test_model_centric_resolve_stream_done() {
    let resolved = make_resolved(
        "mock",
        "test-model",
        ApiProtocol::OpenAiChat,
        "http://localhost/v1",
    );

    let mut provider = MockProvider::new("mock");
    provider.set_response("Hello from mock!");

    let agent = AgentLoop::new();
    let messages = vec![user_message("Hello")];
    let events = agent.run(&provider, resolved, messages, vec![], LoopConfig::default());

    let has_token = events.iter().any(|e| matches!(e, LoopEvent::Token { .. }));
    let has_done = events.iter().any(|e| matches!(e, LoopEvent::Done { .. }));
    assert!(has_token, "should emit Token event");
    assert!(has_done, "should emit Done event");
}

#[test]
fn test_model_centric_resolve_tool_call_submit() {
    let resolved = make_resolved(
        "mock",
        "test-model",
        ApiProtocol::OpenAiChat,
        "http://localhost/v1",
    );

    let provider = MockProvider::new("mock")
        .with_first_content("Let me check the weather.")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_weather_1".to_string(),
            function: FunctionCall {
                name: "get_weather".to_string(),
                arguments: r#"{"city": "Tokyo"}"#.to_string(),
            },
        }])
        .with_final_content("Tokyo is sunny, 22°C.");

    let agent = AgentLoop::new();
    let messages = vec![user_message("What's the weather in Tokyo?")];
    let tools = vec![weather_tool()];

    let events = agent.run(&provider, resolved, messages, tools, LoopConfig::default());

    let has_tool_call = events
        .iter()
        .any(|e| matches!(e, LoopEvent::ToolCallRequired { .. }));
    assert!(has_tool_call, "first call should emit ToolCallRequired");

    let has_done = events.iter().any(|e| matches!(e, LoopEvent::Done { .. }));
    assert!(has_done, "agent loop should terminate with Done");
}

#[test]
fn test_router_resolve_provides_resolved_model() {
    let _lock = ENV_MUTEX.lock().unwrap();
    let saved_keys: Vec<(String, Option<String>)> = [
        "ANTHROPIC_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
        "OPENAI_API_KEY",
    ]
    .iter()
    .map(|k| (k.to_string(), save_env(k)))
    .collect();

    for k in &[
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
        "OPENAI_API_KEY",
    ] {
        env::remove_var(k);
    }
    env::set_var("ANTHROPIC_API_KEY", "sk-ant-test");

    let router = ModelRouter::new();
    let resolved = router
        .resolve("sonnet", None)
        .expect("sonnet should resolve");

    assert_eq!(resolved.canonical_id, "claude-sonnet-4-6");
    assert_eq!(resolved.provider, "anthropic");
    assert_eq!(resolved.api_protocol, ApiProtocol::AnthropicMessages);
    assert!(resolved.api_key.is_some());
    assert_eq!(resolved.api_key.unwrap(), "sk-ant-test");

    for (k, v) in saved_keys {
        restore_env(&k, v);
    }
}

#[test]
fn test_resolve_to_chat_request_roundtrip() {
    let resolved = make_resolved(
        "openai",
        "gpt-4o",
        ApiProtocol::OpenAiChat,
        "https://api.openai.com/v1",
    );

    let request = ChatRequest::new(
        vec![user_message("What is 2+2?")],
        vec![weather_tool()],
        resolved.clone(),
    );

    assert_eq!(request.model, "gpt-4o");
    assert_eq!(request.resolved.canonical_id, "gpt-4o");
    assert_eq!(request.resolved.provider, "openai");
    assert_eq!(request.resolved.api_protocol, ApiProtocol::OpenAiChat);
    assert_eq!(request.messages.len(), 1);
    assert_eq!(request.tools.len(), 1);
}

use std::env;
use std::sync::{LazyLock, Mutex};

static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn save_env(key: &str) -> Option<String> {
    env::var(key).ok()
}

fn restore_env(key: &str, prev: Option<String>) {
    match prev {
        Some(v) => env::set_var(key, v),
        None => env::remove_var(key),
    }
}
