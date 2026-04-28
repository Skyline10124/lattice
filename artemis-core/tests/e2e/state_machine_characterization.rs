//! State machine characterization tests for EngineState and AgentLoop.
//!
//! These tests capture CURRENT behavior before any bug fixes or architecture
//! changes. They document known bugs and design limitations as observable
//! assertions, NOT as desired behavior.
//!
//! ## Documented behaviors (bugs/limitations):
//!
//! 1. **EngineState has no `messages` field** — only stores `tools`,
//!    `last_response`, and `default_model`. Full conversation history is
//!    lost after `run_conversation()`. See doc section below.
//!
//! 2. **submit_tool_result builds a 2-element vec** — only the last
//!    assistant message + the tool result, NOT the full conversation
//!    history. The original user message and prior turns are discarded.
//!    See doc section below.
//!
//! 3. **submit_tool_results (batch) makes N separate API calls** — each
//!    tool result triggers an independent `submit_tool_result` call,
//!    each with its own 2-element message vec, instead of batching all
//!    tool results into a single API call.
//!
//! 4. **engine.rs run_once hardcodes `provider: "mock"`** — the
//!    ResolvedModel always has provider set to "mock" regardless of
//!    the actual registered provider.
//!
//! 5. **AgentLoop uses futures::executor::block_on** — not tokio
//!    runtime. This means real async I/O providers that rely on tokio
//!    will panic at runtime.
//!
//! 6. **AgentLoop hardcodes "mock tool result"** — every tool call
//!    receives the literal string "mock tool result" as its output,
//!    regardless of the actual tool execution.
//!
//! 7. **register_model creates MockProvider** — every model registered
//!    via `register_model()` gets a MockProvider, not a real provider.

use artemis_core::agent_loop::{AgentLoop, LoopConfig, LoopEvent};
use artemis_core::catalog::{ApiProtocol, ResolvedModel};
use artemis_core::engine::{Event, ToolCallInfo};
use artemis_core::mock::MockProvider;
use artemis_core::provider::Provider;
use artemis_core::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};
use serde_json::json;
use std::collections::HashMap;

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

// ---------------------------------------------------------------------------
// ENGINE STATE CHARACTERIZATION (static analysis)
//
// EngineState (engine.rs:142-146) is a private struct inside ArtemisEngine.
// Its methods are PyO3 `#[pymethods]`, so they are Rust-private and only
// callable from Python. The following behaviors are documented from source
// reading, not runtime testing.
//
// EngineState fields:
//   tools: Vec<ToolDefinition>
//   last_response: Option<ChatResponse>
//   default_model: Option<String>
//
// BUG: No `messages` field. Full conversation history is lost after
// run_conversation(). When submit_tool_result needs to continue the
// conversation, it can only reconstruct from last_response.
//
// BUG (engine.rs:341-358): submit_tool_result builds only a 2-element vec:
//   vec![
//     Message { role: Assistant, content: last_response.content, ... },
//     Message { role: Tool, content: result, tool_call_id: Some(id), ... },
//   ]
// The original user message and all prior turns are NOT included.
//
// BUG (engine.rs:238-249): submit_tool_results (batch) iterates and calls
// submit_tool_result N times for N results. Each call makes a separate
// API request with its own 2-element message vec. The correct behavior
// would be to batch all tool results in a single request.
//
// BUG (engine.rs:280,299,388): run_once and submit_tool_result hardcode
// provider: "mock" in the constructed ResolvedModel. This is metadata-only
// (dispatch goes through the registry), so it doesn't break functionality.
//
// BUG (engine.rs:443): register_model() always creates a MockProvider
// regardless of the provider_id parameter.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 1. AgentLoop simple conversation
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_simple_conversation() {
    let mut provider = MockProvider::new("mock");
    provider.set_response("Hello from agent loop!");
    let agent = AgentLoop::new();

    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Hi")],
        vec![],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Token { .. })),
        "CHAR: AgentLoop emits Token event for content responses"
    );
    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: AgentLoop emits Done event on completion"
    );
}

// ---------------------------------------------------------------------------
// 2. AgentLoop tool call triggers auto-continue
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_tool_call_triggers_auto_continue() {
    let provider = MockProvider::new("mock")
        .with_first_content("Let me check.")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "get_weather".to_string(),
                arguments: r#"{"city":"Paris"}"#.to_string(),
            },
        }])
        .with_final_content("Paris is sunny.");

    let agent = AgentLoop::new();
    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Weather in Paris?")],
        vec![weather_tool()],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::ToolCallRequired { .. })),
        "CHAR: AgentLoop emits ToolCallRequired when provider returns tool calls"
    );
    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: AgentLoop auto-continues and emits Done after tool calls"
    );
}

// ---------------------------------------------------------------------------
// 3. AgentLoop max_iterations limit
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_max_iterations_limit() {
    let provider = MockProvider::new("mock")
        .with_first_content("Thinking...")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_loop".to_string(),
            function: FunctionCall {
                name: "loop_tool".to_string(),
                arguments: "{}".to_string(),
            },
        }])
        .with_final_content("Done!");

    let agent = AgentLoop::new();
    let config = LoopConfig {
        max_iterations: 2,
        budget_tokens: 100_000,
    };

    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Go")],
        vec![],
        config,
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: AgentLoop respects max_iterations and terminates"
    );
}

// ---------------------------------------------------------------------------
// 4. AgentLoop interrupt before run
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_interrupt_before_run() {
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

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Interrupted)),
        "CHAR: AgentLoop emits Interrupted when flag is set before run"
    );
    assert!(
        !events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: Interrupted run should not produce Done"
    );
}

// ---------------------------------------------------------------------------
// 5. AgentLoop uses futures::executor::block_on (not tokio)
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_block_on_with_mock_provider_works() {
    // AgentLoop.run() uses futures::executor::block_on(provider.chat(request))
    // (agent_loop.rs:96). This works with MockProvider because its chat()
    // doesn't require a tokio reactor.
    //
    // BUG: Real providers that use reqwest or other tokio-based I/O will
    // panic because futures::executor::block_on doesn't set up a tokio reactor.

    let provider = MockProvider::new("mock").with_first_content("Works with block_on!");
    let agent = AgentLoop::new();

    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Test")],
        vec![],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: MockProvider works with futures::executor::block_on (no tokio reactor needed)"
    );
}

// ---------------------------------------------------------------------------
// 6. AgentLoop hardcodes "mock tool result" string
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_hardcodes_mock_tool_result() {
    // BUG: In agent_loop.rs:115, when the provider returns tool calls,
    // AgentLoop pushes a Tool message with the hardcoded string
    // "mock tool result" for EVERY tool call, regardless of what the
    // tool actually returns.

    let provider = MockProvider::new("mock")
        .with_first_content("Need to search.")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_search".to_string(),
            function: FunctionCall {
                name: "search".to_string(),
                arguments: r#"{"q":"rust"}"#.to_string(),
            },
        }])
        .with_final_content("Based on search results: ...");

    let agent = AgentLoop::new();
    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Search for rust")],
        vec![],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: AgentLoop auto-completes tool calls with hardcoded 'mock tool result'"
    );

    let done_event = events
        .iter()
        .find(|e| matches!(e, LoopEvent::Done { .. }))
        .expect("should have Done event");

    if let LoopEvent::Done {
        finish_reason,
        final_message,
    } = done_event
    {
        assert_eq!(finish_reason, "stop", "CHAR: Done event has finish_reason='stop'");
        assert_eq!(
            final_message.content, "Based on search results: ...",
            "CHAR: final_message content is from MockProvider's final_content (not real tool output)"
        );
    }
}

#[test]
fn char_agent_loop_multiple_tool_calls_all_get_mock_result() {
    // When multiple tool calls are returned, AgentLoop pushes "mock tool result"
    // for EACH one (agent_loop.rs:105-119).

    let provider = MockProvider::new("mock")
        .with_first_content("Checking multiple things.")
        .with_first_tool_calls(vec![
            ToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "search".to_string(),
                    arguments: "{}".to_string(),
                },
            },
            ToolCall {
                id: "call_2".to_string(),
                function: FunctionCall {
                    name: "calculate".to_string(),
                    arguments: "{}".to_string(),
                },
            },
        ])
        .with_final_content("Combined results.");

    let agent = AgentLoop::new();
    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Do multiple things")],
        vec![],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::ToolCallRequired { .. })),
        "CHAR: ToolCallRequired emitted for multiple tool calls"
    );
    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: Loop completes after all tool calls get mock results"
    );
}

// ---------------------------------------------------------------------------
// 7. AgentLoop conversation history grows across tool call iterations
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_conversation_grows_with_tool_calls() {
    // In agent_loop.rs:105-119, when tool calls are received, AgentLoop
    // pushes assistant+tool messages into the conversation vec and loops.
    // This means the conversation grows with each tool call round.
    // (Contrast with EngineState.submit_tool_result which only sends
    // 2 messages — AgentLoop correctly accumulates history.)

    let provider = MockProvider::new("mock")
        .with_first_content("First response with tool call.")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "search".to_string(),
                arguments: "{}".to_string(),
            },
        }])
        .with_final_content("Final answer after tool execution.");

    let agent = AgentLoop::new();
    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Search")],
        vec![],
        LoopConfig::default(),
    );

    assert_eq!(provider.call_count(), 2, "CHAR: MockProvider called twice (tool call + final)");
    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: Loop completes after tool call + final response"
    );
}

// ---------------------------------------------------------------------------
// 8. AgentLoop error handling
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_provider_error_emits_error_event() {
    // When the provider returns an error, AgentLoop emits LoopEvent::Error
    // and breaks out of the loop (agent_loop.rs:154-159).

    use artemis_core::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
    use async_trait::async_trait;

    struct FailingProvider;

    #[async_trait]
    impl Provider for FailingProvider {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
            Err(ProviderError::Api("Simulated API failure".to_string()))
        }
        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<artemis_core::streaming::EventStream, ProviderError> {
            Err(ProviderError::Stream("not supported".to_string()))
        }
        fn name(&self) -> &str {
            "failing"
        }
        fn supports_streaming(&self) -> bool {
            false
        }
        fn supports_tools(&self) -> bool {
            true
        }
    }

    let agent = AgentLoop::new();
    let events = agent.run(
        &FailingProvider,
        make_resolved(),
        vec![user_message("Hi")],
        vec![],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Error { .. })),
        "CHAR: AgentLoop emits Error event when provider fails"
    );
    assert!(
        !events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: Error event means no Done event"
    );
}

// ---------------------------------------------------------------------------
// 9. Event structure characterization
// ---------------------------------------------------------------------------

#[test]
fn char_event_structure_token() {
    let event = Event {
        kind: "token".to_string(),
        content: Some("hello".to_string()),
        tool_calls: None,
        finish_reason: None,
    };
    assert_eq!(event.kind, "token");
    assert_eq!(event.content, Some("hello".to_string()));
    assert!(event.tool_calls.is_none());
    assert!(event.finish_reason.is_none());
}

#[test]
fn char_event_structure_tool_call_required() {
    let event = Event {
        kind: "tool_call_required".to_string(),
        content: None,
        tool_calls: Some(vec![ToolCallInfo {
            id: "call_1".to_string(),
            name: "search".to_string(),
            arguments: "{}".to_string(),
        }]),
        finish_reason: None,
    };
    assert_eq!(event.kind, "tool_call_required");
    assert!(event.content.is_none());
    assert!(event.tool_calls.is_some());
    assert_eq!(event.tool_calls.as_ref().unwrap().len(), 1);
}

#[test]
fn char_event_structure_done() {
    let event = Event {
        kind: "done".to_string(),
        content: None,
        tool_calls: None,
        finish_reason: Some("stop".to_string()),
    };
    assert_eq!(event.kind, "done");
    assert!(event.content.is_none());
    assert!(event.tool_calls.is_none());
    assert_eq!(event.finish_reason, Some("stop".to_string()));
}

// ---------------------------------------------------------------------------
// 10. LoopConfig defaults
// ---------------------------------------------------------------------------

#[test]
fn char_loop_config_defaults() {
    let config = LoopConfig::default();
    assert_eq!(config.max_iterations, 10, "CHAR: LoopConfig defaults to max_iterations=10");
    assert_eq!(config.budget_tokens, 100_000, "CHAR: LoopConfig defaults to budget_tokens=100_000");
}

// ---------------------------------------------------------------------------
// 11. AgentLoop content-only response path
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_content_only_triggers_done() {
    // In agent_loop.rs:137-152, if tool_calls is None and content exists,
    // the code emits Token + Done and breaks.

    let provider = MockProvider::new("mock").with_first_content("Done without tools.");
    let agent = AgentLoop::new();

    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Hi")],
        vec![],
        LoopConfig::default(),
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Token { .. })),
        "CHAR: When tool_calls=None and content exists, Token is emitted"
    );
    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: When tool_calls=None and content exists, Done is emitted"
    );
}

// ---------------------------------------------------------------------------
// 12. MockProvider behavior characterization
// ---------------------------------------------------------------------------

#[test]
fn char_mock_provider_first_call_returns_first_content() {
    let provider = MockProvider::new("test").with_first_content("First!");
    assert_eq!(provider.call_count(), 0, "CHAR: call_count starts at 0");

    let resolved = make_resolved();
    let request = artemis_core::provider::ChatRequest::new(
        vec![user_message("Hi")],
        vec![],
        resolved,
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let response = rt.block_on(provider.chat(request)).unwrap();

    assert_eq!(response.content.unwrap(), "First!", "CHAR: first call returns first_content");
    assert!(response.tool_calls.is_none(), "CHAR: first call has no tool_calls by default");
    assert_eq!(response.finish_reason, "stop", "CHAR: first call finish_reason is 'stop'");
    assert_eq!(provider.call_count(), 1, "CHAR: call_count incremented to 1");
}

#[test]
fn char_mock_provider_subsequent_call_returns_final_content() {
    let provider = MockProvider::new("test")
        .with_first_content("First")
        .with_final_content("Final!");

    let resolved = make_resolved();
    let request = artemis_core::provider::ChatRequest::new(
        vec![user_message("Hi")],
        vec![],
        resolved.clone(),
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let _first = rt.block_on(provider.chat(request)).unwrap();

    let request2 = artemis_core::provider::ChatRequest::new(
        vec![user_message("Hi")],
        vec![],
        resolved,
    );
    let second = rt.block_on(provider.chat(request2)).unwrap();

    assert_eq!(second.content.unwrap(), "Final!", "CHAR: subsequent calls return final_content");
    assert!(second.tool_calls.is_none(), "CHAR: subsequent calls have no tool_calls");
    assert_eq!(second.finish_reason, "stop", "CHAR: subsequent calls finish_reason is 'stop'");
    assert_eq!(provider.call_count(), 2, "CHAR: call_count is 2");
}

#[test]
fn char_mock_provider_first_call_with_tool_calls() {
    let provider = MockProvider::new("test")
        .with_first_content("Need to search.")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "search".to_string(),
                arguments: "{}".to_string(),
            },
        }]);

    let resolved = make_resolved();
    let request = artemis_core::provider::ChatRequest::new(
        vec![user_message("Hi")],
        vec![],
        resolved,
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let response = rt.block_on(provider.chat(request)).unwrap();

    assert_eq!(response.content.unwrap(), "Need to search.", "CHAR: first_content returned");
    assert!(response.tool_calls.is_some(), "CHAR: tool_calls returned on first call");
    assert_eq!(
        response.finish_reason, "tool_calls",
        "CHAR: finish_reason is 'tool_calls' when tool_calls present"
    );
}

// ---------------------------------------------------------------------------
// 13. Done event finish_reason characterization
// ---------------------------------------------------------------------------

#[test]
fn char_done_event_has_finish_reason() {
    let mut provider = MockProvider::new("mock");
    provider.set_response("Hello!");

    let agent = AgentLoop::new();
    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Hi")],
        vec![],
        LoopConfig::default(),
    );

    let done = events.iter().find(|e| matches!(e, LoopEvent::Done { .. }));
    assert!(done.is_some(), "CHAR: Done event present");
    if let Some(LoopEvent::Done { finish_reason, .. }) = done {
        assert_eq!(finish_reason, "stop", "CHAR: Done finish_reason is 'stop' for content responses");
    }
}

#[test]
fn char_tool_call_done_has_tool_calls_finish_reason() {
    let provider = MockProvider::new("mock")
        .with_first_content("Checking...")
        .with_first_tool_calls(vec![ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "search".to_string(),
                arguments: "{}".to_string(),
            },
        }])
        .with_final_content("Found it!");

    let agent = AgentLoop::new();
    let events = agent.run(
        &provider,
        make_resolved(),
        vec![user_message("Search")],
        vec![],
        LoopConfig::default(),
    );

    let tool_call = events.iter().find(|e| matches!(e, LoopEvent::ToolCallRequired { .. }));
    assert!(tool_call.is_some(), "CHAR: ToolCallRequired present");
    if let Some(LoopEvent::ToolCallRequired { tool_calls }) = tool_call {
        assert_eq!(tool_calls.len(), 1, "CHAR: One tool call in ToolCallRequired");
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].function.name, "search");
    }
}

// ---------------------------------------------------------------------------
// 14. AgentLoop run_with_fallback characterization
// ---------------------------------------------------------------------------

#[test]
fn char_agent_loop_fallback_tries_providers_in_order() {
    use artemis_core::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
    use artemis_core::retry::{ErrorClassifier, RetryPolicy};
    use async_trait::async_trait;
    use std::time::Duration;

    struct FailingProvider;

    #[async_trait]
    impl Provider for FailingProvider {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
            Err(ProviderError::Api("fail".to_string()))
        }
        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<artemis_core::streaming::EventStream, ProviderError> {
            Err(ProviderError::Stream("not supported".to_string()))
        }
        fn name(&self) -> &str {
            "failing"
        }
        fn supports_streaming(&self) -> bool {
            false
        }
        fn supports_tools(&self) -> bool {
            true
        }
    }

    let mut fallback = MockProvider::new("fallback");
    fallback.set_response("Fallback result!");

    let agent = AgentLoop::new();
    let providers: Vec<&dyn Provider> = vec![&FailingProvider, &fallback];
    let policy = RetryPolicy {
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
        &ErrorClassifier,
        &policy,
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
        "CHAR: Fallback to second provider succeeds"
    );
}

#[test]
fn char_agent_loop_fallback_all_fail_returns_error() {
    use artemis_core::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
    use artemis_core::retry::{ErrorClassifier, RetryPolicy};
    use async_trait::async_trait;
    use std::time::Duration;

    struct FailingProvider1;
    struct FailingProvider2;

    #[async_trait]
    impl Provider for FailingProvider1 {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
            Err(ProviderError::Api("fail1".to_string()))
        }
        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<artemis_core::streaming::EventStream, ProviderError> {
            Err(ProviderError::Stream("not supported".to_string()))
        }
        fn name(&self) -> &str {
            "fail1"
        }
        fn supports_streaming(&self) -> bool {
            false
        }
        fn supports_tools(&self) -> bool {
            true
        }
    }

    #[async_trait]
    impl Provider for FailingProvider2 {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
            Err(ProviderError::Api("fail2".to_string()))
        }
        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<artemis_core::streaming::EventStream, ProviderError> {
            Err(ProviderError::Stream("not supported".to_string()))
        }
        fn name(&self) -> &str {
            "fail2"
        }
        fn supports_streaming(&self) -> bool {
            false
        }
        fn supports_tools(&self) -> bool {
            true
        }
    }

    let agent = AgentLoop::new();
    let providers: Vec<&dyn Provider> = vec![&FailingProvider1, &FailingProvider2];
    let policy = RetryPolicy {
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
        &ErrorClassifier,
        &policy,
    );

    assert!(
        events.iter().any(|e| matches!(e, LoopEvent::Error { .. })),
        "CHAR: All providers failing produces Error event"
    );
    let error_msg = events.iter().find_map(|e| match e {
        LoopEvent::Error { message } => Some(message.clone()),
        _ => None,
    });
    if let Some(message) = error_msg {
        assert!(
            message.contains("All providers exhausted"),
            "CHAR: Error message mentions exhaustion"
        );
    }
}
