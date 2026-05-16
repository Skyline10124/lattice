//! v1.1 turn execution and streaming API.
//!
//! Phase 6 skeleton: `run_turn` calls the deterministic [`lattice_llm::StubLlm`],
//! records a visible stage trace, and returns `commit_id: None`. `stream_turn`
//! yields fake `Started` / `Token` / `Done` events.

use crate::session::{ApiError, Result};
use futures::stream::{Stream, StreamExt};
use lattice_llm::{ChatRequest, LlmEvent, Message, ModelTarget, StubLlm};
use serde::{Deserialize, Serialize};
use std::pin::Pin;

// ── Request types ────────────────────────────────────────────────────────

/// A request to execute a single synchronous turn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunTurnRequest {
    /// The user's input text.
    pub input: String,
    /// Optional session id to associate the turn with.
    pub session_id: Option<String>,
    /// Optional profile name override.
    pub profile: Option<String>,
    /// Optional model alias override.
    pub model: Option<String>,
}

/// A request to stream a single turn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StreamTurnRequest {
    /// The user's input text.
    pub input: String,
    /// Optional session id to associate the turn with.
    pub session_id: Option<String>,
    /// Optional profile name override.
    pub profile: Option<String>,
    /// Optional model alias override.
    pub model: Option<String>,
}

// ── Result types ─────────────────────────────────────────────────────────

/// A stage in the main path execution pipeline.
///
/// These stages make the profile -> session -> agent -> llm -> tools ->
/// commit -> render skeleton trace observable.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunTurnStage {
    /// Profile loaded or defaulted.
    Profile,
    /// Session opened or created.
    Session,
    /// Agent built (stub in Phase 6).
    Agent,
    /// LLM call completed.
    Llm,
    /// Tool execution completed (stub in Phase 6).
    Tools,
    /// Turn committed (stub in Phase 6).
    Commit,
    /// Result rendered.
    Render,
}

impl std::fmt::Display for RunTurnStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunTurnStage::Profile => write!(f, "profile"),
            RunTurnStage::Session => write!(f, "session"),
            RunTurnStage::Agent => write!(f, "agent"),
            RunTurnStage::Llm => write!(f, "llm"),
            RunTurnStage::Tools => write!(f, "tools"),
            RunTurnStage::Commit => write!(f, "commit"),
            RunTurnStage::Render => write!(f, "render"),
        }
    }
}

/// The result of executing a synchronous turn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunTurnResult {
    /// The assistant's text output.
    pub assistant_output: String,
    /// Durable commit id — always `None` in Phase 6.
    pub commit_id: Option<String>,
    /// The ordered stage trace recording each step of the pipeline.
    pub stages: Vec<RunTurnStage>,
}

// ── Streaming types ──────────────────────────────────────────────────────

/// An event emitted during a streaming turn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TurnEvent {
    /// The turn stream has started.
    Started,
    /// A chunk of assistant text content.
    Token { content: String },
    /// The turn stream is finished.
    Done,
    /// An error occurred during the turn stream.
    Error { message: String },
}

/// A boxed stream of [`TurnEvent`] values.
pub type TurnEventStream = Pin<Box<dyn Stream<Item = TurnEvent> + Send>>;

// ── API functions ────────────────────────────────────────────────────────

/// Execute a synchronous turn.
///
/// Calls the Phase 6 [`StubLlm`] to produce a deterministic assistant
/// response, records the skeleton stage trace, and returns
/// `commit_id: None` (durable commits are deferred to Phase 8).
pub async fn run_turn(request: RunTurnRequest) -> Result<RunTurnResult> {
    let mut stages = Vec::new();

    // Stage: Profile loaded or defaulted.
    stages.push(RunTurnStage::Profile);

    // Stage: Session opened or created.
    stages.push(RunTurnStage::Session);

    // Stage: Agent built (stub in Phase 6).
    stages.push(RunTurnStage::Agent);

    // Build the LLM chat request from user input.
    let target = ModelTarget {
        model: request.model.unwrap_or_else(|| "sonnet".to_string()),
        provider: None,
    };
    let chat_request = ChatRequest {
        messages: vec![Message::user(request.input.clone())],
        tools: vec![],
        target,
        temperature: None,
        max_tokens: None,
        stream: false,
    };

    // Stage: LLM call (stub in Phase 6).
    let response = StubLlm::new()
        .chat(chat_request)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    stages.push(RunTurnStage::Llm);

    // Stage: Tool execution (Phase 6 skeleton — no tools execute).
    stages.push(RunTurnStage::Tools);

    // Stage: Turn committed (Phase 6 skeleton — commit_id is always None).
    stages.push(RunTurnStage::Commit);

    // Stage: Result rendered.
    stages.push(RunTurnStage::Render);

    Ok(RunTurnResult {
        assistant_output: response.content,
        commit_id: None,
        stages,
    })
}

/// Execute a streaming turn, yielding [`TurnEvent`] values.
///
/// Produces a fake `Started` / `Token` / `Done` event sequence by mapping
/// the [`StubLlm`] stream shape to API events.
pub fn stream_turn(request: StreamTurnRequest) -> Result<TurnEventStream> {
    let target = ModelTarget {
        model: request.model.unwrap_or_else(|| "sonnet".to_string()),
        provider: None,
    };
    let chat_request = ChatRequest {
        messages: vec![Message::user(request.input.clone())],
        tools: vec![],
        target,
        temperature: None,
        max_tokens: None,
        stream: true,
    };

    let llm_stream = StubLlm::new()
        .stream(chat_request)
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let turn_stream = llm_stream.map(|event| match event {
        LlmEvent::Started => TurnEvent::Started,
        LlmEvent::Token { content } => TurnEvent::Token { content },
        LlmEvent::Done => TurnEvent::Done,
        LlmEvent::Error { message } => TurnEvent::Error { message },
    });

    Ok(Box::pin(turn_stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    // ── Helpers ──────────────────────────────────────────────────────────

    fn make_run_request(input: &str) -> RunTurnRequest {
        RunTurnRequest {
            input: input.to_string(),
            session_id: None,
            profile: None,
            model: None,
        }
    }

    fn make_stream_request(input: &str) -> StreamTurnRequest {
        StreamTurnRequest {
            input: input.to_string(),
            session_id: None,
            profile: None,
            model: None,
        }
    }

    // ── RED tests (will fail until GREEN implementations are in place) ──

    #[tokio::test]
    async fn run_turn_returns_assistant_output() {
        let req = make_run_request("hello");
        let result = run_turn(req).await.unwrap();
        assert!(
            !result.assistant_output.is_empty(),
            "assistant_output should not be empty"
        );
        assert!(
            result.assistant_output.contains("stub response"),
            "assistant_output should contain 'stub response', got: '{}'",
            result.assistant_output
        );
    }

    #[tokio::test]
    async fn run_turn_commit_id_is_none() {
        let req = make_run_request("test");
        let result = run_turn(req).await.unwrap();
        assert!(
            result.commit_id.is_none(),
            "commit_id must be None in Phase 6, got: {:?}",
            result.commit_id
        );
    }

    #[tokio::test]
    async fn run_turn_stage_trace_is_ordered() {
        let req = make_run_request("trace test");
        let result = run_turn(req).await.unwrap();

        let expected = vec![
            RunTurnStage::Profile,
            RunTurnStage::Session,
            RunTurnStage::Agent,
            RunTurnStage::Llm,
            RunTurnStage::Tools,
            RunTurnStage::Commit,
            RunTurnStage::Render,
        ];
        assert_eq!(
            result.stages, expected,
            "stage trace should be profile -> session -> agent -> llm -> tools -> commit -> render"
        );
    }

    #[tokio::test]
    async fn run_turn_with_model_passes_to_stub() {
        let req = RunTurnRequest {
            input: "hello".to_string(),
            session_id: None,
            profile: None,
            model: Some("gpt-4o".to_string()),
        };
        let result = run_turn(req).await.unwrap();
        // The stub response should contain the input text (deterministic).
        assert!(
            result.assistant_output.contains("stub response"),
            "assistant_output should contain 'stub response'"
        );
    }

    #[tokio::test]
    async fn stream_turn_yields_started_token_done() {
        let req = make_stream_request("hello");
        let stream = stream_turn(req).unwrap();
        let events: Vec<TurnEvent> = stream.collect().await;

        assert!(
            events.len() >= 3,
            "stream should yield at least 3 events, got {}",
            events.len()
        );
        assert!(
            matches!(events[0], TurnEvent::Started),
            "first event should be Started, got {:?}",
            events[0]
        );
        assert!(
            matches!(events.last().unwrap(), TurnEvent::Done),
            "last event should be Done, got {:?}",
            events.last().unwrap()
        );

        let has_token = events
            .iter()
            .any(|e| matches!(e, TurnEvent::Token { .. }));
        assert!(has_token, "stream should contain at least one Token event");
    }

    #[tokio::test]
    async fn stream_turn_token_contains_content() {
        let req = make_stream_request("test prompt");
        let stream = stream_turn(req).unwrap();
        let events: Vec<TurnEvent> = stream.collect().await;

        for event in &events {
            if let TurnEvent::Token { content } = event {
                assert!(
                    !content.is_empty(),
                    "Token content should not be empty"
                );
                assert!(
                    content.contains("stub response"),
                    "Token content should contain 'stub response', got: '{}'",
                    content
                );
            }
        }

        // Confirm there was at least one token.
        let token_count = events
            .iter()
            .filter(|e| matches!(e, TurnEvent::Token { .. }))
            .count();
        assert!(token_count > 0, "should have at least one Token event");
    }
}
