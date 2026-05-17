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
//! - D-08: No fixed tool-round counter; loop until final assistant response.
//! - D-09/D-10: External timeout/cancel as run protection.
//! - D-11: Build model input from complete session transcript + user input.
//! - D-12: No transcript compression, summaries, or prompt-budget logic.
//! - D-13..D-18: Only explicit enabled tool names; no bundle expansion.
//! - D-19..D-22: Project-root path containment via executor.
//! - D-23..D-26: Full tool results in transcript; no summarization/clipping.
//! - T-09-02: `resolve_enabled_tools` called before LLM call.
//! - T-09-03: Result carries status, terminal reason, messages, usage, summaries.
//! - T-09-04: External timeout/cancel; no heuristic round limits.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use lattice_session::{GraphMessage, GraphToolCall, HeadTranscript, TurnUsage};
use lattice_tools::enabled_tools::{resolve_enabled_tools, EnabledTools};

// ---------------------------------------------------------------------------
// Public stable types (D-03, D-04)
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

// ---------------------------------------------------------------------------
// Request / Result / Event types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Injectable LLM and tool seams
// ---------------------------------------------------------------------------

/// Injectable LLM client for the agent loop.
///
/// Production implementations call real providers. Test implementations
/// return deterministic fake responses.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Execute a chat completion.
    async fn chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>;
}

/// Injectable tool executor for the agent loop.
///
/// Wraps the existing `DefaultToolExecutor` or a test fake.
#[async_trait::async_trait]
pub trait ToolCallExecutor: Send + Sync {
    /// Execute a tool call and return the output string.
    async fn execute(&self, call: &lattice_llm::ToolCall) -> String;
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

/// Convert a `GraphMessage` to an `lattice_llm::Message` for the LLM request.
fn convert_to_llm_message(msg: &GraphMessage) -> Result<lattice_llm::Message, String> {
    let role = match msg.role.as_str() {
        "system" => lattice_llm::Role::System,
        "user" => lattice_llm::Role::User,
        "assistant" => lattice_llm::Role::Assistant,
        "tool" => lattice_llm::Role::Tool,
        other => return Err(format!("unknown message role '{}'", other)),
    };

    Ok(lattice_llm::Message {
        role,
        content: msg.content.clone(),
        reasoning_content: msg.reasoning_content.clone(),
        tool_calls: msg.tool_calls.as_ref().map(|calls| {
            calls
                .iter()
                .map(|tc| lattice_llm::ToolCall {
                    id: tc.id.clone(),
                    function: lattice_llm::FunctionCall {
                        name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    },
                })
                .collect()
        }),
        tool_call_id: msg.tool_call_id.clone(),
        name: msg.name.clone(),
    })
}

fn tool_output_is_ok(output: &str) -> bool {
    !output.starts_with("Error:") && !output.starts_with("Tool denied:")
}

// ---------------------------------------------------------------------------
// Tests (TDD)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A fake LLM that returns a predetermined sequence of responses.
    struct FakeLlm {
        responses: std::sync::Mutex<Vec<Result<lattice_llm::ChatResponse, String>>>,
        call_count: AtomicUsize,
    }

    impl FakeLlm {
        fn new(responses: Vec<Result<lattice_llm::ChatResponse, String>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses),
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmClient for FakeLlm {
        async fn chat(
            &self,
            _request: lattice_llm::ChatRequest,
        ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err("no more fake responses".into());
            }
            let resp = responses.remove(0);
            resp.map_err(|e| e.into())
        }
    }

    /// A fake tool executor that returns predetermined results.
    struct FakeToolExecutor {
        results: std::sync::Mutex<Vec<String>>,
    }

    impl FakeToolExecutor {
        fn new(results: Vec<String>) -> Self {
            Self {
                results: std::sync::Mutex::new(results),
            }
        }
    }

    #[async_trait::async_trait]
    impl ToolCallExecutor for FakeToolExecutor {
        async fn execute(&self, _call: &lattice_llm::ToolCall) -> String {
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                return "no more fake results".to_string();
            }
            results.remove(0)
        }
    }

    fn make_request(user_input: &str, enabled_tools: Vec<String>) -> AgentTurnRequest {
        AgentTurnRequest {
            session_id: "test-session".to_string(),
            transcript: HeadTranscript {
                session_id: "test-session".to_string(),
                parent_commit_id: None,
                messages: vec![],
            },
            user_input: user_input.to_string(),
            system_prompt: None,
            model: "test-model".to_string(),
            provider: None,
            enabled_tools,
            project_root: "/tmp".to_string(),
            cancel_flag: None,
            timeout: None,
        }
    }

    fn make_request_with_transcript(
        user_input: &str,
        transcript_messages: Vec<GraphMessage>,
        enabled_tools: Vec<String>,
    ) -> AgentTurnRequest {
        AgentTurnRequest {
            session_id: "test-session".to_string(),
            transcript: HeadTranscript {
                session_id: "test-session".to_string(),
                parent_commit_id: Some("parent-1".to_string()),
                messages: transcript_messages,
            },
            user_input: user_input.to_string(),
            system_prompt: None,
            model: "test-model".to_string(),
            provider: None,
            enabled_tools,
            project_root: "/tmp".to_string(),
            cancel_flag: None,
            timeout: None,
        }
    }

    // ── Test: public contract uses stable names ─────────────────────────

    #[test]
    fn agent_loop_public_contract_uses_stable_names() {
        // Verify that the stable public names are exported and usable.
        // This is a compile-time check: if the types exist, the test passes.
        let _ = std::any::TypeId::of::<AgentLoop>();
        let _ = std::any::TypeId::of::<AgentLoopConfig>();
        let _ = std::any::TypeId::of::<AgentTurnRequest>();
        let _ = std::any::TypeId::of::<AgentTurnResult>();
        let _ = std::any::TypeId::of::<AgentTurnEvent>();
        let _ = std::any::TypeId::of::<AgentTurnStatus>();

        // Verify no V11, Simple, or New public names exist in the module.
        // This is enforced by the module's public API surface.
    }

    // ── Test: messages built from transcript + user input ────────────────

    #[tokio::test]
    async fn agent_loop_builds_messages_from_transcript_and_user_input() {
        let transcript_messages = vec![
            GraphMessage {
                role: "user".to_string(),
                content: "previous question".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            GraphMessage {
                role: "assistant".to_string(),
                content: "previous answer".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        let llm = Arc::new(FakeLlm::new(vec![Ok(lattice_llm::ChatResponse {
            content: "final answer".to_string(),
            reasoning_content: None,
            tool_calls: None,
            finish_reason: "stop".to_string(),
            model: "test-model".to_string(),
            usage: None,
        })]));

        // Track the request sent to the LLM
        let request_capture: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>> =
            Arc::new(std::sync::Mutex::new(None));
        let capture_clone = request_capture.clone();

        struct CapturingLlm {
            inner: Arc<FakeLlm>,
            captured: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>>,
        }

        #[async_trait::async_trait]
        impl LlmClient for CapturingLlm {
            async fn chat(
                &self,
                request: lattice_llm::ChatRequest,
            ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
            {
                *self.captured.lock().unwrap() = Some(request.clone());
                self.inner.chat(request).await
            }
        }

        let capturing_llm = Arc::new(CapturingLlm {
            inner: llm,
            captured: capture_clone,
        });

        let executor = Arc::new(FakeToolExecutor::new(vec![]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm: capturing_llm,
            tool_executor: executor,
        });

        let request = make_request_with_transcript(
            "current question",
            transcript_messages,
            vec!["read_file".to_string()],
        );

        let result = loop_inst.run_turn(request).await;
        assert_eq!(result.status, AgentTurnStatus::Completed);

        // Verify the LLM received the correct messages
        let captured = request_capture.lock().unwrap().take().unwrap();
        assert_eq!(captured.messages.len(), 3); // prev user + prev assistant + current user
        assert_eq!(captured.messages[0].content, "previous question");
        assert_eq!(captured.messages[0].role, lattice_llm::Role::User);
        assert_eq!(captured.messages[1].content, "previous answer");
        assert_eq!(captured.messages[1].role, lattice_llm::Role::Assistant);
        assert_eq!(captured.messages[2].content, "current question");
        assert_eq!(captured.messages[2].role, lattice_llm::Role::User);
    }

    // ── Test: system prompt prepended ────────────────────────────────────

    #[tokio::test]
    async fn agent_loop_prepends_system_prompt_when_present() {
        let request_capture: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>> =
            Arc::new(std::sync::Mutex::new(None));
        let capture_clone = request_capture.clone();

        struct CapturingLlm {
            captured: Arc<std::sync::Mutex<Option<lattice_llm::ChatRequest>>>,
        }

        #[async_trait::async_trait]
        impl LlmClient for CapturingLlm {
            async fn chat(
                &self,
                request: lattice_llm::ChatRequest,
            ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
            {
                *self.captured.lock().unwrap() = Some(request.clone());
                Ok(lattice_llm::ChatResponse {
                    content: "ok".to_string(),
                    reasoning_content: None,
                    tool_calls: None,
                    finish_reason: "stop".to_string(),
                    model: "test".to_string(),
                    usage: None,
                })
            }
        }

        let llm = Arc::new(CapturingLlm {
            captured: capture_clone,
        });
        let executor = Arc::new(FakeToolExecutor::new(vec![]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let mut request = make_request("hello", vec!["read_file".to_string()]);
        request.system_prompt = Some("You are a helpful assistant.".to_string());

        let result = loop_inst.run_turn(request).await;
        assert_eq!(result.status, AgentTurnStatus::Completed);

        let captured = request_capture.lock().unwrap().take().unwrap();
        assert_eq!(captured.messages.len(), 2); // system + user
        assert_eq!(captured.messages[0].role, lattice_llm::Role::System);
        assert_eq!(captured.messages[0].content, "You are a helpful assistant.");
        assert_eq!(captured.messages[1].role, lattice_llm::Role::User);
        assert_eq!(captured.messages[1].content, "hello");
    }

    // ── Test: unknown enabled tools fail before LLM call ────────────────

    #[tokio::test]
    async fn agent_loop_unknown_enabled_tools_fails_before_llm_call() {
        let llm = Arc::new(FakeLlm::new(vec![])); // No responses -- should not be called
        let executor = Arc::new(FakeToolExecutor::new(vec![]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let request = make_request(
            "hello",
            vec!["read_file".to_string(), "nonexistent_tool".to_string()],
        );

        let result = loop_inst.run_turn(request).await;
        assert_eq!(result.status, AgentTurnStatus::Failed);
        assert!(result
            .terminal_reason
            .as_ref()
            .unwrap()
            .contains("unknown enabled tools"));
        assert!(result
            .terminal_reason
            .as_ref()
            .unwrap()
            .contains("nonexistent_tool"));
        assert!(result.messages.is_empty());
        assert_eq!(result.assistant_output, "");
    }

    // ── Task 2 behavioral tests ─────────────────────────────────────────

    #[tokio::test]
    async fn agent_loop_executes_one_tool_then_final_answer() {
        // Fake LLM: first call returns tool call, second call returns final text
        let llm = Arc::new(FakeLlm::new(vec![
            Ok(lattice_llm::ChatResponse {
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![lattice_llm::ToolCall {
                    id: "call_1".to_string(),
                    function: lattice_llm::FunctionCall {
                        name: "read_file".to_string(),
                        arguments: r#"{"path": "test.txt"}"#.to_string(),
                    },
                }]),
                finish_reason: "tool_calls".to_string(),
                model: "test-model".to_string(),
                usage: Some(lattice_llm::TokenUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                }),
            }),
            Ok(lattice_llm::ChatResponse {
                content: "Here is the file content.".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "test-model".to_string(),
                usage: Some(lattice_llm::TokenUsage {
                    prompt_tokens: 20,
                    completion_tokens: 10,
                    total_tokens: 30,
                }),
            }),
        ]));

        let executor = Arc::new(FakeToolExecutor::new(vec!["file content here".to_string()]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let request = make_request("read test.txt", vec!["read_file".to_string()]);
        let result = loop_inst.run_turn(request).await;

        assert_eq!(result.status, AgentTurnStatus::Completed);
        assert_eq!(result.assistant_output, "Here is the file content.");
        // Two LLM calls
        // (verified by the FakeLlm returning exactly 2 responses)
        // Usage summed across both calls
        assert_eq!(result.usage.prompt_tokens, 30); // 10 + 20
        assert_eq!(result.usage.completion_tokens, 15); // 5 + 10
        assert_eq!(result.usage.total_tokens, 45); // 15 + 30

        // Tool results
        assert_eq!(result.tool_results.len(), 1);
        assert_eq!(result.tool_results[0].name, "read_file");
        assert!(result.tool_results[0].ok);
        // Messages: assistant(tool_call) + tool + assistant(final)
        assert_eq!(result.messages.len(), 3);
        assert_eq!(result.messages[0].role, "assistant");
        assert_eq!(result.messages[1].role, "tool");
        assert_eq!(result.messages[1].tool_call_id, Some("call_1".to_string()));
        assert_eq!(result.messages[2].role, "assistant");
        assert_eq!(result.messages[2].content, "Here is the file content.");
    }

    #[tokio::test]
    async fn agent_loop_executes_tool_calls_sequentially() {
        // Fake LLM: first call returns two tool calls, second call returns final
        let llm = Arc::new(FakeLlm::new(vec![
            Ok(lattice_llm::ChatResponse {
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![
                    lattice_llm::ToolCall {
                        id: "call_a".to_string(),
                        function: lattice_llm::FunctionCall {
                            name: "read_file".to_string(),
                            arguments: r#"{"path": "a.txt"}"#.to_string(),
                        },
                    },
                    lattice_llm::ToolCall {
                        id: "call_b".to_string(),
                        function: lattice_llm::FunctionCall {
                            name: "read_file".to_string(),
                            arguments: r#"{"path": "b.txt"}"#.to_string(),
                        },
                    },
                ]),
                finish_reason: "tool_calls".to_string(),
                model: "test-model".to_string(),
                usage: None,
            }),
            Ok(lattice_llm::ChatResponse {
                content: "Done reading both files.".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "test-model".to_string(),
                usage: None,
            }),
        ]));

        // Track execution order
        let execution_order: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let order_clone = execution_order.clone();

        struct OrderTrackingExecutor {
            order: Arc<std::sync::Mutex<Vec<String>>>,
        }

        #[async_trait::async_trait]
        impl ToolCallExecutor for OrderTrackingExecutor {
            async fn execute(&self, call: &lattice_llm::ToolCall) -> String {
                self.order.lock().unwrap().push(call.id.clone());
                format!("content of {}", call.function.name)
            }
        }

        let executor = Arc::new(OrderTrackingExecutor { order: order_clone });
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let request = make_request("read both", vec!["read_file".to_string()]);
        let result = loop_inst.run_turn(request).await;

        assert_eq!(result.status, AgentTurnStatus::Completed);
        // Tool calls executed in model order
        let order = execution_order.lock().unwrap();
        assert_eq!(*order, vec!["call_a", "call_b"]);
        // Messages: assistant + tool_a + tool_b + assistant(final)
        assert_eq!(result.messages.len(), 4);
        assert_eq!(result.messages[0].role, "assistant");
        assert_eq!(result.messages[1].role, "tool");
        assert_eq!(result.messages[1].tool_call_id, Some("call_a".to_string()));
        assert_eq!(result.messages[2].role, "tool");
        assert_eq!(result.messages[2].tool_call_id, Some("call_b".to_string()));
        assert_eq!(result.messages[3].role, "assistant");
    }

    #[tokio::test]
    async fn agent_loop_persists_full_tool_text_in_messages() {
        let large_output = "x".repeat(10_000);
        let large_output_clone = large_output.clone();

        let llm = Arc::new(FakeLlm::new(vec![
            Ok(lattice_llm::ChatResponse {
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![lattice_llm::ToolCall {
                    id: "call_1".to_string(),
                    function: lattice_llm::FunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                finish_reason: "tool_calls".to_string(),
                model: "test-model".to_string(),
                usage: None,
            }),
            Ok(lattice_llm::ChatResponse {
                content: "ok".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "test-model".to_string(),
                usage: None,
            }),
        ]));

        let executor = Arc::new(FakeToolExecutor::new(vec![large_output_clone]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let request = make_request("read file", vec!["read_file".to_string()]);
        let result = loop_inst.run_turn(request).await;

        assert_eq!(result.status, AgentTurnStatus::Completed);
        // Find the tool message
        let tool_msg = result
            .messages
            .iter()
            .find(|m| m.role == "tool")
            .expect("should have tool message");
        // Full tool text persisted, not summarized
        assert_eq!(tool_msg.content, large_output);
        assert_eq!(tool_msg.tool_call_id, Some("call_1".to_string()));
        assert_eq!(tool_msg.name, Some("read_file".to_string()));
    }

    #[tokio::test]
    async fn agent_loop_accumulates_usage() {
        let llm = Arc::new(FakeLlm::new(vec![
            Ok(lattice_llm::ChatResponse {
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![lattice_llm::ToolCall {
                    id: "call_1".to_string(),
                    function: lattice_llm::FunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                finish_reason: "tool_calls".to_string(),
                model: "test-model".to_string(),
                usage: Some(lattice_llm::TokenUsage {
                    prompt_tokens: 100,
                    completion_tokens: 20,
                    total_tokens: 120,
                }),
            }),
            Ok(lattice_llm::ChatResponse {
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![lattice_llm::ToolCall {
                    id: "call_2".to_string(),
                    function: lattice_llm::FunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                finish_reason: "tool_calls".to_string(),
                model: "test-model".to_string(),
                usage: Some(lattice_llm::TokenUsage {
                    prompt_tokens: 200,
                    completion_tokens: 30,
                    total_tokens: 230,
                }),
            }),
            Ok(lattice_llm::ChatResponse {
                content: "done".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "test-model".to_string(),
                usage: Some(lattice_llm::TokenUsage {
                    prompt_tokens: 300,
                    completion_tokens: 40,
                    total_tokens: 340,
                }),
            }),
        ]));

        let executor = Arc::new(FakeToolExecutor::new(vec![
            "result1".to_string(),
            "result2".to_string(),
        ]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let request = make_request("read files", vec!["read_file".to_string()]);
        let result = loop_inst.run_turn(request).await;

        assert_eq!(result.status, AgentTurnStatus::Completed);
        // Usage summed across all 3 LLM calls
        assert_eq!(result.usage.prompt_tokens, 600); // 100 + 200 + 300
        assert_eq!(result.usage.completion_tokens, 90); // 20 + 30 + 40
        assert_eq!(result.usage.total_tokens, 690); // 120 + 230 + 340
    }

    #[tokio::test]
    async fn agent_loop_timeout_returns_failed_status() {
        // Fake LLM that sleeps longer than the timeout
        struct SlowLlm;

        #[async_trait::async_trait]
        impl LlmClient for SlowLlm {
            async fn chat(
                &self,
                _request: lattice_llm::ChatRequest,
            ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
            {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(lattice_llm::ChatResponse {
                    content: "should not reach here".to_string(),
                    reasoning_content: None,
                    tool_calls: None,
                    finish_reason: "stop".to_string(),
                    model: "test".to_string(),
                    usage: None,
                })
            }
        }

        let llm = Arc::new(SlowLlm);
        let executor = Arc::new(FakeToolExecutor::new(vec![]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let mut request = make_request("hello", vec!["read_file".to_string()]);
        request.timeout = Some(Duration::from_millis(50));

        let started = std::time::Instant::now();
        let result = loop_inst.run_turn(request).await;

        assert_eq!(result.status, AgentTurnStatus::Failed);
        assert!(result.terminal_reason.as_ref().unwrap().contains("timeout"));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "timeout should return promptly"
        );
        // Partial messages are empty because timeout happened before any response
        assert!(result.messages.is_empty());
    }

    #[tokio::test]
    async fn agent_loop_cancel_returns_cancelled_status() {
        // Fake LLM that checks cancel flag between calls
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel_flag.clone();

        struct CancelAwareLlm {
            cancel: Arc<AtomicBool>,
            call_count: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl LlmClient for CancelAwareLlm {
            async fn chat(
                &self,
                _request: lattice_llm::ChatRequest,
            ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
            {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);
                if count > 0 {
                    // Second call: simulate cancel between rounds
                    self.cancel.store(true, Ordering::Relaxed);
                }
                Ok(lattice_llm::ChatResponse {
                    content: String::new(),
                    reasoning_content: None,
                    tool_calls: Some(vec![lattice_llm::ToolCall {
                        id: format!("call_{count}"),
                        function: lattice_llm::FunctionCall {
                            name: "read_file".to_string(),
                            arguments: "{}".to_string(),
                        },
                    }]),
                    finish_reason: "tool_calls".to_string(),
                    model: "test".to_string(),
                    usage: None,
                })
            }
        }

        let llm = Arc::new(CancelAwareLlm {
            cancel: cancel_clone,
            call_count: AtomicUsize::new(0),
        });
        let executor = Arc::new(FakeToolExecutor::new(vec![
            "result1".to_string(),
            "result2".to_string(),
        ]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let mut request = make_request("hello", vec!["read_file".to_string()]);
        request.cancel_flag = Some(cancel_flag);

        let result = loop_inst.run_turn(request).await;

        assert_eq!(result.status, AgentTurnStatus::Cancelled);
        assert!(result
            .terminal_reason
            .as_ref()
            .unwrap()
            .contains("cancelled"));
        // Partial messages retained (assistant + tool from first round)
        assert!(!result.messages.is_empty());
    }

    #[tokio::test]
    async fn agent_loop_emits_tool_activity_events() {
        let llm = Arc::new(FakeLlm::new(vec![
            Ok(lattice_llm::ChatResponse {
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![lattice_llm::ToolCall {
                    id: "call_1".to_string(),
                    function: lattice_llm::FunctionCall {
                        name: "read_file".to_string(),
                        arguments: "{}".to_string(),
                    },
                }]),
                finish_reason: "tool_calls".to_string(),
                model: "test-model".to_string(),
                usage: None,
            }),
            Ok(lattice_llm::ChatResponse {
                content: "final answer".to_string(),
                reasoning_content: None,
                tool_calls: None,
                finish_reason: "stop".to_string(),
                model: "test-model".to_string(),
                usage: None,
            }),
        ]));

        let executor = Arc::new(FakeToolExecutor::new(vec!["file content".to_string()]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let request = make_request("read file", vec!["read_file".to_string()]);
        let result = loop_inst.run_turn(request).await;

        assert_eq!(result.status, AgentTurnStatus::Completed);

        // Check event sequence
        let events = &result.events;
        assert!(
            matches!(events[0], AgentTurnEvent::Started),
            "first event should be Started"
        );

        // Should have Token event for first assistant response (empty content, but still emitted)
        // Then ToolCallStart, ToolCallResult, Token (final), Done
        let has_tool_start = events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::ToolCallStart { id, .. } if id == "call_1"));
        assert!(has_tool_start, "should have ToolCallStart event");

        let has_tool_result = events.iter().any(|e| {
            matches!(
                e,
                AgentTurnEvent::ToolCallResult {
                    id,
                    name,
                    ok: true,
                    ..
                } if id == "call_1" && name == "read_file"
            )
        });
        assert!(has_tool_result, "should have ToolCallResult event");

        let has_done = events.iter().any(
            |e| matches!(e, AgentTurnEvent::Done { finish_reason } if finish_reason == "stop"),
        );
        assert!(has_done, "should have Done event");

        let has_token = events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::Token { content } if content == "final answer"));
        assert!(has_token, "should have Token event with final answer");
    }

    #[tokio::test]
    async fn agent_loop_llm_error_returns_failed_status() {
        let llm = Arc::new(FakeLlm::new(vec![Err("provider unavailable".to_string())]));
        let executor = Arc::new(FakeToolExecutor::new(vec![]));
        let loop_inst = AgentLoop::new(AgentLoopConfig {
            llm,
            tool_executor: executor,
        });

        let request = make_request("hello", vec!["read_file".to_string()]);
        let result = loop_inst.run_turn(request).await;

        assert_eq!(result.status, AgentTurnStatus::Failed);
        assert!(result
            .terminal_reason
            .as_ref()
            .unwrap()
            .contains("LLM error"));
        assert!(result
            .terminal_reason
            .as_ref()
            .unwrap()
            .contains("provider unavailable"));

        let has_error = result
            .events
            .iter()
            .any(|e| matches!(e, AgentTurnEvent::Error { message } if message.contains("provider unavailable")));
        assert!(has_error, "should have Error event");
    }
}
