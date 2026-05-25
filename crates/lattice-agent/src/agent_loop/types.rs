use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use lattice_llm::ToolDefinition;
use lattice_session::{GraphMessage, HeadTranscript, TurnUsage};

use super::traits::CepErrorPayload;

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
    /// Pre-resolved tool definitions for this turn (assembled by the API layer).
    pub tool_definitions: Vec<ToolDefinition>,
    /// Optional cancellation flag for external cancel support (D-09).
    /// When set to `true`, the loop terminates with `Cancelled` status.
    pub cancel_flag: Option<Arc<AtomicBool>>,
    /// Optional cooperative deadline for LLM and tool execution.
    ///
    /// The agent checks this deadline around stream creation, stream
    /// consumption, and tool execution. On expiry, it returns `Error` status
    /// with a timeout terminal reason while preserving accumulated messages
    /// and events. Live-sink backpressure uses non-blocking `try_send` (M-19a)
    /// and is not covered by this deadline.
    pub timeout: Option<Duration>,
}

/// Terminal status of an agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentTurnStatus {
    /// The turn completed successfully with a final assistant response.
    Completed,
    /// The turn ended in error (LLM failure, max-rounds, unknown tools, timeout, etc.).
    Error,
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
    /// Structured CEP error metadata when the executor adapter mapped a CEP failure.
    pub cep_error: Option<CepErrorPayload>,
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
/// Process-only events describing what happens inside the agent loop. The agent
/// loop does **not** emit terminal events. Terminal state for a turn lives
/// solely on [`AgentTurnResult::status`] and [`AgentTurnResult::terminal_reason`];
/// the API facade derives public terminal events from the result.
///
/// `AgentTurnEvent` is internal to the runtime and intentionally not
/// `Serialize`/`Deserialize`. Only [`crate::TurnEvent`]-style public types
/// carry serde derives.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentTurnEvent {
    /// A model request is about to start.
    ///
    /// `provider` is the routed provider name when known (useful for
    /// debugging model routing and OpenAI-compatible dispatch); `model`
    /// is the canonical model identifier passed to the LLM client.
    ModelStarted {
        provider: Option<String>,
        model: String,
    },
    /// A chunk of reasoning/thinking text from the LLM.
    ReasoningDelta { content: String },
    /// A chunk of visible assistant text from the LLM.
    TokenDelta { content: String },
    /// A tool call is about to be executed.
    ToolCallStarted { id: String, name: String },
    /// A tool call has completed execution.
    ToolCallFinished {
        id: String,
        name: String,
        ok: bool,
        output_len: usize,
        error_summary: Option<String>,
    },
    /// A model request completed successfully.
    ///
    /// Emitted on successful LLM completion (`finish_reason: "stop"` /
    /// `"tool_calls"` / `"length"`). Error paths and `consume_model_stream`
    /// returning `Cancelled` mid-stream do **not** emit `ModelFinished`;
    /// their outcome is observable on `AgentTurnResult.status`.
    ///
    /// Note: cancellation observed *after* the model's `Done` chunk —
    /// specifically Plan 15-02 checkpoint #3 (post-`ModelFinished`,
    /// pre-tool-execution) and checkpoint #4 (pre-each-tool) — runs after
    /// this event has already been buffered/forwarded. A `Cancelled` turn
    /// can therefore include a trailing `ModelFinished` in
    /// `AgentTurnResult.events`. Phase 16 event mapping must not assume
    /// "no `ModelFinished` ⇒ Completed".
    ModelFinished {
        finish_reason: String,
        usage: Option<TurnUsage>,
    },
    /// A CEP call has started on the embedded server (distinct from model-facing tool calls).
    CepCallStarted {
        call_id: u64,
        method: String,
        impact_kind: String,
        audit_id: i64,
        semantic_stage: String,
        guard_stage: String,
    },
    /// A CEP call has finished on the embedded server.
    CepCallFinished {
        call_id: u64,
        method: String,
        impact_kind: String,
        audit_id: i64,
        ok: bool,
        semantic_stage: String,
        guard_stage: String,
        error_code: Option<String>,
    },
    /// Staged bridge lifecycle guard metadata (Phase 20 only; not Guard L1 enforcement).
    CepGuardVerdict {
        call_id: u64,
        method: String,
        semantic_stage: String,
        guard_stage: String,
        policy_enforced: bool,
        semantic_status: Option<String>,
    },
}
