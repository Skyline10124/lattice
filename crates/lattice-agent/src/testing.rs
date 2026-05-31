//! Feature-gated test doubles for the agent-layer LLM client seam.
//!
//! This module is available only with the `test-utils` feature so downstream
//! crates can opt in from dev-dependencies without exposing the fake client in
//! production builds.

use crate::LlmClient;
use lattice_llm::{ChatRequest, ChatResponse, FunctionCall, LlmEvent, LlmStream, ToolCall};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

enum FakeStreamingResponse {
    Events(Vec<LlmEvent>),
    Error(String),
}

/// Deterministic streaming LLM test double.
///
/// Responses are queued FIFO. `push_error` creates a startup-style
/// `Result::Err`; `LlmEvent::Error` inside `push_events` remains an in-band
/// stream item so tests can distinguish startup and mid-stream failures.
pub struct FakeStreamingLlmClient {
    queue: Mutex<VecDeque<FakeStreamingResponse>>,
    call_count: AtomicUsize,
}

/// Builder for [`FakeStreamingLlmClient`].
pub struct FakeStreamingLlmClientBuilder {
    queue: VecDeque<FakeStreamingResponse>,
}

impl FakeStreamingLlmClient {
    pub fn builder() -> FakeStreamingLlmClientBuilder {
        FakeStreamingLlmClientBuilder {
            queue: VecDeque::new(),
        }
    }

    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

impl FakeStreamingLlmClientBuilder {
    pub fn push_events(mut self, events: Vec<LlmEvent>) -> Self {
        self.queue.push_back(FakeStreamingResponse::Events(events));
        self
    }

    pub fn push_error(mut self, message: impl Into<String>) -> Self {
        self.queue
            .push_back(FakeStreamingResponse::Error(message.into()));
        self
    }

    pub fn build(self) -> FakeStreamingLlmClient {
        FakeStreamingLlmClient {
            queue: Mutex::new(self.queue),
            call_count: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for FakeStreamingLlmClient {
    async fn chat(
        &self,
        request: ChatRequest,
    ) -> Result<ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        let response = self.queue.lock().unwrap().pop_front();

        match response {
            None => Err("no more fake responses".into()),
            Some(FakeStreamingResponse::Error(message)) => Err(message.into()),
            Some(FakeStreamingResponse::Events(events)) => {
                aggregate_events_to_chat_response(events, &request).map_err(Into::into)
            }
        }
    }

    async fn stream_chat(
        &self,
        _request: ChatRequest,
    ) -> Result<LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        let response = self.queue.lock().unwrap().pop_front();

        match response {
            None => Err("no more fake responses".into()),
            Some(FakeStreamingResponse::Error(message)) => Err(message.into()),
            Some(FakeStreamingResponse::Events(events)) => {
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }
    }
}

/// Aggregates provider-stream events into a buffered chat response.
///
/// The helper mirrors the production stub chat aggregation contract: content,
/// reasoning, tool calls, final finish reason, and final usage are folded into
/// a `ChatResponse`; provider metadata events are ignored.
fn aggregate_events_to_chat_response(
    events: Vec<LlmEvent>,
    request: &ChatRequest,
) -> Result<ChatResponse, String> {
    let mut content = String::new();
    let mut reasoning_content = String::new();
    let mut tool_calls: Option<Vec<ToolCall>> = None;
    let mut finish_reason = String::new();
    let mut usage = None;

    for event in events {
        match event {
            LlmEvent::Token { content: token } => content.push_str(&token),
            LlmEvent::Reasoning { content: reasoning } => {
                reasoning_content.push_str(&reasoning);
            }
            LlmEvent::ToolCallStart { id, name } => {
                let calls = tool_calls.get_or_insert_with(Vec::new);
                calls.push(ToolCall {
                    id,
                    function: FunctionCall {
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
                    if let Some(call) = calls.iter_mut().find(|call| call.id == id) {
                        call.function.arguments.push_str(&arguments_delta);
                    }
                }
            }
            LlmEvent::ToolCallEnd { .. } => {}
            LlmEvent::Done {
                finish_reason: reason,
                usage: token_usage,
            } => {
                finish_reason = reason;
                usage = token_usage;
            }
            LlmEvent::Error { message } => return Err(message),
            LlmEvent::Started
            | LlmEvent::Citation { .. }
            | LlmEvent::Signature { .. }
            | LlmEvent::GroundingChunk { .. }
            | LlmEvent::SafetyRating { .. } => {}
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
        // Intentional StubLlm/chat parity: missing Done defaults to "stop".
        // streaming streaming consumers must treat this as Error instead —
        // do not emit ModelFinished on incomplete streams.
        finish_reason: if finish_reason.is_empty() {
            "stop".to_string()
        } else {
            finish_reason
        },
        model: request.target.model.clone(),
        usage,
    })
}
