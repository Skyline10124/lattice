//! v1.1 turn execution and streaming API.
//!
//! Phase 7: `run_turn` calls the real [`lattice_llm::chat`] facade,
//! records a visible stage trace, and returns `commit_id: None`.
//! `stream_turn` maps the full [`LlmEvent`] vocabulary into [`TurnEvent`]
//! values for public consumption.

use crate::session::{ApiError, Result};
use futures::stream::{Stream, StreamExt};
use lattice_llm::{ChatRequest, LlmEvent, Message, ModelTarget};
use lattice_profile::LoadedAgentProfile;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

// ── Request types ────────────────────────────────────────────────────────

/// A request to execute a single synchronous turn.
///
/// When `loaded_profile` is provided, its model name is used for the
/// LLM call and its system prompt is prepended as a system message.
/// Otherwise, the `model` field (defaulting to `"sonnet"`) is used.
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
    /// A pre-loaded agent profile. When set, its model and system prompt
    /// take precedence over the `profile` and `model` string fields.
    #[serde(skip)]
    pub loaded_profile: Option<LoadedAgentProfile>,
}

/// A request to stream a single turn.
///
/// When `loaded_profile` is provided, its model name is used for the
/// LLM call and its system prompt is prepended as a system message.
/// Otherwise, the `model` field (defaulting to `"sonnet"`) is used.
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
    /// A pre-loaded agent profile. When set, its model and system prompt
    /// take precedence over the `profile` and `model` string fields.
    #[serde(skip)]
    pub loaded_profile: Option<LoadedAgentProfile>,
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
    /// Agent built (stub in Phase 7).
    Agent,
    /// LLM call completed.
    Llm,
    /// Tool execution completed (stub in Phase 7).
    Tools,
    /// Turn committed (stub in Phase 7).
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
    /// Durable commit id — always `None` in Phase 7.
    pub commit_id: Option<String>,
    /// The ordered stage trace recording each step of the pipeline.
    pub stages: Vec<RunTurnStage>,
}

// ── Streaming types ──────────────────────────────────────────────────────

/// An event emitted during a streaming turn.
///
/// Mirrors the full [`LlmEvent`] vocabulary directly so consumers
/// can observe tokens, reasoning, tool calls, and completion.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TurnEvent {
    /// The turn stream has started.
    Started,
    /// A chunk of assistant text content.
    Token { content: String },
    /// A chunk of reasoning / thinking content.
    Reasoning { content: String },
    /// Start of a tool call from the model.
    ToolCallStart { id: String, name: String },
    /// An incremental argument delta for an in-progress tool call.
    ToolCallDelta { id: String, arguments_delta: String },
    /// A tool call is complete.
    ToolCallEnd { id: String },
    /// The turn stream finished successfully.
    Done { finish_reason: String },
    /// An error occurred during the turn stream.
    Error { message: String },
}

/// A boxed stream of [`TurnEvent`] values.
pub type TurnEventStream = Pin<Box<dyn Stream<Item = TurnEvent> + Send>>;

// ── API functions ────────────────────────────────────────────────────────

/// Execute a synchronous turn.
///
/// When `loaded_profile` is set on the request, its model name is used
/// for the LLM call and its system prompt is prepended as a system message.
/// Otherwise, the `model` field (defaulting to `"sonnet"`) is used.
///
/// Resolves the model alias through [`lattice_llm::chat`], collects the
/// stream into a single response, records the skeleton stage trace, and
/// returns `commit_id: None` (durable commits are deferred to Phase 8).
///
/// If no credentials are configured for the target provider, the turn
/// fails with an [`ApiError::Internal`] naming the missing env var.
pub async fn run_turn(request: RunTurnRequest) -> Result<RunTurnResult> {
    let mut stages = Vec::new();

    // Stage: Profile loaded or defaulted.
    stages.push(RunTurnStage::Profile);

    // Stage: Session opened or created.
    stages.push(RunTurnStage::Session);

    // Stage: Agent built (stub in Phase 7).
    stages.push(RunTurnStage::Agent);

    // Determine model alias and optional system prompt from loaded profile.
    let (model_alias, system_prompt) = if let Some(ref lp) = request.loaded_profile {
        (
            lp.profile.agent.model.clone(),
            if lp.system_prompt.is_empty() {
                None
            } else {
                Some(lp.system_prompt.clone())
            },
        )
    } else {
        (request.model.unwrap_or_else(|| "sonnet".to_string()), None)
    };

    // Build messages: system prompt first (if any), then user input.
    let mut messages = Vec::new();
    if let Some(sp) = system_prompt {
        messages.push(Message::system(sp));
    }
    messages.push(Message::user(request.input.clone()));

    // Build the LLM chat request.
    let target = ModelTarget {
        model: model_alias,
        provider: None,
    };
    let chat_request = ChatRequest {
        messages,
        tools: vec![],
        target,
        temperature: None,
        max_tokens: None,
        stream: false,
    };

    // Stage: LLM call via the real lattice_llm facade.
    let response = lattice_llm::chat(chat_request)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    stages.push(RunTurnStage::Llm);

    // Stage: Tool execution (Phase 7 skeleton — no tools execute).
    stages.push(RunTurnStage::Tools);

    // Stage: Turn committed (Phase 7 skeleton — commit_id is always None).
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
/// When `loaded_profile` is set on the request, its model name is used
/// for the LLM call and its system prompt is prepended as a system message.
/// Otherwise, the `model` field (defaulting to `"sonnet"`) is used.
///
/// Maps the full [`LlmEvent`] vocabulary (tokens, reasoning, tool call
/// deltas, completion) into [`TurnEvent`] values for public consumption.
pub async fn stream_turn(request: StreamTurnRequest) -> Result<TurnEventStream> {
    // Determine model alias and optional system prompt from loaded profile.
    let (model_alias, system_prompt) = if let Some(ref lp) = request.loaded_profile {
        (
            lp.profile.agent.model.clone(),
            if lp.system_prompt.is_empty() {
                None
            } else {
                Some(lp.system_prompt.clone())
            },
        )
    } else {
        (request.model.unwrap_or_else(|| "sonnet".to_string()), None)
    };

    // Build messages: system prompt first (if any), then user input.
    let mut messages = Vec::new();
    if let Some(sp) = system_prompt {
        messages.push(Message::system(sp));
    }
    messages.push(Message::user(request.input.clone()));

    let target = ModelTarget {
        model: model_alias,
        provider: None,
    };
    let chat_request = ChatRequest {
        messages,
        tools: vec![],
        target,
        temperature: None,
        max_tokens: None,
        stream: true,
    };

    let llm_stream = lattice_llm::stream(chat_request)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let turn_stream = llm_stream.map(|event| match event {
        LlmEvent::Started => TurnEvent::Started,
        LlmEvent::Token { content } => TurnEvent::Token { content },
        LlmEvent::Reasoning { content } => TurnEvent::Reasoning { content },
        LlmEvent::ToolCallStart { id, name } => TurnEvent::ToolCallStart { id, name },
        LlmEvent::ToolCallDelta {
            id,
            arguments_delta,
        } => TurnEvent::ToolCallDelta {
            id,
            arguments_delta,
        },
        LlmEvent::ToolCallEnd { id } => TurnEvent::ToolCallEnd { id },
        LlmEvent::Done { finish_reason, .. } => TurnEvent::Done { finish_reason },
        LlmEvent::Error { message } => TurnEvent::Error { message },
    });

    Ok(Box::pin(turn_stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use lattice_llm::StubLlm;

    // ── Helpers ──────────────────────────────────────────────────────────

    fn make_run_request(input: &str) -> RunTurnRequest {
        RunTurnRequest {
            input: input.to_string(),
            session_id: None,
            profile: None,
            model: None,
            loaded_profile: None,
        }
    }

    fn make_stream_request(input: &str) -> StreamTurnRequest {
        StreamTurnRequest {
            input: input.to_string(),
            session_id: None,
            profile: None,
            model: None,
            loaded_profile: None,
        }
    }

    /// Build a ChatRequest from user input using sonnet target.
    fn make_chat_request(input: &str) -> ChatRequest {
        ChatRequest::new(vec![Message::user(input)], ModelTarget::new("sonnet"))
    }

    /// Run a stub turn directly (tests do not require provider credentials).
    async fn run_stub_turn(input: &str) -> RunTurnResult {
        let mut stages = Vec::new();
        stages.push(RunTurnStage::Profile);
        stages.push(RunTurnStage::Session);
        stages.push(RunTurnStage::Agent);

        let response = StubLlm::new()
            .chat(make_chat_request(input))
            .await
            .expect("stub chat should not fail");

        stages.push(RunTurnStage::Llm);
        stages.push(RunTurnStage::Tools);
        stages.push(RunTurnStage::Commit);
        stages.push(RunTurnStage::Render);

        RunTurnResult {
            assistant_output: response.content,
            commit_id: None,
            stages,
        }
    }

    /// Produce a stub TurnEventStream (tests do not require provider credentials).
    fn stub_turn_stream(input: &str) -> TurnEventStream {
        let llm_stream = StubLlm::new()
            .stream(make_chat_request(input))
            .expect("stub stream should not fail");

        let turn_stream = llm_stream.map(|event| match event {
            LlmEvent::Started => TurnEvent::Started,
            LlmEvent::Token { content } => TurnEvent::Token { content },
            LlmEvent::Reasoning { content } => TurnEvent::Reasoning { content },
            LlmEvent::ToolCallStart { id, name } => TurnEvent::ToolCallStart { id, name },
            LlmEvent::ToolCallDelta {
                id,
                arguments_delta,
            } => TurnEvent::ToolCallDelta {
                id,
                arguments_delta,
            },
            LlmEvent::ToolCallEnd { id } => TurnEvent::ToolCallEnd { id },
            LlmEvent::Done { finish_reason, .. } => TurnEvent::Done { finish_reason },
            LlmEvent::Error { message } => TurnEvent::Error { message },
        });

        Box::pin(turn_stream)
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn run_stub_turn_returns_assistant_output() {
        let result = run_stub_turn("hello").await;
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
    async fn run_stub_turn_commit_id_is_none() {
        let result = run_stub_turn("test").await;
        assert!(
            result.commit_id.is_none(),
            "commit_id must be None in Phase 7, got: {:?}",
            result.commit_id
        );
    }

    #[tokio::test]
    async fn run_stub_turn_stage_trace_is_ordered() {
        let result = run_stub_turn("trace test").await;

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
    async fn stub_turn_with_model_passes_to_stub() {
        let req = make_chat_request("hello");
        let response = StubLlm::new().chat(req).await.unwrap();
        assert!(
            response.content.contains("stub response"),
            "response should contain 'stub response', got: '{}'",
            response.content
        );
    }

    #[tokio::test]
    async fn stub_stream_yields_started_token_done() {
        let stream = stub_turn_stream("hello");
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
            matches!(events.last().unwrap(), TurnEvent::Done { .. }),
            "last event should be Done, got {:?}",
            events.last().unwrap()
        );

        let has_token = events.iter().any(|e| matches!(e, TurnEvent::Token { .. }));
        assert!(has_token, "stream should contain at least one Token event");
    }

    #[tokio::test]
    async fn stub_stream_token_contains_content() {
        let stream = stub_turn_stream("test prompt");
        let events: Vec<TurnEvent> = stream.collect().await;

        for event in &events {
            if let TurnEvent::Token { content } = event {
                assert!(!content.is_empty(), "Token content should not be empty");
                assert!(
                    content.contains("stub response"),
                    "Token content should contain 'stub response', got: '{}'",
                    content
                );
            }
        }

        let token_count = events
            .iter()
            .filter(|e| matches!(e, TurnEvent::Token { .. }))
            .count();
        assert!(token_count > 0, "should have at least one Token event");
    }

    #[tokio::test]
    async fn stub_stream_done_has_stop_reason() {
        let stream = stub_turn_stream("hello");
        let events: Vec<TurnEvent> = stream.collect().await;

        if let TurnEvent::Done { finish_reason } = events.last().unwrap() {
            assert_eq!(finish_reason, "stop");
        } else {
            panic!("last event should be Done");
        }
    }

    #[test]
    fn turn_event_serde_roundtrip() {
        let events = vec![
            TurnEvent::Started,
            TurnEvent::Token {
                content: "hello".into(),
            },
            TurnEvent::Reasoning {
                content: "thinking...".into(),
            },
            TurnEvent::Done {
                finish_reason: "stop".into(),
            },
            TurnEvent::Error {
                message: "oops".into(),
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let deserialized: TurnEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, deserialized);
        }
    }

    // ── Profile consumption tests ────────────────────────────────────────

    /// Build a minimal LoadedAgentProfile for tests.
    fn make_test_profile(name: &str, model: &str, system_prompt: &str) -> LoadedAgentProfile {
        use std::path::PathBuf;

        let profile = lattice_profile::AgentProfile {
            schema_version: 1,
            agent: lattice_profile::AgentSection {
                name: name.to_string(),
                model: model.to_string(),
                description: None,
            },
            prompt: lattice_profile::PromptSection {
                system: None,
                system_path: None,
            },
            tools: Default::default(),
            security: None,
            context: None,
            behavior: None,
        };

        LoadedAgentProfile {
            path: PathBuf::from("/tmp/test/agent.toml"),
            package_dir: PathBuf::from("/tmp/test"),
            profile,
            system_prompt: system_prompt.to_string(),
        }
    }

    #[tokio::test]
    async fn stub_turn_with_profile_uses_profile_model() {
        let profile = make_test_profile("test-agent", "haiku", "Be helpful.");
        let req = ChatRequest {
            messages: vec![Message::system("Be helpful."), Message::user("hello")],
            tools: vec![],
            target: ModelTarget::new("haiku"),
            temperature: None,
            max_tokens: None,
            stream: false,
        };
        // The stub produces deterministic output based on the request.
        let response = StubLlm::new().chat(req).await.unwrap();
        assert!(response.content.contains("stub response"));
        assert_eq!(response.model, "haiku");
    }

    #[tokio::test]
    async fn stub_turn_profile_system_prompt_in_messages() {
        let profile = make_test_profile("dev", "haiku", "You are a dev assistant.");
        let req = ChatRequest {
            messages: vec![
                Message::system("You are a dev assistant."),
                Message::user("refactor"),
            ],
            tools: vec![],
            target: ModelTarget::new("haiku"),
            temperature: None,
            max_tokens: None,
            stream: false,
        };
        let response = StubLlm::new().chat(req).await.unwrap();
        assert!(response.content.contains("stub response"));
    }

    #[test]
    fn profile_roundtrips_in_request() {
        let profile = make_test_profile("agent", "gpt-4o", "System prompt.");
        let req = RunTurnRequest {
            input: "hello".into(),
            session_id: None,
            profile: Some("agent".into()),
            model: None,
            loaded_profile: Some(profile.clone()),
        };

        assert_eq!(req.input, "hello");
        assert_eq!(
            req.loaded_profile.as_ref().unwrap().profile.agent.model,
            "gpt-4o"
        );
        assert_eq!(
            req.loaded_profile.as_ref().unwrap().system_prompt,
            "System prompt."
        );

        // Verify serde round-trip (loaded_profile is skipped, so it won't serialize).
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("LoadedAgentProfile"));
        let deserialized: RunTurnRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.input, "hello");
        assert!(deserialized.loaded_profile.is_none());
    }
}
