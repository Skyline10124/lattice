// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Deterministic stub LLM implementation
//!
//! Provides fake `chat` and `stream` implementations that require no provider
//! credentials, no HTTP clients, and no real model routing. The stub produces
//! deterministic output from the user prompt content.

use crate::router::ModelRouter;
use crate::types::{ChatRequest, ChatResponse, LlmEvent};
use futures::stream::{self, Stream};
use std::pin::Pin;
use thiserror::Error;

/// Errors that can occur during LLM operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum LlmError {
    #[error("unknown model alias: '{0}'")]
    UnknownModel(String),

    #[error("credential error: {message}")]
    Credential { message: String },

    #[error("configuration error: {message}")]
    Config { message: String },

    #[error("DNS error: {message}")]
    Dns { message: String },

    #[error("transport error: {message}")]
    Transport { message: String, retryable: bool },

    #[error("HTTP request timed out: {message}")]
    Timeout { message: String },

    #[error("provider rate limited with HTTP {status_code}: {message}")]
    RateLimited { status_code: u16, message: String },

    #[error("provider returned HTTP {status_code}: {message}")]
    ProviderHttp { status_code: u16, message: String },

    #[error("HTTP response read error: {message}")]
    StreamRead { message: String },

    #[error("protocol limit exceeded: {message}")]
    ProtocolLimit { message: String },

    #[error("LLM error: {0}")]
    Other(String),
}

impl LlmError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UnknownModel(_) => "unknown_model",
            Self::Credential { .. } => "credential",
            Self::Config { .. } => "config",
            Self::Dns { .. } => "dns",
            Self::Transport { .. } => "transport",
            Self::Timeout { .. } => "timeout",
            Self::RateLimited { .. } => "rate_limited",
            Self::ProviderHttp { .. } => "provider_http",
            Self::StreamRead { .. } => "stream_read",
            Self::ProtocolLimit { .. } => "protocol_limit",
            Self::Other(_) => "other",
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::Transport { retryable, .. } => *retryable,
            Self::Timeout { .. } | Self::RateLimited { .. } => true,
            Self::ProviderHttp { status_code, .. } => {
                matches!(*status_code, 500 | 502 | 503 | 504)
            }
            _ => false,
        }
    }

    pub fn status_code(&self) -> Option<u16> {
        match self {
            Self::RateLimited { status_code, .. } | Self::ProviderHttp { status_code, .. } => {
                Some(*status_code)
            }
            _ => None,
        }
    }
}

pub type LlmStream = Pin<Box<dyn Stream<Item = LlmEvent> + Send>>;

/// A deterministic stub LLM that returns fake chat and stream responses.
#[derive(Clone, Debug, Default)]
pub struct StubLlm {
    pub router: ModelRouter,
}

impl StubLlm {
    pub fn new() -> Self {
        StubLlm::default()
    }

    /// Return a deterministic chat response.
    pub async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, LlmError> {
        let resolved = self.router.resolve(&request.target)?;
        let mut stream = self.stream(request)?;
        use futures::StreamExt;

        let mut content = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Option<Vec<crate::types::ToolCall>> = None;
        let mut finish_reason = String::new();
        let mut usage: Option<crate::types::TokenUsage> = None;

        while let Some(event) = stream.next().await {
            match event {
                LlmEvent::Token { content: token } => content.push_str(&token),
                LlmEvent::Reasoning { content: r } => reasoning_content.push_str(&r),
                LlmEvent::ToolCallStart { id, name } => {
                    let calls = tool_calls.get_or_insert_with(Vec::new);
                    calls.push(crate::types::ToolCall {
                        id,
                        function: crate::types::FunctionCall {
                            name,
                            arguments: String::new(),
                        },
                    });
                }
                LlmEvent::ToolCallDelta {
                    id,
                    arguments_delta,
                } => {
                    if let Some(calls) = &mut tool_calls {
                        if let Some(call) = calls.iter_mut().find(|c| c.id == id) {
                            call.function.arguments.push_str(&arguments_delta);
                        }
                    }
                }
                LlmEvent::Done {
                    finish_reason: fr,
                    usage: u,
                } => {
                    finish_reason = fr;
                    usage = u;
                }
                LlmEvent::Error { message } => {
                    return Err(LlmError::Other(message));
                }
                _ => {}
            }
        }

        Ok(ChatResponse {
            content,
            reasoning_content: if reasoning_content.is_empty() {
                None
            } else {
                Some(reasoning_content)
            },
            tool_calls,
            finish_reason: if finish_reason.is_empty() {
                "stop".to_string()
            } else {
                finish_reason
            },
            model: resolved.alias,
            usage,
        })
    }

    /// Return a fake streaming response as a boxed stream of [`LlmEvent`] values.
    pub fn stream(&self, request: ChatRequest) -> Result<LlmStream, LlmError> {
        let _resolved = self.router.resolve(&request.target)?;

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
            LlmEvent::Done {
                finish_reason: "stop".to_string(),
                usage: None,
            },
        ];

        Ok(Box::pin(stream::iter(events)))
    }
}

/// Convenience function: run a chat completion through the real client.
pub async fn chat(request: ChatRequest) -> Result<ChatResponse, LlmError> {
    let resolved = ModelRouter::new().resolve(&request.target)?;
    let model = resolved.alias.clone();
    let mut stream = crate::client::dispatch_stream(request, resolved).await?;
    use futures::StreamExt;

    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: Option<Vec<crate::types::ToolCall>> = None;
    let mut finish_reason = String::new();
    let mut usage: Option<crate::types::TokenUsage> = None;

    while let Some(event) = stream.next().await {
        match event {
            LlmEvent::Token { content: token } => content.push_str(&token),
            LlmEvent::Reasoning { content: r } => reasoning_content.push_str(&r),
            LlmEvent::ToolCallStart { id, name } => {
                let calls = tool_calls.get_or_insert_with(Vec::new);
                calls.push(crate::types::ToolCall {
                    id,
                    function: crate::types::FunctionCall {
                        name,
                        arguments: String::new(),
                    },
                });
            }
            LlmEvent::ToolCallDelta {
                id,
                arguments_delta,
            } => {
                if let Some(calls) = &mut tool_calls {
                    if let Some(call) = calls.iter_mut().find(|c| c.id == id) {
                        call.function.arguments.push_str(&arguments_delta);
                    }
                }
            }
            LlmEvent::Done {
                finish_reason: fr,
                usage: u,
            } => {
                finish_reason = fr;
                usage = u;
            }
            LlmEvent::Error { message } => {
                return Err(LlmError::Other(message));
            }
            _ => {}
        }
    }

    Ok(ChatResponse {
        content,
        reasoning_content: if reasoning_content.is_empty() {
            None
        } else {
            Some(reasoning_content)
        },
        tool_calls,
        finish_reason: if finish_reason.is_empty() {
            "stop".to_string()
        } else {
            finish_reason
        },
        model,
        usage,
    })
}

/// Convenience function: stream a completion through the real client.
pub async fn stream(request: ChatRequest) -> Result<LlmStream, LlmError> {
    let resolved = ModelRouter::new().resolve(&request.target)?;
    crate::client::dispatch_stream(request, resolved).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, ModelTarget};
    use futures::StreamExt;

    fn make_request(text: &str) -> ChatRequest {
        ChatRequest::new(vec![Message::user(text)], ModelTarget::default())
    }

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
        let stub = StubLlm::new();
        let req = make_request("hello");
        let resp = stub.chat(req).await.unwrap();
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
            matches!(&events[events.len() - 1], LlmEvent::Done { .. }),
            "last event should be Done, got {:?}",
            events.last()
        );

        let has_token = events.iter().any(|e| matches!(e, LlmEvent::Token { .. }));
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
        assert!(
            !token_events.is_empty(),
            "should have at least one Token event"
        );
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

    #[tokio::test]
    async fn chat_collects_stream() {
        let stub = StubLlm::new();
        let req = make_request("hello world");
        let resp = stub.chat(req).await.unwrap();
        assert_eq!(resp.content, "stub response: hello world");
        assert_eq!(resp.finish_reason, "stop");
        assert_eq!(resp.model, "sonnet");
        assert!(resp.usage.is_none());
    }

    #[tokio::test]
    async fn chat_still_works_for_empty_messages() {
        let stub = StubLlm::new();
        let req = ChatRequest::new(vec![], ModelTarget::default());
        let resp = stub.chat(req).await.unwrap();
        assert_eq!(resp.content, "stub response");
    }

    #[tokio::test]
    async fn stream_resolves_model_before_yielding() {
        let stub = StubLlm::new();
        let req = make_request("test");
        let stream = stub.stream(req).unwrap();
        let events: Vec<LlmEvent> = stream.collect().await;
        let last = events.last().unwrap();
        assert!(
            matches!(last, LlmEvent::Done { finish_reason, .. } if finish_reason == "stop"),
            "Done should have finish_reason 'stop', got {:?}",
            last
        );
    }

    #[test]
    fn stub_llm_no_lattice_core_references() {
        let stub = StubLlm::new();
        assert_eq!(stub.router.default_model, Some("sonnet".to_string()));
    }
}
