use serde::{Deserialize, Serialize};
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use futures::stream::Stream;
use lattice_profile::LoadedAgentProfile;

/// Request to run a turn through either [`run_turn`] or [`stream_turn`].
///
/// Both facades accept the same input shape, so they share this single
/// request type. `run_turn` returns after the turn is committed, while
/// `stream_turn` returns a live [`TurnEvent`] stream after startup validation
/// so callers can observe model deltas, tool activity, terminal status, and
/// commit lifecycle events as they occur.
///
/// [`run_turn`]: crate::turn::run_turn
/// [`stream_turn`]: crate::turn::stream_turn
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunTurnRequest {
    pub input: String,
    pub session_id: Option<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
    #[serde(skip)]
    pub loaded_profile: Option<LoadedAgentProfile>,
    /// Cooperative cancellation flag forwarded to the agent loop.
    #[serde(skip)]
    pub cancel_flag: Option<Arc<AtomicBool>>,
}

impl PartialEq for RunTurnRequest {
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input
            && self.session_id == other.session_id
            && self.profile == other.profile
            && self.model == other.model
            && self.loaded_profile == other.loaded_profile
    }
}

/// A stage in the turn execution pipeline.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunTurnStage {
    Profile,
    Session,
    Agent,
    Llm,
    Tools,
    Commit,
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
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunTurnStatus {
    Completed,
    Error,
    Cancelled,
}

impl RunTurnStatus {
    pub fn to_commit_status(&self) -> lattice_session::TurnCommitStatus {
        match self {
            Self::Completed => lattice_session::TurnCommitStatus::Completed,
            Self::Error => lattice_session::TurnCommitStatus::Error,
            Self::Cancelled => lattice_session::TurnCommitStatus::Cancelled,
        }
    }
}

/// Summary of a single tool execution.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolResultSummary {
    pub tool_call_id: String,
    pub name: String,
    pub ok: bool,
    pub output_len: usize,
    pub error_summary: Option<String>,
}

/// Token usage statistics.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnUsageSummary {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
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

/// Result of executing a synchronous turn.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunTurnResult {
    pub session_id: String,
    pub commit_id: String,
    pub assistant_output: String,
    pub status: RunTurnStatus,
    pub usage: Option<TurnUsageSummary>,
    pub tool_results: Vec<ToolResultSummary>,
    pub stages: Vec<RunTurnStage>,
}

/// Public turn stream event with stable JSON discriminator.
///
/// Serializes with `#[serde(tag = "type", rename_all = "snake_case")]`, so
/// each variant produces JSON of the form `{"type": "snake_case_name", ...}`.
/// Unit variants serialize as `{"type": "snake_case_name"}`.
///
/// The vocabulary covers turn lifecycle (`TurnStarted`, terminal events,
/// commit events), per-request model activity (`ModelStarted`,
/// `ModelFinished`), incremental deltas (`TokenDelta`, `ReasoningDelta`),
/// and tool execution (`ToolCallStarted`, `ToolCallFinished`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    /// The turn has started execution.
    TurnStarted {
        session_id: String,
        parent_commit_id: Option<String>,
        branch: Option<String>,
    },
    /// A model request is starting.
    ModelStarted {
        provider: Option<String>,
        model: String,
    },
    /// A chunk of reasoning/thinking text from the LLM.
    ReasoningDelta { content: String },
    /// A chunk of visible assistant text from the LLM.
    TokenDelta { content: String },
    /// A tool call is about to execute.
    ToolCallStarted { id: String, name: String },
    /// A tool call has completed.
    ToolCallFinished {
        id: String,
        name: String,
        ok: bool,
        output_len: usize,
        error_summary: Option<String>,
    },
    /// A model request completed successfully.
    ModelFinished {
        finish_reason: String,
        usage: Option<TurnUsageSummary>,
    },
    /// The turn failed due to an error.
    TurnFailed { reason: String },
    /// The turn was cancelled.
    TurnCancelled { reason: Option<String> },
    /// Commit persistence is in progress.
    TurnCommitting,
    /// The turn was committed successfully.
    TurnCommitted {
        session_id: String,
        commit_id: String,
        status: RunTurnStatus,
    },
    /// Commit persistence failed.
    CommitFailed { reason: String },
}

/// Boxed stream of TurnEvent values.
pub type TurnEventStream = Pin<Box<dyn Stream<Item = TurnEvent> + Send>>;
