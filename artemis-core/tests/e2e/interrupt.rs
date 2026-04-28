use artemis_core::agent_loop::{AgentLoop, LoopConfig, LoopEvent};
use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::mock::MockProvider;
use artemis_core::types::{Message, Role};
use std::collections::HashMap;
use std::time::Duration;

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

#[test]
fn test_interrupt_before_run() {
    let mut provider = MockProvider::new("mock");
    provider.set_response("Hello!");

    let agent = AgentLoop::new();
    agent.interrupt();

    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Hi")],
        vec![],
        LoopConfig::default(),
    );

    assert!(events.iter().any(|e| matches!(e, LoopEvent::Interrupted)));
    assert!(!events.iter().any(|e| matches!(e, LoopEvent::Done { .. })));
}

#[test]
fn test_interrupt_during_conversation() {
    let provider = MockProvider::new("mock")
        .with_first_content("Thinking...")
        .with_first_tool_calls(vec![artemis_core::types::ToolCall {
            id: "call_1".to_string(),
            function: artemis_core::types::FunctionCall {
                name: "search".to_string(),
                arguments: "{}".to_string(),
            },
        }])
        .with_final_content("Done!")
        .with_delay(Duration::from_millis(50));

    let agent = AgentLoop::new();
    agent.interrupt();

    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Search for something")],
        vec![],
        LoopConfig::default(),
    );

    assert!(events.iter().any(|e| matches!(e, LoopEvent::Interrupted)));
}

#[test]
fn test_interrupt_with_fallback() {
    use artemis_core::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
    use async_trait::async_trait;

    struct SlowProvider;

    #[async_trait]
    impl Provider for SlowProvider {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
            tokio::time::sleep(Duration::from_secs(10)).await;
            Ok(ChatResponse {
                content: Some("Should not reach here".to_string()),
                tool_calls: None,
                usage: None,
                finish_reason: "stop".to_string(),
                model: "slow".to_string(),
            })
        }

        async fn chat_stream(&self, _request: ChatRequest) -> Result<artemis_core::streaming::EventStream, ProviderError> {
            Err(ProviderError::Stream("not supported".to_string()))
        }

        fn name(&self) -> &str { "slow" }
        fn supports_streaming(&self) -> bool { false }
        fn supports_tools(&self) -> bool { true }
    }

    let agent = AgentLoop::new();
    agent.interrupt();

    let mut fallback = MockProvider::new("fallback");
    fallback.set_response("Fallback result!");

    let providers: Vec<&dyn Provider> = vec![&SlowProvider, &fallback];
    let policy = artemis_core::retry::RetryPolicy {
        max_retries: 1,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(5),
    };

    let events = agent.run_with_fallback(
        providers,
        make_resolved(),
        vec![user_message("Hello")],
        vec![],
        LoopConfig::default(),
        &artemis_core::retry::ErrorClassifier,
        &policy,
    );

    assert!(events.iter().any(|e| matches!(e, LoopEvent::Interrupted)));
}

#[test]
fn test_no_interrupt_produces_normal_result() {
    let mut provider = MockProvider::new("mock");
    provider.set_response("Normal response");

    let agent = AgentLoop::new();
    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Hello")],
        vec![],
        LoopConfig::default(),
    );

    assert!(events.iter().any(|e| matches!(e, LoopEvent::Token { .. })));
    assert!(events.iter().any(|e| matches!(e, LoopEvent::Done { .. })));
    assert!(!events.iter().any(|e| matches!(e, LoopEvent::Interrupted)));
}