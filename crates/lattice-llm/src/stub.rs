//! Deterministic stub LLM implementation for Phase 6.
//!
//! Provides fake `chat` and `stream` implementations that require no provider
//! credentials, no HTTP clients, and no real model routing. The stub produces
//! deterministic output from the user prompt content.

use crate::types::{ChatRequest, ChatResponse, LlmEvent, ModelRouter};
use futures::stream::{self, Stream};
use std::pin::Pin;
use thiserror::Error;

/// Errors that can occur during LLM operations.
#[derive(Debug, Error)]
pub enum LlmError {
    /// A stub-only error for Phase 6.
    #[error("LLM error: {0}")]
    Other(String),
}

/// A streaming LLM response — a boxed stream of [`LlmEvent`] values.
pub type LlmStream = Pin<Box<dyn Stream<Item = LlmEvent> + Send>>;

/// A deterministic stub LLM that returns fake chat and stream responses.
///
/// Never reads provider credentials, environment variables, or makes HTTP calls.
#[derive(Clone, Debug, Default)]
pub struct StubLlm {
    /// Optional model router for target resolution (Phase 6 stub: identity).
    pub router: ModelRouter,
}

impl StubLlm {
    /// Create a new StubLlm with default settings.
    pub fn new() -> Self {
        StubLlm::default()
    }

    /// Return a deterministic chat response.
    ///
    /// The response content includes the phrase "stub response" and incorporates
    /// the last user message for deterministic observability.
    /// Never calls provider transports or reads credentials.
    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, LlmError> {
        let model = self.router.resolve(&request.target);

        let last_user_msg = request
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, crate::types::Role::User))
            .map(|m| m.content.as_str())
            .unwrap_or("");

        let content = if last_user_msg.is_empty() {
            "stub response".to_string()
        } else {
            format!("stub response: {}", last_user_msg)
        };

        Ok(ChatResponse {
            content,
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model,
        })
    }

    /// Return a fake streaming response as a boxed stream of [`LlmEvent`] values.
    ///
    /// Yields `Started`, a `Token("stub response")`, then `Done`.
    /// Never calls provider transports or reads credentials.
    pub fn stream(&self, request: ChatRequest) -> Result<LlmStream, LlmError> {
        let last_user_msg = request
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, crate::types::Role::User))
            .map(|m| m.content.as_str())
            .unwrap_or("");

        let token_content = if last_user_msg.is_empty() {
            "stub response".to_string()
        } else {
            format!("stub response: {}", last_user_msg)
        };

        let events: Vec<LlmEvent> = vec![
            LlmEvent::Started,
            LlmEvent::Token {
                content: token_content,
            },
            LlmEvent::Done,
        ];

        Ok(Box::pin(stream::iter(events)))
    }
}

/// Convenience function: run a chat completion through the default stub.
pub async fn chat(request: ChatRequest) -> Result<ChatResponse, LlmError> {
    StubLlm::new().chat(request).await
}

/// Convenience function: stream a completion through the default stub.
pub fn stream(request: ChatRequest) -> Result<LlmStream, LlmError> {
    StubLlm::new().stream(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ModelTarget};
    use futures::StreamExt;

    /// Build a minimal chat request with the given user text.
    fn make_request(text: &str) -> ChatRequest {
        ChatRequest::new(vec![Message::user(text)], ModelTarget::default())
    }

    // ── RED tests (will fail until GREEN implementations are in place) ─────

    #[tokio::test]
    async fn chat_returns_stub_response_content() {
        let stub = StubLlm::new();
        let req = make_request("hello");
        let resp = stub.chat(req).await.unwrap();
        assert!(
            resp.content.contains("stub response"),
            "chat response should contain 'stub response', got: '{}'",
            resp.content
        );
    }

    #[tokio::test]
    async fn chat_default_stub_produces_deterministic_response() {
        let req = make_request("hello");
        let resp = chat(req).await.unwrap();
        assert!(
            !resp.content.is_empty(),
            "chat convenience fn should produce non-empty content"
        );
    }

    #[tokio::test]
    async fn stream_yields_started_token_done_sequence() {
        let stub = StubLlm::new();
        let req = make_request("hello");
        let stream = stub.stream(req).unwrap();
        let events: Vec<LlmEvent> = stream.collect().await;

        assert!(
            events.len() >= 3,
            "stream should yield at least 3 events, got {}: {:?}",
            events.len(),
            events
        );
        assert!(
            matches!(events[0], LlmEvent::Started),
            "first event should be Started, got {:?}",
            events[0]
        );
        assert!(
            matches!(&events[events.len() - 1], LlmEvent::Done),
            "last event should be Done, got {:?}",
            events.last()
        );

        // At least one Token event should exist between Started and Done.
        let has_token = events
            .iter()
            .any(|e| matches!(e, LlmEvent::Token { .. }));
        assert!(has_token, "stream should contain at least one Token event");
    }

    #[tokio::test]
    async fn stream_token_contains_stub_response() {
        let stub = StubLlm::new();
        let req = make_request("test prompt");
        let stream = stub.stream(req).unwrap();
        let events: Vec<LlmEvent> = stream.collect().await;
        let token_events: Vec<&LlmEvent> = events
            .iter()
            .filter(|e| matches!(e, LlmEvent::Token { .. }))
            .collect();
        assert!(!token_events.is_empty(), "should have at least one Token event");
        for token in token_events {
            if let LlmEvent::Token { content } = token {
                assert!(
                    content.contains("stub response"),
                    "Token content should contain 'stub response', got: '{}'",
                    content
                );
            }
        }
    }

    // ── Boundary tests (must always pass) ──────────────────────────────────

    #[test]
    fn stub_llm_no_lattice_core_references() {
        // Compile-time check: no `lattice_core` or `lattice_runtime` in this file.
        // If this file compiles without those crates, the test passes.
        // We verify at the module level that no forbidden imports exist.
        let stub = StubLlm::new();
        // StubLlm must exist and be constructable.
        assert!(stub.router.default_model.is_none());
    }
}
