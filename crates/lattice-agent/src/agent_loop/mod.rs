//! agent loop: sequences LLM calls and tool execution for a single turn.
//!
//! This module implements the stable public main-path `AgentLoop`. It uses
//! `lattice-llm` types at the model boundary and `lattice-tools::enabled_tools`
//! for tool authorization.

mod messages;
mod traits;
mod types;

use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures::StreamExt;
use lattice_session::{GraphMessage, GraphToolCall, TurnUsage};
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

        // Apply timeout if configured (D-09/D-10)
        if let Some(duration) = request.timeout {
            match tokio::time::timeout(duration, self.run_turn_inner(&request, &enabled)).await {
                Ok(result) => result,
                Err(_elapsed) => AgentTurnResult {
                    messages: Vec::new(),
                    assistant_output: String::new(),
                    usage: TurnUsage {
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        total_tokens: 0,
                    },
                    tool_results: Vec::new(),
                    status: AgentTurnStatus::Error,
                    terminal_reason: Some("timeout".to_string()),
                    events: Vec::new(),
                },
            }
        } else {
            self.run_turn_inner(&request, &enabled).await
        }
    }

    /// Inner turn execution (called by `run_turn`, possibly wrapped in timeout).
    async fn run_turn_inner(
        &self,
        request: &AgentTurnRequest,
        enabled: &EnabledTools,
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

            all_events.push(AgentTurnEvent::ModelStarted {
                provider: request.provider.clone(),
                model: request.model.clone(),
            });

            let stream = match self.config.llm.stream_chat(chat_request).await {
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
            };

            let stream_result = self.consume_model_stream(stream, &mut all_events).await;

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
                    if !stream_result.assistant_content.is_empty()
                        || !stream_result.reasoning_content.is_empty()
                    {
                        let partial_msg = GraphMessage {
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
                        messages.push(partial_msg.clone());
                        all_messages.push(partial_msg);
                    }
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
                    if !stream_result.assistant_content.is_empty()
                        || !stream_result.reasoning_content.is_empty()
                    {
                        let partial_msg = GraphMessage {
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
                        messages.push(partial_msg.clone());
                        all_messages.push(partial_msg);
                    }
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
                StreamOutcome::Done => {
                    let finish_reason = stream_result
                        .finish_reason
                        .unwrap_or_else(|| "stop".to_string());

                    if stream_result.tool_calls.is_empty() {
                        // No tool calls: final response
                        all_events.push(AgentTurnEvent::ModelFinished {
                            finish_reason: finish_reason.clone(),
                            usage: stream_result.usage.clone(),
                        });

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
                            status: AgentTurnStatus::Error,
                            terminal_reason: Some(invalid_info.unwrap_or_default()),
                            events: all_events,
                        };
                    }

                    // All valid: emit ModelFinished, build assistant message with tool calls
                    all_events.push(AgentTurnEvent::ModelFinished {
                        finish_reason: finish_reason.clone(),
                        usage: stream_result.usage.clone(),
                    });

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

                    // Execute validated tool calls sequentially (D-10, AGENT-07)
                    for call in &validated_calls {
                        all_events.push(AgentTurnEvent::ToolCallStarted {
                            id: call.id.clone(),
                            name: call.name.clone(),
                        });

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

                        all_events.push(AgentTurnEvent::ToolCallFinished {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            ok,
                            output_len: tool_content.len(),
                            error_summary: error_summary.clone(),
                        });

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
    /// Stream ended without receiving a `Done` event.
    Exhausted,
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
    /// tool calls, usage, and the terminal outcome (Done, Error, or Exhausted).
    async fn consume_model_stream(
        &self,
        mut stream: lattice_llm::LlmStream,
        events: &mut Vec<AgentTurnEvent>,
    ) -> StreamResult {
        let mut assistant_content = String::new();
        let mut reasoning_content = String::new();
        let mut pending_calls: Vec<PendingToolCall> = Vec::new();
        let mut usage: Option<TurnUsage> = None;
        let mut finish_reason: Option<String> = None;

        while let Some(event) = stream.next().await {
            match event {
                lattice_llm::LlmEvent::Token { content } => {
                    assistant_content.push_str(&content);
                    events.push(AgentTurnEvent::TokenDelta { content });
                }
                lattice_llm::LlmEvent::Reasoning { content } => {
                    reasoning_content.push_str(&content);
                    events.push(AgentTurnEvent::ReasoningDelta { content });
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
                    if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                        call.arguments.push_str(&arguments_delta);
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
                    if let Some(call) = pending_calls.iter_mut().find(|c| c.id == id) {
                        call.ended = true;
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
        }

        // Stream exhausted without Done (D-12)
        StreamResult {
            assistant_content,
            reasoning_content,
            tool_calls: pending_calls,
            usage,
            finish_reason,
            outcome: StreamOutcome::Exhausted,
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
