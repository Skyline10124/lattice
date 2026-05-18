//! v1.1 agent loop: sequences LLM calls and tool execution for a single turn.
//!
//! This module implements the stable public main-path `AgentLoop` that is
//! It uses `lattice-llm` types at the model boundary and
//! `lattice-tools::enabled_tools` for tool authorization.
//!
//! # Design decisions
//!
//! - D-01: Ownership belongs in `lattice-agent`.
//! - D-02: Independent v1.1 minimal path, not a reuse of old `Agent`.
//! - D-03/D-04: Final public names: `AgentLoop`, `AgentLoopConfig`,
//!   `AgentTurnRequest`, `AgentTurnResult`, `AgentTurnEvent`, `AgentTurnStatus`.
//! - D-05/D-06/D-07: Old `Agent` architecture is deleted from active source.
//! - D-08: No fixed tool counter; loop until final assistant response.
//! - D-09/D-10: External timeout/cancel as run protection.
//! - D-11: Build model input from complete session transcript + user input.
//! - D-12: No transcript compression, summaries, or prompt-budget logic.
//! - D-13..D-18: Only explicit enabled tool names; no bundle expansion.
//! - D-19..D-22: Project-root path containment via executor.
//! - D-23..D-26: Full tool results in transcript; no summarization/clipping.
//! - T-09-02: `resolve_enabled_tools` called before LLM call.
//! - T-09-03: Result carries status, terminal reason, messages, usage, summaries.
//! - T-09-04: External timeout/cancel; no heuristic round limits.

mod messages;
mod tools;
mod traits;
mod types;

use std::fmt;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use lattice_session::{GraphMessage, GraphToolCall, TurnUsage};
use lattice_tools::enabled_tools::{resolve_enabled_tools, EnabledTools};

use messages::convert_to_llm_message;
use tools::tool_output_is_ok;

pub use traits::{LlmClient, ToolCallExecutor};
pub use types::{
    AgentToolResultSummary, AgentTurnEvent, AgentTurnRequest, AgentTurnResult, AgentTurnStatus,
};

// ---------------------------------------------------------------------------
// AgentLoopConfig
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// AgentLoop
// ---------------------------------------------------------------------------

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
                    status: AgentTurnStatus::Failed,
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
                    status: AgentTurnStatus::Failed,
                    terminal_reason: Some("timeout".to_string()),
                    events: vec![AgentTurnEvent::Started, AgentTurnEvent::Cancelled],
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

        all_events.push(AgentTurnEvent::Started);

        // Repeat-until-final loop (D-08: no fixed round counter)
        loop {
            // Check cancel (D-09)
            if let Some(flag) = &request.cancel_flag {
                if flag.load(Ordering::Relaxed) {
                    all_events.push(AgentTurnEvent::Cancelled);
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
                    all_events.push(AgentTurnEvent::Error { message: e.clone() });
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: String::new(),
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Failed,
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
                stream: false,
            };

            let response = match self.config.llm.chat(chat_request).await {
                Ok(resp) => resp,
                Err(err) => {
                    all_events.push(AgentTurnEvent::Error {
                        message: err.to_string(),
                    });
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: String::new(),
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Failed,
                        terminal_reason: Some(format!("LLM error: {err}")),
                        events: all_events,
                    };
                }
            };

            // Accumulate usage
            if let Some(ref usage) = response.usage {
                accumulated_usage.prompt_tokens += usage.prompt_tokens as u64;
                accumulated_usage.completion_tokens += usage.completion_tokens as u64;
                accumulated_usage.total_tokens += usage.total_tokens as u64;
            }

            // Emit token/assistant event
            if !response.content.is_empty() {
                all_events.push(AgentTurnEvent::Token {
                    content: response.content.clone(),
                });
            }

            // Build assistant GraphMessage
            let assistant_tool_calls: Option<Vec<GraphToolCall>> =
                response.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|tc| GraphToolCall {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            arguments: tc.function.arguments.clone(),
                        })
                        .collect()
                });

            let assistant_msg = GraphMessage {
                role: "assistant".to_string(),
                content: response.content.clone(),
                reasoning_content: response.reasoning_content.clone(),
                tool_calls: assistant_tool_calls.clone(),
                tool_call_id: None,
                name: None,
            };
            messages.push(assistant_msg.clone());
            all_messages.push(assistant_msg);

            // If no tool calls, this is the final response
            let tool_calls = match response.tool_calls {
                Some(calls) if !calls.is_empty() => calls,
                _ => {
                    all_events.push(AgentTurnEvent::Done {
                        finish_reason: response.finish_reason.clone(),
                    });
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: response.content,
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Completed,
                        terminal_reason: Some(response.finish_reason),
                        events: all_events,
                    };
                }
            };

            // Execute tool calls sequentially (D-19..D-22)
            for call in &tool_calls {
                all_events.push(AgentTurnEvent::ToolCallStart {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                });

                let executor_call = lattice_llm::ToolCall {
                    id: call.id.clone(),
                    function: lattice_llm::FunctionCall {
                        name: call.function.name.clone(),
                        arguments: call.function.arguments.clone(),
                    },
                };

                let tool_output = self.config.tool_executor.execute(&executor_call).await;

                all_events.push(AgentTurnEvent::ToolCallResult {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    ok: tool_output_is_ok(&tool_output),
                    output_len: tool_output.len(),
                });

                // Build tool summary
                let ok = tool_output_is_ok(&tool_output);
                tool_summaries.push(AgentToolResultSummary {
                    tool_call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    ok,
                    output_len: tool_output.len(),
                    error_summary: if ok {
                        None
                    } else {
                        Some(tool_output.chars().take(200).collect())
                    },
                });

                // Full tool result as GraphMessage (D-23..D-26: no summarization)
                let tool_msg = GraphMessage {
                    role: "tool".to_string(),
                    content: tool_output,
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some(call.id.clone()),
                    name: Some(call.function.name.clone()),
                };
                messages.push(tool_msg.clone());
                all_messages.push(tool_msg);
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
