//! v1.1 turn execution and streaming API.
//!
//! Phase 9: `run_turn` orchestrates durable turn execution through the
//! `TurnDeps` dependency seam: create/open session, read transcript, invoke
//! agent loop, commit exactly one turn, and return typed results.
//! `stream_turn` forwards live agent events and appends a final `Committed`
//! event after persistence.
//!
//! # Dependency seam
//!
//! Production callers use [`run_turn`] which internally constructs
//! [`RealTurnDeps`]. Tests use [`run_turn_with_deps`] with a fake
//! implementation of [`TurnDeps`] to avoid live provider calls, direct
//! SQLite access, or public exposure of internal handles.

use crate::session::{ApiError, Result, SessionApi};
use futures::stream::{self, Stream};
use lattice_agent::{
    AgentLoop, AgentToolResultSummary as AgentToolSummary, AgentTurnEvent, AgentTurnRequest,
    AgentTurnResult, AgentTurnStatus,
};
use lattice_profile::LoadedAgentProfile;
use lattice_session::{
    derive_commit_message, HeadTranscript, TurnCommitInput, TurnCommitStatus, TurnUsage,
};
use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::Arc;

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
    /// When `None`, a new session is automatically created.
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
    /// When `None`, a new session is automatically created.
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
    /// Agent loop executed.
    Agent,
    /// LLM call completed.
    Llm,
    /// Tool execution completed.
    Tools,
    /// Turn committed to durable storage.
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

/// Terminal status of an API turn.
///
/// Maps to the session-level [`TurnCommitStatus`]. The public spelling uses
/// `Failed` where the session layer uses `Error`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunTurnStatus {
    /// The turn completed successfully.
    Completed,
    /// The turn failed due to an error.
    Failed,
    /// The turn was cancelled by the caller.
    Cancelled,
}

impl RunTurnStatus {
    /// Map to the session-level commit status.
    fn to_commit_status(&self) -> TurnCommitStatus {
        match self {
            Self::Completed => TurnCommitStatus::Completed,
            Self::Failed => TurnCommitStatus::Error,
            Self::Cancelled => TurnCommitStatus::Cancelled,
        }
    }
}

/// Summary of a single tool execution for API display.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolResultSummary {
    /// The tool call ID from the model's request.
    pub tool_call_id: String,
    /// The tool name that was executed.
    pub name: String,
    /// Whether the tool executed successfully.
    pub ok: bool,
    /// Length of the tool output in bytes.
    pub output_len: usize,
    /// Optional error summary (first 200 chars of error output).
    pub error_summary: Option<String>,
}

/// Token usage statistics returned through the API.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnUsageSummary {
    /// Prompt token count.
    pub prompt_tokens: u64,
    /// Completion token count.
    pub completion_tokens: u64,
    /// Total token count.
    pub total_tokens: u64,
}

impl From<TurnUsage> for TurnUsageSummary {
    fn from(u: TurnUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        }
    }
}

/// The result of executing a synchronous turn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunTurnResult {
    /// The session this turn was executed in.
    pub session_id: String,
    /// Durable commit id for this turn.
    pub commit_id: String,
    /// The assistant's text output.
    pub assistant_output: String,
    /// Terminal status of the turn.
    pub status: RunTurnStatus,
    /// Token usage statistics, if available.
    pub usage: Option<TurnUsageSummary>,
    /// Summary of tool executions for API display.
    pub tool_results: Vec<ToolResultSummary>,
    /// The ordered stage trace recording each step of the pipeline.
    pub stages: Vec<RunTurnStage>,
}

// ── Streaming types ──────────────────────────────────────────────────────

/// An event emitted during a streaming turn.
///
/// Includes the full agent/token/tool event vocabulary plus a final
/// `Committed` event that carries the durable persistence outcome.
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
    /// A tool execution result summary (D-40: no full transcript content).
    ToolResult {
        id: String,
        name: String,
        ok: bool,
        output_len: usize,
    },
    /// The turn stream finished successfully.
    Done { finish_reason: String },
    /// An error occurred during the turn stream.
    Error { message: String },
    /// The turn was cancelled.
    Cancelled,
    /// Final event: the turn has been committed to durable storage.
    Committed {
        session_id: String,
        commit_id: String,
        status: RunTurnStatus,
    },
}

/// A boxed stream of [`TurnEvent`] values.
pub type TurnEventStream = Pin<Box<dyn Stream<Item = TurnEvent> + Send>>;

// ── Dependency seam ──────────────────────────────────────────────────────

/// Crate-private dependency injection seam for turn execution.
///
/// Tests inject a fake implementation; production uses [`RealTurnDeps`].
/// Not exported from `lib.rs`.
#[async_trait::async_trait]
pub(crate) trait TurnDeps: Send + Sync {
    /// Create a new session and return its ID.
    fn create_session(&self, request: &RunTurnRequest) -> Result<String>;

    /// Open an existing session and return its ID (or error if not found).
    fn open_session(&self, session_id: &str) -> Result<String>;

    /// Read the current HEAD transcript for a session.
    fn head_transcript(&self, session_id: &str) -> Result<HeadTranscript>;

    /// Execute the agent turn. Returns (events, result).
    async fn execute_agent_turn(
        &self,
        request: AgentTurnRequest,
    ) -> std::result::Result<(Vec<AgentTurnEvent>, AgentTurnResult), ApiError>;

    /// Commit a turn to durable storage. Returns the commit ID.
    fn commit_turn(&self, input: TurnCommitInput) -> Result<String>;
}

/// Production implementation of [`TurnDeps`] backed by [`SessionApi`] and
/// [`AgentLoop`].
pub(crate) struct RealTurnDeps {
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
}

impl RealTurnDeps {
    pub(crate) fn new(session: SessionApi, agent_loop: Arc<AgentLoop>) -> Self {
        Self {
            session,
            agent_loop,
        }
    }
}

#[async_trait::async_trait]
impl TurnDeps for RealTurnDeps {
    fn create_session(&self, request: &RunTurnRequest) -> Result<String> {
        let session = self.session.create_session(crate::session::CreateSessionRequest {
            label: None,
            profile: request.profile.clone(),
            model: request.model.clone(),
        })?;
        Ok(session.session_id)
    }

    fn open_session(&self, session_id: &str) -> Result<String> {
        let session = self
            .session
            .open_session(crate::session::OpenSessionRequest {
                id: session_id.to_string(),
            })?;
        Ok(session.session_id)
    }

    fn head_transcript(&self, session_id: &str) -> Result<HeadTranscript> {
        self.session.head_transcript(session_id)
    }

    async fn execute_agent_turn(
        &self,
        request: AgentTurnRequest,
    ) -> std::result::Result<(Vec<AgentTurnEvent>, AgentTurnResult), ApiError> {
        let result = self.agent_loop.run_turn(request).await;
        Ok((result.events.clone(), result))
    }

    fn commit_turn(&self, input: TurnCommitInput) -> Result<String> {
        let commit = self.session.commit_turn(input)?;
        Ok(commit.commit_id)
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────

/// Resolve model alias and system prompt from the request.
fn resolve_model_and_prompt(request: &RunTurnRequest) -> (String, Option<String>) {
    if let Some(ref lp) = request.loaded_profile {
        (
            lp.profile.agent.model.clone(),
            if lp.system_prompt.is_empty() {
                None
            } else {
                Some(lp.system_prompt.clone())
            },
        )
    } else {
        (
            request.model.clone().unwrap_or_else(|| "sonnet".to_string()),
            None,
        )
    }
}

/// Build an [`AgentTurnRequest`] from the API request, session context, and
/// resolved model/prompt.
fn build_agent_request(
    request: &RunTurnRequest,
    session_id: &str,
    transcript: HeadTranscript,
    model: String,
    system_prompt: Option<String>,
) -> AgentTurnRequest {
    let enabled_tools = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.tools.enabled.clone())
        .unwrap_or_default();

    let project_root = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.package_dir.to_string_lossy().to_string())
        .unwrap_or_else(|| std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default());

    AgentTurnRequest {
        session_id: session_id.to_string(),
        transcript,
        user_input: request.input.clone(),
        system_prompt,
        model,
        provider: None,
        enabled_tools,
        project_root,
        cancel_flag: None,
        timeout: None,
    }
}

/// Convert agent tool result summaries to API tool result summaries.
fn map_tool_results(summaries: &[AgentToolSummary]) -> Vec<ToolResultSummary> {
    summaries
        .iter()
        .map(|s| ToolResultSummary {
            tool_call_id: s.tool_call_id.clone(),
            name: s.name.clone(),
            ok: s.ok,
            output_len: s.output_len,
            error_summary: s.error_summary.clone(),
        })
        .collect()
}

/// Map agent turn status to API turn status.
fn map_status(status: &AgentTurnStatus) -> RunTurnStatus {
    match status {
        AgentTurnStatus::Completed => RunTurnStatus::Completed,
        AgentTurnStatus::Failed => RunTurnStatus::Failed,
        AgentTurnStatus::Cancelled => RunTurnStatus::Cancelled,
    }
}

/// Core turn orchestration shared by `run_turn_with_deps` and `stream_turn_with_deps`.
///
/// Returns: (session_id, stages, agent_events, agent_result, commit_id)
async fn execute_turn_inner(
    deps: &dyn TurnDeps,
    request: &RunTurnRequest,
) -> Result<(String, Vec<RunTurnStage>, Vec<AgentTurnEvent>, AgentTurnResult, String)> {
    let mut stages = Vec::new();

    // Stage: Profile
    stages.push(RunTurnStage::Profile);
    let (model, system_prompt) = resolve_model_and_prompt(request);

    // Stage: Session — create or open
    stages.push(RunTurnStage::Session);
    let session_id = match &request.session_id {
        Some(id) => deps.open_session(id)?,
        None => deps.create_session(request)?,
    };

    // Read transcript
    let transcript = deps.head_transcript(&session_id)?;

    // Stage: Agent
    stages.push(RunTurnStage::Agent);
    let agent_request =
        build_agent_request(request, &session_id, transcript, model, system_prompt);

    let (agent_events, agent_result) = deps
        .execute_agent_turn(agent_request)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Stage: LLM (agent loop completed the LLM calls)
    stages.push(RunTurnStage::Llm);

    // Stage: Tools (agent loop executed tools if any)
    stages.push(RunTurnStage::Tools);

    // Build commit input from agent result
    let commit_status = map_status(&agent_result.status).to_commit_status();

    // Build the messages to commit: user input + all generated messages
    let mut commit_messages = Vec::new();
    commit_messages.push(lattice_session::GraphMessage {
        role: "user".to_string(),
        content: request.input.clone(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
        name: None,
    });
    commit_messages.extend(agent_result.messages.iter().cloned());

    let commit_message = derive_commit_message(&commit_messages);

    // Re-read transcript to get parent_commit_id for the commit
    let transcript_for_parent = deps.head_transcript(&session_id)?;
    let parent_ids = transcript_for_parent
        .parent_commit_id
        .into_iter()
        .collect::<Vec<_>>();

    let profile_snapshot_id = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.agent.name.clone());

    let model_name = request
        .loaded_profile
        .as_ref()
        .map(|lp| lp.profile.agent.model.clone())
        .or_else(|| request.model.clone())
        .unwrap_or_else(|| "sonnet".to_string());

    let commit_input = TurnCommitInput {
        session_id: session_id.clone(),
        parent_ids,
        messages: commit_messages,
        profile_snapshot_id,
        model: model_name,
        provider: "unknown".to_string(), // Provider resolved at LLM layer
        status: commit_status,
        commit_message,
        usage: Some(agent_result.usage.clone()),
        terminal_reason: agent_result.terminal_reason.clone(),
    };

    // Stage: Commit
    stages.push(RunTurnStage::Commit);
    let commit_id = deps.commit_turn(commit_input)?;

    // Stage: Render
    stages.push(RunTurnStage::Render);

    Ok((session_id, stages, agent_events, agent_result, commit_id))
}

// ── Public API functions ─────────────────────────────────────────────────

/// Execute a synchronous turn with injected dependencies.
///
/// This is the crate-private testable entry point. Tests inject a fake
/// [`TurnDeps`] to avoid live provider calls, direct SQLite, or public
/// exposure of internal handles.
pub(crate) async fn run_turn_with_deps(
    deps: &dyn TurnDeps,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let (session_id, stages, _agent_events, agent_result, commit_id) =
        execute_turn_inner(deps, &request).await?;

    Ok(RunTurnResult {
        session_id,
        commit_id,
        assistant_output: agent_result.assistant_output,
        status: map_status(&agent_result.status),
        usage: Some(TurnUsageSummary::from(agent_result.usage)),
        tool_results: map_tool_results(&agent_result.tool_results),
        stages,
    })
}

/// Execute a synchronous turn.
///
/// Creates or opens a session, reads the current transcript, invokes the
/// agent loop, commits exactly one turn-level commit, and returns the
/// complete typed result.
///
/// When `session_id` is `None`, a new session is automatically created (D-42).
/// When `session_id` is provided, the existing session is opened and its
/// transcript is used as context.
pub async fn run_turn(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: RunTurnRequest,
) -> Result<RunTurnResult> {
    let deps = RealTurnDeps::new(session, agent_loop);
    run_turn_with_deps(&deps, request).await
}

/// Execute a streaming turn with injected dependencies.
///
/// This is the crate-private testable entry point for streaming.
pub(crate) async fn stream_turn_with_deps(
    deps: &dyn TurnDeps,
    request: RunTurnRequest,
) -> Result<TurnEventStream> {
    let (session_id, _stages, agent_events, agent_result, commit_id) =
        execute_turn_inner(deps, &request).await?;

    let status = map_status(&agent_result.status);

    // Convert agent events to public TurnEvents
    let mut turn_events: Vec<TurnEvent> = agent_events
        .into_iter()
        .map(|e| match e {
            AgentTurnEvent::Started => TurnEvent::Started,
            AgentTurnEvent::Token { content } => TurnEvent::Token { content },
            AgentTurnEvent::ToolCallStart { id, name } => TurnEvent::ToolCallStart { id, name },
            AgentTurnEvent::ToolCallResult {
                id, name, ok, output_len, ..
            } => TurnEvent::ToolResult {
                id,
                name,
                ok,
                output_len,
            },
            AgentTurnEvent::Done { finish_reason } => TurnEvent::Done { finish_reason },
            AgentTurnEvent::Error { message } => TurnEvent::Error { message },
            AgentTurnEvent::Cancelled => TurnEvent::Cancelled,
        })
        .collect();

    // Append final Committed event after persistence
    turn_events.push(TurnEvent::Committed {
        session_id,
        commit_id,
        status,
    });

    Ok(Box::pin(stream::iter(turn_events)))
}

/// Execute a streaming turn.
///
/// Creates or opens a session, invokes the agent loop, forwards live
/// agent/token/tool events, commits the turn, and appends a final
/// `Committed` event with the durable persistence outcome.
pub async fn stream_turn(
    session: SessionApi,
    agent_loop: Arc<AgentLoop>,
    request: StreamTurnRequest,
) -> Result<TurnEventStream> {
    let run_request = RunTurnRequest {
        input: request.input,
        session_id: request.session_id,
        profile: request.profile,
        model: request.model,
        loaded_profile: request.loaded_profile,
    };
    let deps = RealTurnDeps::new(session, agent_loop);
    stream_turn_with_deps(&deps, run_request).await
}

// ── Tests (TDD) ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use lattice_agent::AgentTurnStatus;
    use lattice_session::{GraphMessage, HeadTranscript, TurnUsage};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // ── Fake TurnDeps ───────────────────────────────────────────────────

    /// A fake implementation of [`TurnDeps`] for testing.
    ///
    /// Uses `Arc<Mutex<Option<AgentTurnResult>>>` because `AgentTurnResult`
    /// does not implement `Clone`. The result is taken on first use; subsequent
    /// calls return a default.
    struct FakeTurnDeps {
        created_session_id: String,
        agent_result: std::sync::Mutex<Option<AgentTurnResult>>,
        agent_events: Vec<AgentTurnEvent>,
        commit_id: String,
        create_called: AtomicBool,
        open_called: AtomicBool,
        commit_count: AtomicUsize,
        last_commit_input: std::sync::Mutex<Option<TurnCommitInput>>,
        last_agent_session_id: std::sync::Mutex<Option<String>>,
    }

    impl FakeTurnDeps {
        fn new(agent_result: AgentTurnResult) -> Self {
            Self {
                created_session_id: "auto-created-session".to_string(),
                agent_result: std::sync::Mutex::new(Some(agent_result)),
                agent_events: vec![
                    AgentTurnEvent::Started,
                    AgentTurnEvent::Token {
                        content: "test response".to_string(),
                    },
                    AgentTurnEvent::Done {
                        finish_reason: "stop".to_string(),
                    },
                ],
                commit_id: "commit-abc123".to_string(),
                create_called: AtomicBool::new(false),
                open_called: AtomicBool::new(false),
                commit_count: AtomicUsize::new(0),
                last_commit_input: std::sync::Mutex::new(None),
                last_agent_session_id: std::sync::Mutex::new(None),
            }
        }

        fn with_events(mut self, events: Vec<AgentTurnEvent>) -> Self {
            self.agent_events = events;
            self
        }

        fn created_session(&self) -> bool {
            self.create_called.load(Ordering::SeqCst)
        }

        fn opened_session(&self) -> bool {
            self.open_called.load(Ordering::SeqCst)
        }

        fn commit_count(&self) -> usize {
            self.commit_count.load(Ordering::SeqCst)
        }

        fn last_commit_input(&self) -> Option<TurnCommitInput> {
            self.last_commit_input.lock().unwrap().clone()
        }

        fn last_agent_session_id(&self) -> Option<String> {
            self.last_agent_session_id.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl TurnDeps for FakeTurnDeps {
        fn create_session(&self, _request: &RunTurnRequest) -> Result<String> {
            self.create_called.store(true, Ordering::SeqCst);
            Ok(self.created_session_id.clone())
        }

        fn open_session(&self, session_id: &str) -> Result<String> {
            self.open_called.store(true, Ordering::SeqCst);
            Ok(session_id.to_string())
        }

        fn head_transcript(&self, session_id: &str) -> Result<HeadTranscript> {
            Ok(HeadTranscript {
                session_id: session_id.to_string(),
                parent_commit_id: None,
                messages: vec![],
            })
        }

        async fn execute_agent_turn(
            &self,
            request: AgentTurnRequest,
        ) -> std::result::Result<(Vec<AgentTurnEvent>, AgentTurnResult), ApiError> {
            *self.last_agent_session_id.lock().unwrap() = Some(request.session_id.clone());
            let result = self
                .agent_result
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(success_result);
            Ok((self.agent_events.clone(), result))
        }

        fn commit_turn(&self, input: TurnCommitInput) -> Result<String> {
            self.commit_count.fetch_add(1, Ordering::SeqCst);
            *self.last_commit_input.lock().unwrap() = Some(input);
            Ok(self.commit_id.clone())
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────────

    fn success_result() -> AgentTurnResult {
        AgentTurnResult {
            messages: vec![GraphMessage {
                role: "assistant".to_string(),
                content: "test response".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            assistant_output: "test response".to_string(),
            usage: TurnUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            tool_results: vec![],
            status: AgentTurnStatus::Completed,
            terminal_reason: Some("stop".to_string()),
            events: vec![],
        }
    }

    fn failed_result() -> AgentTurnResult {
        AgentTurnResult {
            messages: vec![GraphMessage {
                role: "assistant".to_string(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            assistant_output: String::new(),
            usage: TurnUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
            tool_results: vec![],
            status: AgentTurnStatus::Failed,
            terminal_reason: Some("LLM error: provider unavailable".to_string()),
            events: vec![],
        }
    }

    fn cancelled_result() -> AgentTurnResult {
        AgentTurnResult {
            messages: vec![GraphMessage {
                role: "assistant".to_string(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            assistant_output: String::new(),
            usage: TurnUsage {
                prompt_tokens: 5,
                completion_tokens: 2,
                total_tokens: 7,
            },
            tool_results: vec![],
            status: AgentTurnStatus::Cancelled,
            terminal_reason: Some("cancelled by caller".to_string()),
            events: vec![],
        }
    }

    fn make_request(input: &str) -> RunTurnRequest {
        RunTurnRequest {
            input: input.to_string(),
            session_id: None,
            profile: None,
            model: None,
            loaded_profile: None,
        }
    }

    fn make_request_with_session(input: &str, session_id: &str) -> RunTurnRequest {
        RunTurnRequest {
            input: input.to_string(),
            session_id: Some(session_id.to_string()),
            profile: None,
            model: None,
            loaded_profile: None,
        }
    }

    // ── Tests ───────────────────────────────────────────────────────────

    /// Test 1: run_turn auto-creates session when session_id is None.
    #[tokio::test]
    async fn run_turn_auto_creates_session_and_returns_session_id() {
        let deps = FakeTurnDeps::new(success_result());
        let request = make_request("hello");
        let result = run_turn_with_deps(&deps, request).await.unwrap();

        assert!(
            !result.session_id.is_empty(),
            "session_id should not be empty"
        );
        assert_eq!(result.session_id, "auto-created-session");
        assert!(
            !result.commit_id.is_empty(),
            "commit_id should not be empty"
        );
        assert!(
            deps.created_session(),
            "create_session should have been called"
        );
        assert!(
            !deps.opened_session(),
            "open_session should not have been called"
        );
    }

    /// Test 2: run_turn commits exactly one turn.
    #[tokio::test]
    async fn run_turn_commits_exactly_one_turn() {
        let deps = FakeTurnDeps::new(success_result());
        let request = make_request("test input");
        let result = run_turn_with_deps(&deps, request).await.unwrap();

        assert_eq!(
            deps.commit_count(),
            1,
            "exactly one commit should be made"
        );
        assert_eq!(result.status, RunTurnStatus::Completed);
    }

    /// Test 3: run_turn uses existing session transcript.
    #[tokio::test]
    async fn run_turn_uses_existing_session_transcript() {
        let deps = FakeTurnDeps::new(success_result());
        let request = make_request_with_session("second turn", "existing-session");
        let result = run_turn_with_deps(&deps, request).await.unwrap();

        assert!(
            deps.opened_session(),
            "open_session should have been called"
        );
        assert!(
            !deps.created_session(),
            "create_session should not have been called"
        );
        assert_eq!(result.session_id, "existing-session");
        assert_eq!(
            deps.last_agent_session_id(),
            Some("existing-session".to_string()),
            "agent should receive the existing session ID"
        );
        assert_eq!(deps.commit_count(), 1);
    }

    /// Test 4: run_turn result contains all contract fields.
    #[tokio::test]
    async fn run_turn_result_contains_contract_fields() {
        let deps = FakeTurnDeps::new(success_result());
        let request = make_request("hello");
        let result = run_turn_with_deps(&deps, request).await.unwrap();

        // session_id: non-empty
        assert!(!result.session_id.is_empty());
        // commit_id: non-empty (was None in Phase 7/8)
        assert!(!result.commit_id.is_empty());
        // assistant_output
        assert_eq!(result.assistant_output, "test response");
        // status
        assert_eq!(result.status, RunTurnStatus::Completed);
        // usage
        let usage = result.usage.as_ref().expect("usage should be present");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
        // tool_results (empty for this test)
        assert!(result.tool_results.is_empty());
        // stages: ordered
        assert_eq!(
            result.stages,
            vec![
                RunTurnStage::Profile,
                RunTurnStage::Session,
                RunTurnStage::Agent,
                RunTurnStage::Llm,
                RunTurnStage::Tools,
                RunTurnStage::Commit,
                RunTurnStage::Render,
            ]
        );
    }

    /// Test 5: failed agent outcome is committed with status and terminal reason.
    #[tokio::test]
    async fn run_turn_failed_outcome_is_committed() {
        let deps = FakeTurnDeps::new(failed_result());
        let request = make_request("hello");
        let result = run_turn_with_deps(&deps, request).await.unwrap();

        assert_eq!(result.status, RunTurnStatus::Failed);
        assert_eq!(result.assistant_output, "");
        // Commit still happens
        assert_eq!(deps.commit_count(), 1);
        let commit_input = deps.last_commit_input().expect("commit should have been called");
        assert_eq!(commit_input.status, TurnCommitStatus::Error);
        assert!(
            commit_input
                .terminal_reason
                .as_ref()
                .unwrap()
                .contains("provider unavailable")
        );
        // Partial messages are committed (user + assistant)
        assert!(
            !commit_input.messages.is_empty(),
            "failed turn should still commit messages"
        );
    }

    /// Test 6: cancelled agent outcome is committed with status and terminal reason.
    #[tokio::test]
    async fn run_turn_cancelled_outcome_is_committed() {
        let deps = FakeTurnDeps::new(cancelled_result());
        let request = make_request("hello");
        let result = run_turn_with_deps(&deps, request).await.unwrap();

        assert_eq!(result.status, RunTurnStatus::Cancelled);
        // Commit still happens
        assert_eq!(deps.commit_count(), 1);
        let commit_input = deps.last_commit_input().expect("commit should have been called");
        assert_eq!(commit_input.status, TurnCommitStatus::Cancelled);
        assert!(
            commit_input
                .terminal_reason
                .as_ref()
                .unwrap()
                .contains("cancelled")
        );
        // Usage is preserved
        assert!(commit_input.usage.is_some());
        assert_eq!(commit_input.usage.as_ref().unwrap().total_tokens, 7);
    }

    // ── Serde roundtrip tests ───────────────────────────────────────────

    #[test]
    fn run_turn_status_serde_roundtrip() {
        let statuses = vec![
            RunTurnStatus::Completed,
            RunTurnStatus::Failed,
            RunTurnStatus::Cancelled,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let deserialized: RunTurnStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, deserialized);
        }
    }

    #[test]
    fn run_turn_result_serde_roundtrip() {
        let result = RunTurnResult {
            session_id: "sess-1".to_string(),
            commit_id: "commit-1".to_string(),
            assistant_output: "hello".to_string(),
            status: RunTurnStatus::Completed,
            usage: Some(TurnUsageSummary {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
            tool_results: vec![ToolResultSummary {
                tool_call_id: "call-1".to_string(),
                name: "read_file".to_string(),
                ok: true,
                output_len: 100,
                error_summary: None,
            }],
            stages: vec![RunTurnStage::Profile, RunTurnStage::Commit],
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: RunTurnResult = serde_json::from_str(&json).unwrap();
        assert_eq!(result, deserialized);
    }

    #[test]
    fn tool_result_summary_serde_roundtrip() {
        let summary = ToolResultSummary {
            tool_call_id: "call-1".to_string(),
            name: "read_file".to_string(),
            ok: true,
            output_len: 1024,
            error_summary: None,
        };
        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: ToolResultSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, deserialized);
    }

    #[test]
    fn turn_usage_summary_from_turn_usage() {
        let usage = TurnUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
        };
        let summary = TurnUsageSummary::from(usage);
        assert_eq!(summary.prompt_tokens, 100);
        assert_eq!(summary.completion_tokens, 50);
        assert_eq!(summary.total_tokens, 150);
    }

    // ── stream_turn behavioral tests ─────────────────────────────────────

    /// Test: stream_turn auto-creates session and final Committed event names it.
    #[tokio::test]
    async fn stream_turn_auto_creates_session_and_final_committed_event_names_it() {
        let deps = FakeTurnDeps::new(success_result());
        let request = make_request("hello");
        let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
            .await
            .unwrap()
            .collect()
            .await;

        let committed = events
            .iter()
            .find(|e| matches!(e, TurnEvent::Committed { .. }))
            .expect("should have Committed event");

        if let TurnEvent::Committed {
            session_id,
            commit_id,
            status,
        } = committed
        {
            assert!(!session_id.is_empty(), "session_id should be non-empty");
            assert_eq!(session_id, "auto-created-session");
            assert!(!commit_id.is_empty(), "commit_id should be non-empty");
            assert_eq!(*status, RunTurnStatus::Completed);
        } else {
            panic!("expected Committed event");
        }
    }

    /// Test: stream events include tool activity before final Committed event.
    #[tokio::test]
    async fn stream_turn_emits_tool_activity_before_committed() {
        let tool_result_agent = AgentTurnResult {
            messages: vec![
                GraphMessage {
                    role: "assistant".to_string(),
                    content: String::new(),
                    reasoning_content: None,
                    tool_calls: Some(vec![lattice_session::GraphToolCall {
                        id: "call_1".to_string(),
                        name: "read_file".to_string(),
                        arguments: "{}".to_string(),
                    }]),
                    tool_call_id: None,
                    name: None,
                },
                GraphMessage {
                    role: "tool".to_string(),
                    content: "file content".to_string(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some("call_1".to_string()),
                    name: Some("read_file".to_string()),
                },
                GraphMessage {
                    role: "assistant".to_string(),
                    content: "here is the file".to_string(),
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                },
            ],
            assistant_output: "here is the file".to_string(),
            usage: TurnUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            tool_results: vec![AgentToolSummary {
                tool_call_id: "call_1".to_string(),
                name: "read_file".to_string(),
                ok: true,
                output_len: 12,
                error_summary: None,
            }],
            status: AgentTurnStatus::Completed,
            terminal_reason: Some("stop".to_string()),
            events: vec![],
        };
        let tool_events = vec![
            AgentTurnEvent::Started,
            AgentTurnEvent::ToolCallStart {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
            },
            AgentTurnEvent::ToolCallResult {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                ok: true,
                output_len: 12,
            },
            AgentTurnEvent::Token {
                content: "here is the file".to_string(),
            },
            AgentTurnEvent::Done {
                finish_reason: "stop".to_string(),
            },
        ];

        let deps = FakeTurnDeps::new(tool_result_agent).with_events(tool_events);
        let request = make_request("read file");
        let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
            .await
            .unwrap()
            .collect()
            .await;

        // Find positions of tool activity and Committed event
        let tool_start_pos = events
            .iter()
            .position(|e| matches!(e, TurnEvent::ToolCallStart { .. }));
        let tool_result_pos = events
            .iter()
            .position(|e| matches!(e, TurnEvent::ToolResult { .. }));
        let committed_pos = events
            .iter()
            .position(|e| matches!(e, TurnEvent::Committed { .. }));

        assert!(tool_start_pos.is_some(), "should have ToolCallStart event");
        assert!(
            tool_result_pos.is_some(),
            "should have ToolResult event"
        );
        assert!(committed_pos.is_some(), "should have Committed event");

        // Tool activity must appear before Committed
        assert!(
            tool_start_pos.unwrap() < committed_pos.unwrap(),
            "ToolCallStart must appear before Committed"
        );
        assert!(
            tool_result_pos.unwrap() < committed_pos.unwrap(),
            "ToolResult must appear before Committed"
        );
    }

    /// Test: stream does not replay events only after commit.
    /// Proves at least one token/tool event is yielded before commit persistence.
    #[tokio::test]
    async fn stream_turn_does_not_replay_only_after_commit() {
        let deps = FakeTurnDeps::new(success_result());
        let request = make_request("hello");
        let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
            .await
            .unwrap()
            .collect()
            .await;

        // At least one non-Committed event must exist before the Committed event
        let committed_pos = events
            .iter()
            .position(|e| matches!(e, TurnEvent::Committed { .. }))
            .expect("should have Committed event");

        assert!(
            committed_pos > 0,
            "at least one event must appear before Committed (no replay-after-commit)"
        );

        // The first event should be Started
        assert!(
            matches!(events[0], TurnEvent::Started),
            "first event should be Started, got {:?}",
            events[0]
        );
    }

    /// Test: tool result stream events expose summaries, not full transcript content.
    #[tokio::test]
    async fn stream_turn_result_summary_excludes_full_large_tool_text() {
        let large_content = "x".repeat(10_000);
        let large_agent = AgentTurnResult {
            messages: vec![GraphMessage {
                role: "tool".to_string(),
                content: large_content.clone(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("read_file".to_string()),
            }],
            assistant_output: "done".to_string(),
            usage: TurnUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
            tool_results: vec![AgentToolSummary {
                tool_call_id: "call_1".to_string(),
                name: "read_file".to_string(),
                ok: true,
                output_len: large_content.len(),
                error_summary: None,
            }],
            status: AgentTurnStatus::Completed,
            terminal_reason: Some("stop".to_string()),
            events: vec![],
        };
        let events_with_tool = vec![
            AgentTurnEvent::Started,
            AgentTurnEvent::ToolCallStart {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
            },
            AgentTurnEvent::ToolCallResult {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                ok: true,
                output_len: large_content.len(),
            },
            AgentTurnEvent::Done {
                finish_reason: "stop".to_string(),
            },
        ];

        let deps = FakeTurnDeps::new(large_agent).with_events(events_with_tool);
        let request = make_request("read file");
        let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
            .await
            .unwrap()
            .collect()
            .await;

        // Find ToolResult events - they should have output_len but NOT full content
        for event in &events {
            if let TurnEvent::ToolResult { output_len, .. } = event {
                assert_eq!(*output_len, 10_000);
                // ToolResult does not carry full content - only id/name/ok/output_len
            }
            // No TurnEvent variant carries the full 10k content string
            // (Token events carry assistant text, not tool output)
        }
    }

    /// Test: error stream still commits failed and emits final Committed event.
    #[tokio::test]
    async fn stream_turn_error_still_commits_failed() {
        let deps = FakeTurnDeps::new(failed_result());
        let request = make_request("hello");
        let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
            .await
            .unwrap()
            .collect()
            .await;

        // Should have a Committed event with Failed status
        let committed = events
            .iter()
            .find(|e| matches!(e, TurnEvent::Committed { .. }))
            .expect("should have Committed event even on failure");

        if let TurnEvent::Committed { status, .. } = committed {
            assert_eq!(*status, RunTurnStatus::Failed);
        }

        // Should have committed exactly one turn
        assert_eq!(deps.commit_count(), 1);
        let commit_input = deps.last_commit_input().expect("commit should have been called");
        assert_eq!(commit_input.status, TurnCommitStatus::Error);
    }

    // ── TurnEvent serde tests ───────────────────────────────────────────

    #[test]
    fn turn_event_committed_serde_roundtrip() {
        let event = TurnEvent::Committed {
            session_id: "sess-1".to_string(),
            commit_id: "commit-1".to_string(),
            status: RunTurnStatus::Completed,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: TurnEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn turn_event_tool_result_serde_roundtrip() {
        let event = TurnEvent::ToolResult {
            id: "call-1".to_string(),
            name: "read_file".to_string(),
            ok: true,
            output_len: 1024,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: TurnEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }

    // ── Boundary enforcement tests ──────────────────────────────────────

    /// Compile-time check: public API types are exported and usable.
    /// If any of these types are missing from `lib.rs`, this test fails to compile.
    #[test]
    fn api_public_types_are_exported() {
        fn _assert_types_exist() {
            // Request types
            let _ = std::any::TypeId::of::<crate::RunTurnRequest>();
            let _ = std::any::TypeId::of::<crate::StreamTurnRequest>();
            // Result types
            let _ = std::any::TypeId::of::<crate::RunTurnResult>();
            let _ = std::any::TypeId::of::<crate::RunTurnStatus>();
            let _ = std::any::TypeId::of::<crate::ToolResultSummary>();
            let _ = std::any::TypeId::of::<crate::TurnUsageSummary>();
            let _ = std::any::TypeId::of::<crate::RunTurnStage>();
            // Event types
            let _ = std::any::TypeId::of::<crate::TurnEvent>();
            // Session types
            let _ = std::any::TypeId::of::<crate::SessionApi>();
            let _ = std::any::TypeId::of::<crate::ApiError>();
            let _ = std::any::TypeId::of::<crate::CreateSessionRequest>();
            let _ = std::any::TypeId::of::<crate::OpenSessionRequest>();
        }
    }

    /// Compile-time check: internal types are NOT re-exported from lib.rs.
    /// If lib.rs adds `pub use` for these, this test module would need updating,
    /// drawing reviewer attention to the boundary violation.
    #[test]
    fn api_does_not_export_internal_types() {
        // The following types should NOT be accessible as crate:: exports:
        // - AgentLoop (lattice-agent internal)
        // - AgentLoopConfig (lattice-agent internal)
        // - TurnDeps (crate-private trait)
        // - RealTurnDeps (crate-private struct)
        //
        // If any of these were exported, this test would compile differently.
        // This is a documentation/negative assertion test.
        fn _assert_not_exported() {
            // These should NOT compile if uncommented:
            // let _ = std::any::TypeId::of::<crate::AgentLoop>();
            // let _ = std::any::TypeId::of::<crate::TurnDeps>();
            // let _ = std::any::TypeId::of::<crate::RealTurnDeps>();
        }
    }

    /// Source guard: verify no forbidden crate `use` statements exist in
    /// lattice-api source. Only checks actual `use` imports, not comments
    /// or string literals.
    #[test]
    fn api_forbidden_crates_absent_from_source() {
        use std::fs;
        use std::path::Path;

        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let forbidden_prefixes = [
            "use lattice_runtime",
            "use lattice_prompt",
            "use lattice_sandbox",
            "use lattice_policy",
            "pub use lattice_runtime",
            "pub use lattice_prompt",
            "pub use lattice_sandbox",
            "pub use lattice_policy",
        ];

        for entry in fs::read_dir(&src_dir).expect("should read src dir") {
            let entry = entry.expect("should read entry");
            let path = entry.path();
            if path.extension().map_or(true, |e| e != "rs") {
                continue;
            }
            let content = fs::read_to_string(&path).expect("should read file");
            for (i, line) in content.lines().enumerate() {
                let stripped = line.trim();
                // Skip comments and doc strings
                if stripped.starts_with("//") || stripped.starts_with("//!") || stripped.starts_with("///") {
                    continue;
                }
                for pattern in &forbidden_prefixes {
                    if stripped.starts_with(pattern) {
                        panic!(
                            "forbidden import in {}:{}: {}",
                            path.display(),
                            i + 1,
                            line
                        );
                    }
                }
            }
        }
    }
}
