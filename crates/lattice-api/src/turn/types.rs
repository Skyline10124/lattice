use serde::{Deserialize, Serialize};
use std::pin::Pin;

use futures::stream::Stream;
use lattice_profile::LoadedAgentProfile;

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
    pub fn to_commit_status(&self) -> lattice_session::TurnCommitStatus {
        match self {
            Self::Completed => lattice_session::TurnCommitStatus::Completed,
            Self::Failed => lattice_session::TurnCommitStatus::Error,
            Self::Cancelled => lattice_session::TurnCommitStatus::Cancelled,
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

impl From<lattice_session::TurnUsage> for TurnUsageSummary {
    fn from(u: lattice_session::TurnUsage) -> Self {
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
