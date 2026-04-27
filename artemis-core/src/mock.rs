#![allow(deprecated)]
//! Mock provider and tool for validating the Python↔Rust boundary.
//!
//! These types are **not** for production use — they exercise the full
//! `ArtemisEngine → Provider → Event` pipeline with canned responses,
//! proving the architecture works end-to-end before any real provider
//! is implemented.

use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use crate::provider::{ChatRequest, ChatResponse, Provider, ProviderError};
use crate::streaming::{EventStream, TokenUsage};
use crate::types::ToolCall;

// ---------------------------------------------------------------------------
// MockProvider
// ---------------------------------------------------------------------------

/// A configurable mock provider that returns canned responses.
///
/// By default, the first call to `chat()` returns content + optional tool
/// calls, and subsequent calls return a final text response (simulating the
/// "tool result → final answer" round-trip). This behaviour can be
/// customised via the builder-style setters.
pub struct MockProvider {
    name: String,
    /// Content returned on the *first* `chat()` call.
    first_content: String,
    /// Tool calls returned on the *first* `chat()` call.
    first_tool_calls: Option<Vec<ToolCall>>,
    /// Content returned on *subsequent* `chat()` calls (after tool results).
    final_content: String,
    /// Simulated delay before each response.
    delay: Duration,
    /// Tracks how many times `chat()` has been called.
    call_count: Mutex<u64>,
}

impl MockProvider {
    /// Create a new mock provider with the given name and sensible defaults.
    ///
    /// Defaults:
    /// - First response: content = `"Hello from mock!"`, no tool calls
    /// - Final response: content = `"Final response from mock!"`
    /// - No delay
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            first_content: "Hello from mock!".to_string(),
            first_tool_calls: None,
            final_content: "Final response from mock!".to_string(),
            delay: Duration::ZERO,
            call_count: Mutex::new(0),
        }
    }

    /// Set the content for the first response.
    pub fn with_first_content(mut self, content: &str) -> Self {
        self.first_content = content.to_string();
        self
    }

    /// Set tool calls for the first response.
    pub fn with_first_tool_calls(mut self, calls: Vec<ToolCall>) -> Self {
        self.first_tool_calls = Some(calls);
        self
    }

    /// Set the content for subsequent (final) responses.
    pub fn with_final_content(mut self, content: &str) -> Self {
        self.final_content = content.to_string();
        self
    }

    /// Set a simulated delay for each response.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Return the number of times `chat()` has been called.
    pub fn call_count(&self) -> u64 {
        *self.call_count.lock().unwrap()
    }
}

#[async_trait]
impl Provider for MockProvider {
    async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, ProviderError> {
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }

        let count = {
            let mut c = self.call_count.lock().unwrap();
            *c += 1;
            *c
        };

        if count == 1 {
            // First call: return initial content + optional tool calls
            Ok(ChatResponse {
                content: Some(self.first_content.clone()),
                tool_calls: self.first_tool_calls.clone(),
                usage: Some(TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 20,
                    total_tokens: 30,
                }),
                finish_reason: if self.first_tool_calls.is_some() {
                    "tool_calls".to_string()
                } else {
                    "stop".to_string()
                },
                model: self.name.clone(),
            })
        } else {
            // Subsequent calls: return final content (tool results processed)
            Ok(ChatResponse {
                content: Some(self.final_content.clone()),
                tool_calls: None,
                usage: Some(TokenUsage {
                    prompt_tokens: 15,
                    completion_tokens: 25,
                    total_tokens: 40,
                }),
                finish_reason: "stop".to_string(),
                model: self.name.clone(),
            })
        }
    }

    async fn chat_stream(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
        Err(ProviderError::Stream(
            "MockProvider does not support streaming".to_string(),
        ))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_tools(&self) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// MockTool
// ---------------------------------------------------------------------------

/// A mock tool that records calls and returns configurable results.
///
/// Each call is recorded as a `(name, arguments)` pair accessible via
/// `calls()`. The return value is configurable via `with_output()`.
pub struct MockTool {
    name: String,
    output: String,
    calls: Mutex<Vec<(String, String)>>,
}

impl MockTool {
    /// Create a new mock tool with the given name.
    ///
    /// Default output: `"mock tool output"`.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            output: "mock tool output".to_string(),
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Set the output returned when the tool is called.
    pub fn with_output(mut self, output: &str) -> Self {
        self.output = output.to_string();
        self
    }

    /// "Execute" the tool: record the call and return the configured output.
    pub fn execute(&self, name: &str, arguments: &str) -> String {
        let mut calls = self.calls.lock().unwrap();
        calls.push((name.to_string(), arguments.to_string()));
        self.output.clone()
    }

    /// Return all recorded calls as `(name, arguments)` pairs.
    pub fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().unwrap().clone()
    }

    /// Return the number of times the tool has been called.
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    /// Return the tool's name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FunctionCall, Message, ProviderConfig, Role, TransportType};

    fn make_request() -> ChatRequest {
        ChatRequest {
            messages: vec![Message {
                role: Role::User,
                content: "test".to_string(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            model: "mock-model".to_string(),
            temperature: None,
            max_tokens: None,
            stream: false,
            provider_config: ProviderConfig {
                name: "mock".to_string(),
                api_base: "http://localhost".to_string(),
                api_key: None,
                transport: TransportType::ChatCompletions,
                extra_headers: None,
            },
        }
    }

    #[tokio::test]
    async fn test_mock_provider_first_call_no_tools() {
        let provider = MockProvider::new("test");
        let resp = provider.chat(make_request()).await.unwrap();
        assert_eq!(resp.content.unwrap(), "Hello from mock!");
        assert!(resp.tool_calls.is_none());
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(provider.call_count(), 1);
    }

    #[tokio::test]
    async fn test_mock_provider_first_call_with_tools() {
        let provider = MockProvider::new("test").with_first_tool_calls(vec![ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "get_weather".to_string(),
                arguments: r#"{"city":"Paris"}"#.to_string(),
            },
        }]);
        let resp = provider.chat(make_request()).await.unwrap();
        assert_eq!(resp.content.unwrap(), "Hello from mock!");
        assert!(resp.tool_calls.is_some());
        assert_eq!(resp.finish_reason, "tool_calls");
    }

    #[tokio::test]
    async fn test_mock_provider_subsequent_call_returns_final() {
        let provider = MockProvider::new("test")
            .with_first_content("first")
            .with_final_content("done!");
        let _ = provider.chat(make_request()).await.unwrap();
        let resp = provider.chat(make_request()).await.unwrap();
        assert_eq!(resp.content.unwrap(), "done!");
        assert!(resp.tool_calls.is_none());
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(provider.call_count(), 2);
    }

    #[tokio::test]
    async fn test_mock_provider_custom_content() {
        let provider = MockProvider::new("test")
            .with_first_content("custom first")
            .with_final_content("custom final");
        let resp = provider.chat(make_request()).await.unwrap();
        assert_eq!(resp.content.unwrap(), "custom first");
        let resp = provider.chat(make_request()).await.unwrap();
        assert_eq!(resp.content.unwrap(), "custom final");
    }

    #[tokio::test]
    async fn test_mock_provider_delay() {
        let provider = MockProvider::new("test").with_delay(Duration::from_millis(50));
        let start = std::time::Instant::now();
        let _ = provider.chat(make_request()).await.unwrap();
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(40)); // small tolerance
    }

    #[tokio::test]
    async fn test_mock_provider_name() {
        let provider = MockProvider::new("my_mock");
        assert_eq!(provider.name(), "my_mock");
    }

    #[tokio::test]
    async fn test_mock_provider_capabilities() {
        let provider = MockProvider::new("test");
        assert!(!provider.supports_streaming());
        assert!(provider.supports_tools());
    }

    #[tokio::test]
    async fn test_mock_provider_stream_returns_error() {
        let provider = MockProvider::new("test");
        let result = provider.chat_stream(make_request()).await;
        assert!(result.is_err());
        match result.err().unwrap() {
            ProviderError::Stream(msg) => assert!(msg.contains("does not support streaming")),
            other => panic!("Expected Stream error, got {other:?}"),
        }
    }

    #[test]
    fn test_mock_tool_execute() {
        let tool = MockTool::new("search").with_output("found 3 results");
        let result = tool.execute("search", r#"{"query":"rust"}"#);
        assert_eq!(result, "found 3 results");
        assert_eq!(tool.call_count(), 1);
    }

    #[test]
    fn test_mock_tool_records_calls() {
        let tool = MockTool::new("calc");
        tool.execute("calc", "1+1");
        tool.execute("calc", "2+2");
        let calls = tool.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], ("calc".to_string(), "1+1".to_string()));
        assert_eq!(calls[1], ("calc".to_string(), "2+2".to_string()));
    }

    #[test]
    fn test_mock_tool_name() {
        let tool = MockTool::new("my_tool");
        assert_eq!(tool.name(), "my_tool");
    }

    #[test]
    fn test_mock_tool_default_output() {
        let tool = MockTool::new("test");
        let result = tool.execute("test", "");
        assert_eq!(result, "mock tool output");
    }
}
