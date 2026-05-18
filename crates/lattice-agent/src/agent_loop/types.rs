use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use lattice_session::{GraphMessage, HeadTranscript, TurnUsage};

/// Input for a single agent turn.
#[derive(Debug, Clone)]
pub struct AgentTurnRequest {
    /// Session identifier.
    pub session_id: String,
    /// The complete transcript from the current branch HEAD.
    pub transcript: HeadTranscript,
    /// The user's current input for this turn.
    pub user_input: String,
    /// Optional system prompt to prepend.
    pub system_prompt: Option<String>,
    /// Model alias to use for LLM calls.
    pub model: String,
    /// Optional provider override.
    pub provider: Option<String>,
    /// Explicit tool names to enable for this turn.
    pub enabled_tools: Vec<String>,
    /// Project root for tool execution path containment.
    pub project_root: String,
    /// Optional cancellation flag for external cancel support (D-09).
    /// When set to `true`, the loop terminates with `Cancelled` status.
    pub cancel_flag: Option<Arc<AtomicBool>>,
    /// Optional timeout for the entire turn (D-09/D-10).
    /// If the turn does not complete within this duration, it returns
    /// `Failed` status with a timeout terminal reason.
    pub timeout: Option<Duration>,
}

/// Terminal status of an agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTurnStatus {
    /// The turn completed successfully with a final assistant response.
    Completed,
    /// The turn failed due to an error (LLM error, unknown tools, etc.).
    Failed,
    /// The turn was cancelled by the caller.
    Cancelled,
}

/// Summary of a single tool execution for API display.
#[derive(Debug, Clone)]
pub struct AgentToolResultSummary {
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

/// Result of a completed agent turn.
#[derive(Debug)]
pub struct AgentTurnResult {
    /// All messages generated during this turn (assistant + tool messages).
    pub messages: Vec<GraphMessage>,
    /// The final assistant output text.
    pub assistant_output: String,
    /// Accumulated token usage across all LLM calls in this turn.
    pub usage: TurnUsage,
    /// Summary of tool executions for API display.
    pub tool_results: Vec<AgentToolResultSummary>,
    /// Terminal status of the turn.
    pub status: AgentTurnStatus,
    /// Terminal reason string (e.g. "stop", "timeout", "error: ...").
    pub terminal_reason: Option<String>,
    /// Events emitted during turn execution.
    pub events: Vec<AgentTurnEvent>,
}

/// Events emitted during agent turn execution.
///
/// These events are the internal vocabulary for the agent loop. The API facade
/// maps them to public `TurnEvent` values for streaming consumers.
#[derive(Debug, Clone)]
pub enum AgentTurnEvent {
    /// The turn has started.
    Started,
    /// A chunk of generated text content from the LLM.
    Token { content: String },
    /// A tool call is about to be executed.
    ToolCallStart { id: String, name: String },
    /// A tool call has completed.
    ToolCallResult {
        id: String,
        name: String,
        ok: bool,
        output_len: usize,
    },
    /// The turn completed successfully.
    Done { finish_reason: String },
    /// The turn encountered an error.
    Error { message: String },
    /// The turn was cancelled.
    Cancelled,
}
