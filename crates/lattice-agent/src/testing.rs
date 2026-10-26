// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Feature-gated test doubles for the agent-layer LLM client seam.
//!
//! This module is available only with the `test-utils` feature so downstream
//! crates can opt in from dev-dependencies without exposing the fake client in
//! production builds.

use crate::LlmClient;
use lattice_llm::testing::{ScriptEvent, script_stream};
use lattice_llm::{ChatRequest, LlmStream, StreamControl};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

enum FakeStreamingResponse {
    Events(Vec<ScriptEvent>),
    Error(String),
    LlmError(lattice_llm::LlmError),
}

/// Deterministic streaming LLM test double.
///
/// Responses are queued FIFO. `push_error` creates a startup-style
/// `Result::Err`; `ScriptEvent::Failure` inside `push_events` remains an in-band
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
    pub fn push_events(mut self, events: Vec<ScriptEvent>) -> Self {
        self.queue.push_back(FakeStreamingResponse::Events(events));
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

#[async_trait::async_trait]
impl LlmClient for FakeStreamingLlmClient {
    async fn stream_chat(
        &self,
        _request: ChatRequest,
        control: StreamControl,
    ) -> Result<LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        let response = self.queue.lock().unwrap().pop_front();

        match response {
            None => Err("no more fake responses".into()),
            Some(FakeStreamingResponse::Error(message)) => Err(message.into()),
            Some(FakeStreamingResponse::LlmError(error)) => Err(Box::new(error)),
            Some(FakeStreamingResponse::Events(events)) => Ok(script_stream(events, control)),
        }
    }
}
