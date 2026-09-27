// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Feature-gated test doubles for the agent-layer LLM client seam.
//!
//! This module is available only with the `test-utils` feature so downstream
//! crates can opt in from dev-dependencies without exposing the fake client in
//! production builds.

use crate::LlmClient;
use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, ProviderOrigin, RawToolCall,
    validate_tool_call_batch,
};
use lattice_llm::{ChatRequest, LlmEvent, LlmStream};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

enum FakeStreamingResponse {
    Events(Vec<LlmEvent>),
    Error(String),
    LlmError(lattice_llm::LlmError),
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
        self.queue
            .push_back(FakeStreamingResponse::Events(with_complete_message(events)));
        self
    }

    pub fn push_error(mut self, message: impl Into<String>) -> Self {
        self.queue
            .push_back(FakeStreamingResponse::Error(message.into()));
        self
    }

    pub fn push_llm_error(mut self, error: lattice_llm::LlmError) -> Self {
        self.queue.push_back(FakeStreamingResponse::LlmError(error));
        self
    }

    pub fn build(self) -> FakeStreamingLlmClient {
        FakeStreamingLlmClient {
            queue: Mutex::new(self.queue),
            call_count: AtomicUsize::new(0),
        }
    }
}

fn with_complete_message(mut events: Vec<LlmEvent>) -> Vec<LlmEvent> {
    if !events
        .iter()
        .any(|event| matches!(event, LlmEvent::Done { .. }))
        || events.iter().any(|event| {
            matches!(
                event,
                LlmEvent::MessageComplete { .. }
                    | LlmEvent::MessageRejected { .. }
                    | LlmEvent::Error { .. }
                    | LlmEvent::OutputLimit { .. }
            )
        })
    {
        return events;
    }

    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = std::collections::BTreeMap::<usize, RawToolCall>::new();
    for event in &events {
        match event {
            LlmEvent::Token { content } => text.push_str(content),
            LlmEvent::Reasoning { content } => reasoning.push_str(content),
            LlmEvent::ToolCallStart {
                index,
                provider_call_id,
                name,
            } => {
                calls.insert(
                    *index,
                    RawToolCall {
                        provider_call_id: provider_call_id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                    },
                );
            }
            LlmEvent::ToolCallDelta {
                index,
                arguments_delta,
            } => {
                if let Some(call) = calls.get_mut(index) {
                    call.arguments.push_str(arguments_delta);
                }
            }
            _ => {}
        }
    }

    let raw_calls: Vec<_> = calls.into_values().collect();
    let completion = match validate_tool_call_batch(&raw_calls) {
        Ok(calls) => {
            let mut parts = Vec::new();
            if !reasoning.is_empty() {
                parts.push(MessagePart::semantic_only(MessageSemantic::Reasoning {
                    text: reasoning,
                }));
            }
            if !text.is_empty() {
                parts.push(MessagePart::semantic_only(MessageSemantic::Text { text }));
            }
            parts.extend(
                calls
                    .into_iter()
                    .map(|call| MessagePart::semantic_only(MessageSemantic::ToolCall { call })),
            );
            LlmEvent::MessageComplete {
                message: NativeMessage::new(
                    MessageRole::Assistant,
                    Some(ProviderOrigin::OpenAiCompatible),
                    parts,
                ),
            }
        }
        Err(validation) => LlmEvent::MessageRejected {
            rejection: lattice_conversation::AttemptRejection::InvalidToolCall { validation },
        },
    };

    let done = events
        .iter()
        .position(|event| matches!(event, LlmEvent::Done { .. }))
        .expect("Done checked above");
    events.insert(done, completion);
    events
}

#[async_trait::async_trait]
impl LlmClient for FakeStreamingLlmClient {
    async fn stream_chat(
        &self,
        _request: ChatRequest,
    ) -> Result<LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        let response = self.queue.lock().unwrap().pop_front();

        match response {
            None => Err("no more fake responses".into()),
            Some(FakeStreamingResponse::Error(message)) => Err(message.into()),
            Some(FakeStreamingResponse::LlmError(error)) => Err(Box::new(error)),
            Some(FakeStreamingResponse::Events(events)) => {
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }
    }
}
