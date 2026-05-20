use serde::{Deserialize, Serialize};
use std::pin::Pin;

use futures::stream::Stream;
use lattice_profile::LoadedAgentProfile;

/// Request to run a turn through either [`run_turn`] or [`stream_turn`].
///
/// Both facades accept the same input shape, so they share this single
/// request type. `stream_turn` does not currently expose intermediate
/// progress live — it executes the turn to completion and then replays
/// buffered events — so callers do not need a distinct request type.
///
/// [`run_turn`]: crate::turn::run_turn
/// [`stream_turn`]: crate::turn::stream_turn
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RunTurnRequest {
    pub input: String,
    pub session_id: Option<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
    #[serde(skip)]
    pub loaded_profile: Option<LoadedAgentProfile>,
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
    Failed,
    Cancelled,
}

impl RunTurnStatus {
    pub fn to_commit_status(&self) -> lattice_session::TurnCommitStatus {
        match self {
            Self::Completed => lattice_session::TurnCommitStatus::Completed,
            Self::Failed => lattice_session::TurnCommitStatus::Error,
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

/// Turn stream event.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum TurnEvent {
    Started,
    Token {
        content: String,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolResult {
        id: String,
        name: String,
        ok: bool,
        output_len: usize,
    },
    Done {
        finish_reason: String,
    },
    Error {
        message: String,
    },
    Cancelled,
    Committed {
        session_id: String,
        commit_id: String,
        status: RunTurnStatus,
    },
}

/// Boxed stream of TurnEvent values.
pub type TurnEventStream = Pin<Box<dyn Stream<Item = TurnEvent> + Send>>;
