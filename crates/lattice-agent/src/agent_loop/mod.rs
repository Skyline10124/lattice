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
                all_events.push(AgentTurnEvent::Error {
                    message: reason.clone(),
                });
                return AgentTurnResult {
                    messages: all_messages,
                    assistant_output: String::new(),
                    usage: accumulated_usage,
                    tool_results: tool_summaries,
                    status: AgentTurnStatus::Failed,
                    terminal_reason: Some(reason),
                    events: all_events,
                };
            }
            round += 1;

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

                let tool_result = self.config.tool_executor.execute(&executor_call).await;
                let (ok, tool_content) = match tool_result {
                    Ok(output) => (true, output),
                    Err(error) => (false, error),
                };

                all_events.push(AgentTurnEvent::ToolCallResult {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    ok,
                    output_len: tool_content.len(),
                });

                tool_summaries.push(AgentToolResultSummary {
                    tool_call_id: call.id.clone(),
                    name: call.function.name.clone(),
                    ok,
                    output_len: tool_content.len(),
                    error_summary: if ok {
                        None
                    } else {
                        Some(tool_content.chars().take(200).collect())
                    },
                });

                // Full tool result as GraphMessage (D-23..D-26: no summarization)
                let tool_msg = GraphMessage {
                    role: "tool".to_string(),
                    content: tool_content,
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
