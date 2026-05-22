//! agent loop: sequences LLM calls and tool execution for a single turn.
//!
//! This module implements the stable public main-path `AgentLoop`. It uses
//! `lattice-llm` types at the model boundary and `lattice-tools::enabled_tools`
//! for tool authorization.

mod messages;
mod traits;
mod types;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::StreamExt;
use lattice_session::{GraphMessage, GraphToolCall, TurnUsage};
use tokio::time::Instant;
use lattice_tools::enabled_tools::{resolve_enabled_tools, EnabledTools};

use messages::convert_to_llm_message;

/// Hard upper bound on LLM rounds within a single turn.
///
/// Acts as a safety net when caller-supplied timeout/cancel are missing or
/// when an LLM repeatedly emits tool calls without a final response. Set
/// well above expected agent task length (well-behaved agents finish in
/// well under 50 rounds; we cap at 200 to prevent runaway loops without
/// constraining legitimate long tasks).
const MAX_TURN_ROUNDS: u32 = 200;

pub use traits::{LlmClient, ToolCallExecutor};
pub use types::{
    AgentToolResultSummary, AgentTurnEvent, AgentTurnRequest, AgentTurnResult, AgentTurnStatus,
};

/// Configuration for an [`AgentLoop`] instance.
///
/// Holds the injectable LLM client and tool executor. The loop itself is
/// stateless between turns -- all per-turn state lives in
/// [`AgentTurnRequest`].
pub struct AgentLoopConfig {
    /// The LLM client used for chat completions.
    pub llm: Arc<dyn LlmClient>,
    /// The tool executor used for running tool calls.
    pub tool_executor: Arc<dyn ToolCallExecutor>,
}

impl fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("llm", &"<dyn LlmClient>")
            .field("tool_executor", &"<dyn ToolCallExecutor>")
            .finish()
    }
}

/// The v1.1 agent loop.
///
/// Owns the configuration needed to execute a single turn: an LLM client and
/// a tool executor. Call [`AgentLoop::run_turn`] to execute a turn.
pub struct AgentLoop {
    config: AgentLoopConfig,
}

impl AgentLoop {
    /// Create a new agent loop with the given configuration.
    pub fn new(config: AgentLoopConfig) -> Self {
        Self { config }
    }

    /// Execute a single turn: call the LLM, execute requested tools
    /// sequentially, and repeat until a final assistant response or terminal
    /// outcome.
    pub async fn run_turn(&self, request: AgentTurnRequest) -> AgentTurnResult {
        // Resolve enabled tools (T-09-02: fail before LLM call)
        let enabled = match resolve_enabled_tools(&request.enabled_tools) {
            Ok(tools) => tools,
            Err(err) => {
                return AgentTurnResult {
                    messages: Vec::new(),
                    assistant_output: String::new(),
                    usage: TurnUsage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    },
                    tool_results: Vec::new(),
                    status: AgentTurnStatus::Error,
                    terminal_reason: Some(format!("unknown enabled tools: {err}")),
                    events: Vec::new(),
                };
            }
        };

        // Compute cooperative deadline from request.timeout (D-13, Phase 17)
        let deadline = request.timeout.map(|d| Instant::now() + d);
        self.run_turn_inner(&request, &enabled, None, deadline).await
    }

    /// Execute a single turn with a live `AgentTurnEvent` mirror channel.
    ///
    /// Behavior is identical to [`AgentLoop::run_turn`] — same tool execution,
    /// same cancel checkpoints, same timeout shape — with one addition: every
    /// emission site routes through `emit_agent_event` (Plan 15-02 D-12 / D-13)
    /// so events also flow into the supplied `events` sender as they happen.
    /// The buffered `AgentTurnResult.events` returned at the end stays
    /// complete: the live channel is a best-effort mirror, not a replacement.
    ///
    /// Drop semantics — if the receiver is dropped at any point during the
    /// turn, subsequent live `send`s return `SendError` which `emit_agent_event`
    /// silently swallows; the turn continues normally and returns a fully
    /// populated `AgentTurnResult` (Plan 15-02 D-12).
    ///
    /// Cancellation — `request.cancel_flag` (set by the caller) is the sole
    /// cancel input (Plan 15-02 D-11 / D-17). All five new cancel checkpoints
    /// (1b, 2, 3, 4, 5) honor it identically across `run_turn` and
    /// `run_turn_live`; the live mirror does not change cancel semantics.
    ///
    /// Timeout — when `request.timeout` is `Some`, a cooperative deadline is
    /// computed before entering `run_turn_inner` and threaded into both the
    /// `stream_chat().await` call and the `stream.next()` loop via
    /// `tokio::select!`. On elapse, the returned `AgentTurnResult` preserves
    /// all partial state accumulated before the deadline fired (Phase 17 D-13).
    ///
    /// Added in Plan 15-02 (D-09 / D-10). Timeout cooperative deadline added
    /// in Phase 17 Plan 01.
    pub async fn run_turn_live(
        &self,
        request: AgentTurnRequest,
        events: tokio::sync::mpsc::Sender<AgentTurnEvent>,
    ) -> AgentTurnResult {
        // Resolve enabled tools (T-09-02: fail before LLM call) — same shape
        // as `run_turn`, no live emission yet because the turn has not started.
        let enabled = match resolve_enabled_tools(&request.enabled_tools) {
            Ok(tools) => tools,
            Err(err) => {
                return AgentTurnResult {
                    messages: Vec::new(),
                    assistant_output: String::new(),
                    usage: TurnUsage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    },
                    tool_results: Vec::new(),
                    status: AgentTurnStatus::Error,
                    terminal_reason: Some(format!("unknown enabled tools: {err}")),
                    events: Vec::new(),
                };
            }
        };

        // Compute cooperative deadline from request.timeout (D-13, Phase 17)
        let deadline = request.timeout.map(|d| Instant::now() + d);
        self.run_turn_inner(&request, &enabled, Some(&events), deadline)
            .await
    }

    /// Inner turn execution (called by `run_turn` and `run_turn_live`).
    ///
    /// `live_events` (Plan 15-02 D-10): when `Some`, every event emission site
    /// in this function and inside `consume_model_stream` mirrors the event
    /// onto the live channel via `emit_agent_event`; when `None`, emissions
    /// are buffered only — preserving the pre-Phase-15 `run_turn` shape.
    ///
    /// `deadline` (Phase 17 D-13): when `Some`, a cooperative deadline is
    /// applied to both `stream_chat().await` and `stream.next()` calls. On
    /// elapse, the function returns with all accumulated partial state preserved.
    async fn run_turn_inner(
        &self,
        request: &AgentTurnRequest,
        enabled: &EnabledTools,
        live_events: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>,
        deadline: Option<Instant>,
    ) -> AgentTurnResult {
        // Build initial messages from transcript + user input
        let mut messages = self.build_messages(request, enabled);

        let mut all_messages: Vec<GraphMessage> = Vec::new();
        let mut all_events: Vec<AgentTurnEvent> = Vec::new();
        let mut accumulated_usage = TurnUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        let mut tool_summaries: Vec<AgentToolResultSummary> = Vec::new();

        // Repeat-until-final loop with safety cap (MAX_TURN_ROUNDS).
        // External timeout/cancel remain primary stops; this guards against
        // missing-timeout cases where a model emits tool calls indefinitely.
        let mut round: u32 = 0;
        loop {
            if round >= MAX_TURN_ROUNDS {
                let reason = format!(
                    "exceeded max rounds ({}); set a shorter timeout or fix prompt",
                    MAX_TURN_ROUNDS
                );
                return AgentTurnResult {
                    messages: all_messages,
                    assistant_output: String::new(),
                    usage: accumulated_usage,
                    tool_results: tool_summaries,
                    status: AgentTurnStatus::Error,
                    terminal_reason: Some(reason),
                    events: all_events,
                };
            }
            round += 1;

            // Check cancel (D-09)
            if let Some(flag) = &request.cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: String::new(),
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Cancelled,
                        terminal_reason: Some("cancelled by caller".to_string()),
                        events: all_events,
                    };
                }
            }

            // Call the LLM
            let llm_messages: Vec<lattice_llm::Message> = match messages
                .iter()
                .map(convert_to_llm_message)
                .collect::<Result<Vec<_>, String>>()
            {
                Ok(msgs) => msgs,
                Err(e) => {
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: String::new(),
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Error,
                        terminal_reason: Some(e),
                        events: all_events,
                    };
                }
            };

            let chat_request = lattice_llm::ChatRequest {
                messages: llm_messages,
                tools: enabled.definitions.clone(),
                target: lattice_llm::ModelTarget {
                    model: request.model.clone(),
                    provider: request.provider.clone(),
                },
                temperature: None,
                max_tokens: None,
                max_completion_tokens: None,
            };

            emit_agent_event(
                &mut all_events,
                live_events,
                AgentTurnEvent::ModelStarted {
                    provider: request.provider.clone(),
                    model: request.model.clone(),
                },
            )
            .await;

            // Cancel checkpoint #1b (Plan 15-02 D-06): the awaited
            // `emit_agent_event` for ModelStarted can yield; check the cancel
            // flag here so a flip between checkpoint #1 (round top) and the
            // actual `stream_chat` open is observed before we open the stream.
            // No partial to preserve — `stream_chat` was never called.
            if let Some(flag) = &request.cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: String::new(),
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Cancelled,
                        terminal_reason: Some("cancelled by caller".to_string()),
                        events: all_events,
                    };
                }
            }

            let stream = match deadline {
                Some(dl) => {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    tokio::select! {
                        result = self.config.llm.stream_chat(chat_request) => {
                            match result {
                                Ok(s) => s,
                                Err(err) => {
                                    return AgentTurnResult {
                                        messages: all_messages,
                                        assistant_output: String::new(),
                                        usage: accumulated_usage,
                                        tool_results: tool_summaries,
                                        status: AgentTurnStatus::Error,
                                        terminal_reason: Some(format!("LLM error: {err}")),
                                        events: all_events,
                                    };
                                }
                            }
                        }
                        _ = tokio::time::sleep(remaining) => {
                            // Timeout before stream creation — preserve accumulated state
                            return AgentTurnResult {
                                messages: all_messages,
                                assistant_output: String::new(),
                                usage: accumulated_usage,
                                tool_results: tool_summaries,
                                status: AgentTurnStatus::Error,
                                terminal_reason: Some("timeout".to_string()),
                                events: all_events,
                            };
                        }
                    }
                }
                None => {
                    match self.config.llm.stream_chat(chat_request).await {
                        Ok(s) => s,
                        Err(err) => {
                            return AgentTurnResult {
                                messages: all_messages,
                                assistant_output: String::new(),
                                usage: accumulated_usage,
                                tool_results: tool_summaries,
                                status: AgentTurnStatus::Error,
                                terminal_reason: Some(format!("LLM error: {err}")),
                                events: all_events,
                            };
                        }
                    }
                }
            };

            let stream_result = self
                .consume_model_stream(
                    stream,
                    &mut all_events,
                    live_events,
                    request.cancel_flag.as_ref(),
                    deadline,
                )
                .await;

            // Accumulate usage from the stream
            if let Some(ref usage) = stream_result.usage {
                accumulated_usage.prompt_tokens += usage.prompt_tokens;
                accumulated_usage.completion_tokens += usage.completion_tokens;
                accumulated_usage.total_tokens += usage.total_tokens;
            }

            // Handle stream outcome
            match stream_result.outcome {
                StreamOutcome::Error(msg) => {
                    // Preserve partial content on error (D-11, D-13)
                    flush_partial_assistant_message(
                        &mut messages,
                        &mut all_messages,
                        &stream_result.assistant_content,
                        &stream_result.reasoning_content,
                    );
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: stream_result.assistant_content,
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Error,
                        terminal_reason: Some(format!("LLM error: {msg}")),
                        events: all_events,
                    };
                }
                StreamOutcome::Exhausted => {
                    // Stream ended without Done (D-12)
                    flush_partial_assistant_message(
                        &mut messages,
                        &mut all_messages,
                        &stream_result.assistant_content,
                        &stream_result.reasoning_content,
                    );
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: stream_result.assistant_content,
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Error,
                        terminal_reason: Some("LLM stream ended without completion".to_string()),
                        events: all_events,
                    };
                }
                StreamOutcome::Cancelled => {
                    // Cancel observed inside `consume_model_stream` (post-event,
                    // pre-next-iteration). Plan 15-02 D-04: flush any partial
                    // assistant + reasoning content via the shared helper from
                    // Plan 15-01 D-05 (no `ModelFinished` synthesized — D-08).
                    flush_partial_assistant_message(
                        &mut messages,
                        &mut all_messages,
                        &stream_result.assistant_content,
                        &stream_result.reasoning_content,
                    );
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: stream_result.assistant_content,
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Cancelled,
                        terminal_reason: Some("cancelled by caller".to_string()),
                        events: all_events,
                    };
                }
                StreamOutcome::Timeout => {
                    // Cooperative deadline fired during stream.next() (Phase 17
                    // D-13). Flush partial content and return with preserved state.
                    flush_partial_assistant_message(
                        &mut messages,
                        &mut all_messages,
                        &stream_result.assistant_content,
                        &stream_result.reasoning_content,
                    );
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: stream_result.assistant_content,
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Error,
                        terminal_reason: Some("timeout".to_string()),
                        events: all_events,
                    };
                }
                StreamOutcome::Done => {
                    let finish_reason = stream_result
                        .finish_reason
                        .unwrap_or_else(|| "stop".to_string());

                    if stream_result.tool_calls.is_empty() {
                        // No tool calls: final response
                        emit_agent_event(
                            &mut all_events,
                            live_events,
                            AgentTurnEvent::ModelFinished {
                                finish_reason: finish_reason.clone(),
                                usage: stream_result.usage.clone(),
                            },
                        )
                        .await;

                        let assistant_msg = GraphMessage {
                            role: "assistant".to_string(),
                            content: stream_result.assistant_content.clone(),
                            reasoning_content: if stream_result.reasoning_content.is_empty() {
                                None
                            } else {
                                Some(stream_result.reasoning_content.clone())
                            },
                            tool_calls: None,
                            tool_call_id: None,
                            name: None,
                        };
                        messages.push(assistant_msg.clone());
                        all_messages.push(assistant_msg);

                        return AgentTurnResult {
                            messages: all_messages,
                            assistant_output: stream_result.assistant_content,
                            usage: accumulated_usage,
                            tool_results: tool_summaries,
                            status: AgentTurnStatus::Completed,
                            terminal_reason: Some(finish_reason),
                            events: all_events,
                        };
                    }

                    // Tool calls present: validate all-or-nothing (D-09)
                    let mut all_valid = true;
                    let mut invalid_info: Option<String> = None;
                    let mut validated_calls: Vec<PendingToolCall> = Vec::new();

                    for call in &stream_result.tool_calls {
                        // Normalize empty arguments to "{}"
                        let normalized_args = if call.arguments.is_empty() {
                            "{}".to_string()
                        } else {
                            call.arguments.clone()
                        };
                        // Validate via serde_json
                        if serde_json::from_str::<serde_json::Value>(&normalized_args).is_err() {
                            all_valid = false;
                            invalid_info = Some(format!(
                                "invalid/incomplete tool call: {} (id={})",
                                call.name, call.id
                            ));
                            break;
                        }
                        validated_calls.push(PendingToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: normalized_args,
                            ended: call.ended,
                        });
                    }

                    if !all_valid {
                        // Invalid tool calls: no ModelFinished, content-only message
                        if !stream_result.assistant_content.is_empty()
                            || !stream_result.reasoning_content.is_empty()
                        {
                            let assistant_msg = GraphMessage {
                                role: "assistant".to_string(),
                                content: stream_result.assistant_content.clone(),
                                reasoning_content: if stream_result.reasoning_content.is_empty() {
                                    None
                                } else {
                                    Some(stream_result.reasoning_content.clone())
                                },
                                tool_calls: None,
                                tool_call_id: None,
                                name: None,
                            };
                            messages.push(assistant_msg.clone());
                            all_messages.push(assistant_msg);
                        }

                        return AgentTurnResult {
                            messages: all_messages,
                            assistant_output: stream_result.assistant_content,
                            usage: accumulated_usage,
                            tool_results: tool_summaries,
                            status: AgentTurnStatus::Error,
                            terminal_reason: Some(invalid_info.unwrap_or_default()),
                            events: all_events,
                        };
                    }

                    // All valid: emit ModelFinished, build assistant message with tool calls
                    emit_agent_event(
                        &mut all_events,
                        live_events,
                        AgentTurnEvent::ModelFinished {
                            finish_reason: finish_reason.clone(),
                            usage: stream_result.usage.clone(),
                        },
                    )
                    .await;

                    let graph_tool_calls: Vec<GraphToolCall> = validated_calls
                        .iter()
                        .map(|call| GraphToolCall {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                        })
                        .collect();

                    let assistant_msg = GraphMessage {
                        role: "assistant".to_string(),
                        content: stream_result.assistant_content.clone(),
                        reasoning_content: if stream_result.reasoning_content.is_empty() {
                            None
                        } else {
                            Some(stream_result.reasoning_content.clone())
                        },
                        tool_calls: Some(graph_tool_calls),
                        tool_call_id: None,
                        name: None,
                    };
                    messages.push(assistant_msg.clone());
                    all_messages.push(assistant_msg);

                    // Cancel checkpoint #3 (Plan 15-02 D-06 / D-08): the
                    // assistant tool-call message is committed above; observe
                    // cancel here so the transcript keeps the assistant
                    // message but no tool result messages. Done's "model
                    // response was finalized" semantics is preserved before
                    // this branch.
                    if let Some(flag) = &request.cancel_flag {
                        if flag.load(Ordering::Relaxed) {
                            return AgentTurnResult {
                                messages: all_messages,
                                assistant_output: stream_result.assistant_content,
                                usage: accumulated_usage,
                                tool_results: tool_summaries,
                                status: AgentTurnStatus::Cancelled,
                                terminal_reason: Some("cancelled by caller".to_string()),
                                events: all_events,
                            };
                        }
                    }

                    // Execute validated tool calls sequentially (D-10, AGENT-07)
                    for call in &validated_calls {
                        // Cancel checkpoint #4 (Plan 15-02 D-06): observe cancel
                        // before emitting `ToolCallStarted` for the next tool.
                        // Skips the rest of the batch on cancel; no
                        // `ToolCallStarted` for the skipped tool, prior tools'
                        // events / `tool_summaries` / messages are preserved
                        // (D-07).
                        if let Some(flag) = &request.cancel_flag {
                            if flag.load(Ordering::Relaxed) {
                                return AgentTurnResult {
                                    messages: all_messages,
                                    assistant_output: stream_result.assistant_content,
                                    usage: accumulated_usage,
                                    tool_results: tool_summaries,
                                    status: AgentTurnStatus::Cancelled,
                                    terminal_reason: Some("cancelled by caller".to_string()),
                                    events: all_events,
                                };
                            }
                        }

                        emit_agent_event(
                            &mut all_events,
                            live_events,
                            AgentTurnEvent::ToolCallStarted {
                                id: call.id.clone(),
                                name: call.name.clone(),
                            },
                        )
                        .await;

                        let executor_call = lattice_llm::ToolCall {
                            id: call.id.clone(),
                            function: lattice_llm::FunctionCall {
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                            },
                        };

                        let tool_result = self.config.tool_executor.execute(&executor_call).await;
                        let (ok, tool_content) = match tool_result {
                            Ok(output) => (true, output),
                            Err(error) => (false, error),
                        };

                        let error_summary = if ok {
                            None
                        } else {
                            Some(tool_content.chars().take(200).collect::<String>())
                        };

                        emit_agent_event(
                            &mut all_events,
                            live_events,
                            AgentTurnEvent::ToolCallFinished {
                                id: call.id.clone(),
                                name: call.name.clone(),
                                ok,
                                output_len: tool_content.len(),
                                error_summary: error_summary.clone(),
                            },
                        )
                        .await;

                        tool_summaries.push(AgentToolResultSummary {
                            tool_call_id: call.id.clone(),
                            name: call.name.clone(),
                            ok,
                            output_len: tool_content.len(),
                            error_summary,
                        });

                        // Full tool result as GraphMessage (D-23..D-26: no summarization)
                        let tool_msg = GraphMessage {
                            role: "tool".to_string(),
                            content: tool_content,
                            reasoning_content: None,
                            tool_calls: None,
                            tool_call_id: Some(call.id.clone()),
                            name: Some(call.name.clone()),
                        };
                        messages.push(tool_msg.clone());
                        all_messages.push(tool_msg);

                        // Cancel checkpoint #5 (Plan 15-02 D-06 / D-07): the
                        // just-finished tool's `ToolCallStarted` /
                        // `ToolCallFinished` events, summary, and tool message
                        // are all committed above. On cancel, exit before
                        // the next iteration starts another tool — already
                        // in-flight tools are never aborted (D-07).
                        if let Some(flag) = &request.cancel_flag {
                            if flag.load(Ordering::Relaxed) {
                                return AgentTurnResult {
                                    messages: all_messages,
                                    assistant_output: stream_result.assistant_content.clone(),
                                    usage: accumulated_usage,
                                    tool_results: tool_summaries,
                                    status: AgentTurnStatus::Cancelled,
                                    terminal_reason: Some("cancelled by caller".to_string()),
                                    events: all_events,
                                };
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A tool call being assembled from stream events.
///
/// `ended` records whether `ToolCallEnd` was received for this call. It is
/// read during diagnostics (terminal_reason can report "closed by: tool_call_end
/// vs done") but is NOT required for execution eligibility — Done + valid JSON
/// is sufficient (some providers skip ToolCallEnd).
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
    ended: bool,
}

/// Outcome of consuming a model stream.
enum StreamOutcome {
    /// Stream completed with a `Done` event.
    Done,
    /// Stream encountered an `Error` event.
    Error(String),
    /// Caller requested cancellation while the stream was still producing
    /// events. Observed via the post-event cancel-flag check inside
    /// `consume_model_stream` (Plan 15-02 D-01..D-04). Surrounding
    /// `StreamResult` preserves accumulated `assistant_content` /
    /// `reasoning_content` / `tool_calls` so the upstream `Cancelled` arm in
    /// `run_turn_inner` can flush partial content with the same shape used by
    /// `Error` and `Exhausted`. The cancel reason is fixed
    /// (`"cancelled by caller"`) at the upstream call site, not on this
    /// variant.
    Cancelled,
    /// Stream ended without receiving a `Done` event.
    Exhausted,
    /// Cooperative deadline fired during `stream.next()` (Phase 17 D-13).
    /// Accumulated partial state is preserved in the surrounding `StreamResult`.
    Timeout,
}

/// Result of consuming a model stream.
struct StreamResult {
    assistant_content: String,
    reasoning_content: String,
    tool_calls: Vec<PendingToolCall>,
    usage: Option<TurnUsage>,
    finish_reason: Option<String>,
    outcome: StreamOutcome,
}

impl AgentLoop {
    /// Consume an LLM stream, accumulating deltas and assembling tool calls.
    ///
    /// Returns a `StreamResult` with accumulated content, reasoning, assembled
    /// tool calls, usage, and the terminal outcome (Done, Error, Cancelled,
    /// or Exhausted).
    ///
    /// Plan 15-02 widens this with two optional side-channels:
    ///
    /// - `live` — when `Some`, every Token/Reasoning emission is mirrored onto
    ///   the live `mpsc::Sender` via `emit_agent_event`. Buffered `events`
    ///   remain the canonical sink.
    /// - `cancel_flag` — when `Some`, the function checks the flag *after*
    ///   each non-`Done` `LlmEvent` match arm body completes (after the per-event
    ///   emit, after pending-call mutations) and *before* the next
    ///   `stream.next().await`. On observed cancel, returns immediately with
    ///   `outcome: StreamOutcome::Cancelled` and accumulated state preserved
    ///   (D-02). `Done` is intentionally never a cancel point — the upstream
    ///   loop finalizes the model response before any cancel branch (D-03).
    ///
    /// Phase 17 adds:
    /// - `deadline` — when `Some`, each `stream.next()` is raced against the
    ///   remaining time via `tokio::select!`. On timeout, returns with
    ///   `outcome: StreamOutcome::Error("timeout")` and all accumulated state
    ///   preserved (D-13).
    async fn consume_model_stream(
        &self,
        mut stream: lattice_llm::LlmStream,
        events: &mut Vec<AgentTurnEvent>,
        live: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>,
        cancel_flag: Option<&Arc<AtomicBool>>,
        deadline: Option<Instant>,
    ) -> StreamResult {
        let mut assistant_content = String::new();
        let mut reasoning_content = String::new();
        let mut pending_calls: Vec<PendingToolCall> = Vec::new();
        let mut usage: Option<TurnUsage> = None;
        let mut finish_reason: Option<String> = None;

        loop {
            // Fetch next event, racing against deadline when configured (D-13).
            let next_event = match deadline {
                Some(dl) => {
                    let remaining = dl.saturating_duration_since(Instant::now());
                    tokio::select! {
                        event = stream.next() => event,
                        _ = tokio::time::sleep(remaining) => {
                            // Deadline fired — return with accumulated partial state
                            return StreamResult {
                                assistant_content,
                                reasoning_content,
                                tool_calls: pending_calls,
                                usage,
                                finish_reason,
                                outcome: StreamOutcome::Timeout,
                            };
                        }
                    }
                }
                None => stream.next().await,
            };

            let event = match next_event {
                Some(ev) => ev,
                None => {
                    // Stream exhausted without Done (D-12)
                    return StreamResult {
                        assistant_content,
                        reasoning_content,
                        tool_calls: pending_calls,
                        usage,
                        finish_reason,
                        outcome: StreamOutcome::Exhausted,
                    };
                }
            };

            match event {
                lattice_llm::LlmEvent::Token { content } => {
                    assistant_content.push_str(&content);
                    emit_agent_event(events, live, AgentTurnEvent::TokenDelta { content }).await;
                }
                lattice_llm::LlmEvent::Reasoning { content } => {
                    reasoning_content.push_str(&content);
                    emit_agent_event(events, live, AgentTurnEvent::ReasoningDelta { content })
                        .await;
                }
                lattice_llm::LlmEvent::ToolCallStart { id, name } => {
                    if pending_calls.iter().any(|c| c.id == id) {
                        return StreamResult {
                            assistant_content,
                            reasoning_content,
                            tool_calls: pending_calls,
                            usage,
                            finish_reason,
                            outcome: StreamOutcome::Error(format!(
                                "invalid/incomplete tool call: duplicate id={id}"
                            )),
                        };
                    }
                    pending_calls.push(PendingToolCall {
                        id,
                        name,
                        arguments: String::new(),
                        ended: false,
                    });
                }
                lattice_llm::LlmEvent::ToolCallDelta {
                    id,
                    arguments_delta,
                } => {
                    if let Some(index) = pending_calls.iter().position(|c| c.id == id) {
                        if pending_calls[index].ended {
                            return StreamResult {
                                assistant_content,
                                reasoning_content,
                                tool_calls: pending_calls,
                                usage,
                                finish_reason,
                                outcome: StreamOutcome::Error(format!(
                                    "invalid/incomplete tool call: delta after end (id={id})"
                                )),
                            };
                        }
                        pending_calls[index].arguments.push_str(&arguments_delta);
                    } else {
                        return StreamResult {
                            assistant_content,
                            reasoning_content,
                            tool_calls: pending_calls,
                            usage,
                            finish_reason,
                            outcome: StreamOutcome::Error(format!(
                                "invalid/incomplete tool call: delta without start (id={id})"
                            )),
                        };
                    }
                }
                lattice_llm::LlmEvent::ToolCallEnd { id } => {
                    if let Some(index) = pending_calls.iter().position(|c| c.id == id) {
                        if pending_calls[index].ended {
                            return StreamResult {
                                assistant_content,
                                reasoning_content,
                                tool_calls: pending_calls,
                                usage,
                                finish_reason,
                                outcome: StreamOutcome::Error(format!(
                                    "invalid/incomplete tool call: duplicate end (id={id})"
                                )),
                            };
                        }
                        pending_calls[index].ended = true;
                    } else {
                        return StreamResult {
                            assistant_content,
                            reasoning_content,
                            tool_calls: pending_calls,
                            usage,
                            finish_reason,
                            outcome: StreamOutcome::Error(format!(
                                "invalid/incomplete tool call: end without start (id={id})"
                            )),
                        };
                    }
                }
                lattice_llm::LlmEvent::Done {
                    finish_reason: fr,
                    usage: u,
                } => {
                    finish_reason = Some(fr);
                    usage = u.map(|tu| TurnUsage {
                        prompt_tokens: tu.prompt_tokens as u64,
                        completion_tokens: tu.completion_tokens as u64,
                        total_tokens: tu.total_tokens as u64,
                    });
                    return StreamResult {
                        assistant_content,
                        reasoning_content,
                        tool_calls: pending_calls,
                        usage,
                        finish_reason,
                        outcome: StreamOutcome::Done,
                    };
                }
                lattice_llm::LlmEvent::Error { message } => {
                    return StreamResult {
                        assistant_content,
                        reasoning_content,
                        tool_calls: pending_calls,
                        usage,
                        finish_reason,
                        outcome: StreamOutcome::Error(message),
                    };
                }
                // No-op events (D-04, D-05)
                lattice_llm::LlmEvent::Started
                | lattice_llm::LlmEvent::Citation { .. }
                | lattice_llm::LlmEvent::Signature { .. }
                | lattice_llm::LlmEvent::GroundingChunk { .. }
                | lattice_llm::LlmEvent::SafetyRating { .. } => {}
            }

            // Cancel checkpoint #2 (Plan 15-02 D-02 / D-06): observe cancel
            // *after* the per-event emit + pending-call mutations and *before*
            // the next `stream.next().await`. Done is excluded above (D-03);
            // malformed-toolcall Error early-returns are also above (they keep
            // their pre-existing inline StreamResult shape).
            if let Some(flag) = cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    return StreamResult {
                        assistant_content,
                        reasoning_content,
                        tool_calls: pending_calls,
                        usage,
                        finish_reason,
                        outcome: StreamOutcome::Cancelled,
                    };
                }
            }
        }
    }

    /// Build the initial message list from the transcript, system prompt,
    /// and current user input (D-11).
    fn build_messages(
        &self,
        request: &AgentTurnRequest,
        _enabled: &EnabledTools,
    ) -> Vec<GraphMessage> {
        let mut messages = Vec::new();

        // System prompt first (if present)
        if let Some(ref system_prompt) = request.system_prompt {
            messages.push(GraphMessage {
                role: "system".to_string(),
                content: system_prompt.clone(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }

        // Transcript messages (complete history from root to HEAD)
        messages.extend(request.transcript.messages.iter().cloned());

        // Current user input
        messages.push(GraphMessage {
            role: "user".to_string(),
            content: request.user_input.clone(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });

        messages
    }
}

/// Append an `AgentTurnEvent` to the buffered per-turn event vector and, when a
/// live `mpsc::Sender` is provided, mirror the same event onto the live
/// channel.
///
/// The buffered store is the canonical sink — every event always lands in
/// `buffered` so `AgentTurnResult.events` stays complete regardless of whether
/// a live receiver is attached or has already been dropped.
///
/// The live mirror is best-effort: receiver drop or any other `SendError` is
/// silently swallowed via `let _ = sender.send(event).await;` (Plan 15-02
/// D-12). The awaited `send` blocks on backpressure when the bounded channel
/// is full — alpha policy, no `try_send`, no buffering.
///
/// Added in Plan 15-02 (D-12 / D-13) so every emission site in
/// `run_turn_inner` and `consume_model_stream` routes through one path.
async fn emit_agent_event(
    buffered: &mut Vec<AgentTurnEvent>,
    live: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>,
    event: AgentTurnEvent,
) {
    buffered.push(event.clone());
    if let Some(sender) = live {
        let _ = sender.send(event).await;
    }
}

/// Append a partial assistant message to both the per-turn `messages` buffer and
/// the `all_messages` accumulator when an LLM stream terminates before a clean
/// `Done` (Error/Exhausted, and in Plan 15-02 also Cancelled).
///
/// Returns early without mutating either buffer when both `assistant_content`
/// and `reasoning_content` are empty. Otherwise constructs a single
/// `GraphMessage { role: "assistant", ... }` with `reasoning_content` wrapped
/// in `Some(...)` only when non-empty, then pushes a clone to `messages` and
/// the original to `all_messages`.
///
/// Extracted in Plan 15-01 (D-05) to deduplicate the Error/Exhausted partial
/// flush blocks before the new Cancelled arm becomes a third user.
fn flush_partial_assistant_message(
    messages: &mut Vec<GraphMessage>,
    all_messages: &mut Vec<GraphMessage>,
    assistant_content: &str,
    reasoning_content: &str,
) {
    if assistant_content.is_empty() && reasoning_content.is_empty() {
        return;
    }
    let msg = GraphMessage {
        role: "assistant".to_string(),
        content: assistant_content.to_string(),
        reasoning_content: if reasoning_content.is_empty() {
            None
        } else {
            Some(reasoning_content.to_string())
        },
        tool_calls: None,
        tool_call_id: None,
        name: None,
    };
    messages.push(msg.clone());
    all_messages.push(msg);
}
