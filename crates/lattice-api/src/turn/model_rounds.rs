// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Temporary multi-round sequencing adapter (#139-B Commit 4).
//!
//! `lattice-agent` now exposes only the one-attempt
//! [`ModelStepDriver`](lattice_agent::ModelStepDriver); the pre-cut
//! production path still needs multi-round model/tool sequencing, so that
//! sequencing lives here until the single production switch replaces it with
//! `TurnCoordinator` (plan §5 Commit 6). This adapter is deliberately
//! private and temporary: it must not call `reserve_batch`, publish
//! `DispatchStarted`, write the InvocationStore, or otherwise become a
//! second Turn saga owner. It continues to dispatch through the numeric CEP
//! client, exactly as the deleted in-crate loop did.
//!
//! Two behavior changes are the intended ADR 0005 §7 hardening, not drift:
//!
//! - an invalid ToolCall batch now fails the whole attempt (no empty-to-`{}`
//!   normalization, no content-only commit of a dropped call);
//! - partial assistant content from an interrupted attempt is diagnostic
//!   only and is never flushed into the transcript.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lattice_agent::{
    LlmClient, ModelStepDriver, ModelStepError, ModelStepEvent, ModelStepRequest, ModelStepStatus,
};
use lattice_conversation::{
    BoundToolCall, GraphMessage, GraphToolCall, HeadTranscript, InvocationId, MessagePart,
    MessageRole, MessageSemantic, NativeMessage, TokenUsage, ToolCallAddress, ToolCallState,
    ToolResultOutcome, UnboundToolCall,
};
use lattice_llm::{FunctionCall, ToolCall, ToolDefinition};
use tokio::time::Instant;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Hard upper bound on model rounds within a single turn (safety net until
/// the coordinator's frozen `MAX_MODEL_STEPS = 32` lands in Commit 6).
const MAX_TURN_ROUNDS: u32 = 200;

/// Maximum display columns for error summaries shown in tool-finished events.
const ERROR_SUMMARY_MAX_COLS: usize = 200;

/// Bounded capacity of the per-attempt driver-event mirror channel.
const TURN_STEP_CHANNEL_CAPACITY: usize = 256;

/// Map one driver stream event onto the pre-cut agent-event vocabulary.
/// Deltas mirror; tool-delta/end and the opened marker have no pre-cut
/// counterpart.
fn map_step_event(event: &ModelStepEvent) -> Option<AgentTurnEvent> {
    match event {
        ModelStepEvent::ReasoningDelta { content } => Some(AgentTurnEvent::ReasoningDelta {
            content: content.clone(),
        }),
        ModelStepEvent::TokenDelta { content } => Some(AgentTurnEvent::TokenDelta {
            content: content.clone(),
        }),
        ModelStepEvent::ToolCallStarted { id, name } => Some(AgentTurnEvent::ToolCallStarted {
            id: id.clone(),
            name: name.clone(),
        }),
        ModelStepEvent::Opened { .. }
        | ModelStepEvent::ToolCallDelta { .. }
        | ModelStepEvent::ToolCallEnded { .. } => None,
    }
}

/// Structured CEP error metadata preserved for API/CLI observation.
#[derive(Debug, Clone, PartialEq)]
pub struct CepErrorPayload {
    pub code: i32,
    pub message: String,
    pub data: serde_json::Value,
    pub retriability: String,
}

/// Tool execution failure with optional structured CEP metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionError {
    pub message: String,
    pub cep_error: Option<CepErrorPayload>,
}

impl ToolExecutionError {
    pub fn simple(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cep_error: None,
        }
    }
}

impl From<String> for ToolExecutionError {
    fn from(message: String) -> Self {
        Self::simple(message)
    }
}

/// Injectable tool dispatch for the temporary sequencing adapter.
///
/// Production dispatches through the CEP executor adapter; tests return
/// deterministic fake responses.
#[async_trait::async_trait]
pub(crate) trait ToolDispatch: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> Result<String, ToolExecutionError>;
}

/// Retry policy for opening provider attempts (pre-cut shape; the
/// coordinator's frozen `max_model_attempts` replaces it in Commit 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LlmRetryConfig {
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for LlmRetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            initial_delay_ms: 500,
            max_delay_ms: 3_000,
        }
    }
}

impl LlmRetryConfig {
    pub(crate) fn max_attempts(self) -> u32 {
        self.max_retries.saturating_add(1)
    }

    pub(crate) fn delay_for_retry(self, retry_index: u32) -> u64 {
        if self.max_retries == 0 {
            return 0;
        }
        let multiplier = 3_u64.saturating_pow(retry_index.saturating_sub(1));
        self.initial_delay_ms
            .saturating_mul(multiplier)
            .min(self.max_delay_ms)
    }
}

/// Input for a single pre-cut agent turn.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct AgentTurnRequest {
    pub session_id: String,
    pub transcript: HeadTranscript,
    pub user_input: String,
    pub system_prompt: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub tool_definitions: Vec<ToolDefinition>,
    pub cancel_flag: Option<Arc<AtomicBool>>,
    pub timeout: Option<Duration>,
    pub llm_retry: LlmRetryConfig,
}

/// Terminal status of a pre-cut agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentTurnStatus {
    Completed,
    Error,
    Cancelled,
}

/// Summary of a single tool execution for API display.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct AgentToolResultSummary {
    pub tool_call_id: String,
    pub name: String,
    pub ok: bool,
    pub output_len: usize,
    pub error_summary: Option<String>,
    pub cep_error: Option<CepErrorPayload>,
}

/// Result of a completed pre-cut agent turn.
///
/// Field-level dead-code allowances: the temporary adapter keeps the old
/// result shape for the collector/lifecycle layers; some fields are read
/// only by tests until the Commit 6 coordinator replaces this type.
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct AgentTurnResult {
    pub messages: Vec<GraphMessage>,
    pub assistant_output: String,
    pub usage: TokenUsage,
    pub tool_results: Vec<AgentToolResultSummary>,
    pub status: AgentTurnStatus,
    pub terminal_reason: Option<String>,
    pub events: Vec<AgentTurnEvent>,
}

/// Process-only events describing sequencing progress. Terminal state lives
/// solely on [`AgentTurnResult::status`] / [`AgentTurnResult::terminal_reason`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AgentTurnEvent {
    ModelStarted {
        provider: Option<String>,
        model: String,
    },
    LlmRequestError {
        provider: Option<String>,
        model: String,
        attempt: u32,
        max_attempts: u32,
        error_kind: String,
        retryable: bool,
        will_retry: bool,
        delay_ms: Option<u64>,
        status_code: Option<u16>,
        message: String,
    },
    ReasoningDelta {
        content: String,
    },
    TokenDelta {
        content: String,
    },
    ToolCallStarted {
        id: String,
        name: String,
    },
    ToolCallFinished {
        id: String,
        name: String,
        ok: bool,
        output_len: usize,
        error_summary: Option<String>,
    },
    ModelFinished {
        finish_reason: String,
        usage: Option<TokenUsage>,
    },
    CepCallStarted {
        call_id: u64,
        method: String,
        impact_kind: String,
        audit_id: i64,
        semantic_stage: String,
        guard_stage: String,
    },
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
}

/// The temporary multi-round sequencer over the one-attempt driver.
pub(crate) struct ModelRoundSequencer {
    driver: ModelStepDriver,
    dispatch: Arc<dyn ToolDispatch>,
}

impl ModelRoundSequencer {
    pub(crate) fn new(llm: Arc<dyn LlmClient>, dispatch: Arc<dyn ToolDispatch>) -> Self {
        Self {
            driver: ModelStepDriver::new(llm),
            dispatch,
        }
    }

    /// Execute one pre-cut turn: model step, sequential tool dispatch, repeat
    /// until a final assistant response or terminal outcome.
    pub(crate) async fn run_turn_live(
        &self,
        request: AgentTurnRequest,
        events: tokio::sync::mpsc::Sender<AgentTurnEvent>,
    ) -> AgentTurnResult {
        let deadline = request.timeout.map(|d| Instant::now() + d);
        self.run(request, Some(&events), deadline).await
    }

    async fn run(
        &self,
        request: AgentTurnRequest,
        live: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>,
        deadline: Option<Instant>,
    ) -> AgentTurnResult {
        let mut messages = build_initial_messages(&request);
        let mut all_messages: Vec<GraphMessage> = Vec::new();
        let mut all_events: Vec<AgentTurnEvent> = Vec::new();
        let mut accumulated_usage = TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        let mut tool_summaries: Vec<AgentToolResultSummary> = Vec::new();

        let cancelled = |messages: Vec<GraphMessage>,
                         all_messages: Vec<GraphMessage>,
                         usage: TokenUsage,
                         summaries: Vec<AgentToolResultSummary>,
                         events: Vec<AgentTurnEvent>| AgentTurnResult {
            messages,
            #[allow(unused_variables)]
            all_messages,
            assistant_output: String::new(),
            usage,
            tool_results: summaries,
            status: AgentTurnStatus::Cancelled,
            terminal_reason: Some("cancelled by caller".to_string()),
            events,
        };

        let mut round: u32 = 0;
        loop {
            if round >= MAX_TURN_ROUNDS {
                return AgentTurnResult {
                    messages: all_messages,
                    assistant_output: String::new(),
                    usage: accumulated_usage,
                    tool_results: tool_summaries,
                    status: AgentTurnStatus::Error,
                    terminal_reason: Some(format!(
                        "exceeded max rounds ({MAX_TURN_ROUNDS}); set a shorter timeout or fix prompt"
                    )),
                    events: all_events,
                };
            }
            round += 1;

            if is_cancelled(&request.cancel_flag) {
                return cancelled(
                    messages,
                    all_messages,
                    accumulated_usage,
                    tool_summaries,
                    all_events,
                );
            }

            emit(
                &mut all_events,
                live,
                AgentTurnEvent::ModelStarted {
                    provider: request.provider.clone(),
                    model: request.model.clone(),
                },
            )
            .await;

            if is_cancelled(&request.cancel_flag) {
                return cancelled(
                    messages,
                    all_messages,
                    accumulated_usage,
                    tool_summaries,
                    all_events,
                );
            }

            let history = match graph_buffer_to_history(&messages) {
                Ok(history) => history,
                Err(detail) => {
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: String::new(),
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Error,
                        terminal_reason: Some(detail),
                        events: all_events,
                    };
                }
            };
            let step_request = ModelStepRequest {
                system_prompt: None, // the system prompt is in the buffer
                history,
                model: request.model.clone(),
                provider: request.provider.clone(),
                tools: request.tool_definitions.clone(),
                temperature: None,
                max_tokens: None,
                cancel: request.cancel_flag.clone(),
                deadline,
            };

            // Live mirroring: forward the driver's stream deltas to the
            // agent-event channel while the attempt runs, and buffer the
            // mapped events for the returned result.
            let (step_tx, mut step_rx) =
                tokio::sync::mpsc::channel::<ModelStepEvent>(TURN_STEP_CHANNEL_CAPACITY);
            let forward_live = live.cloned();
            let buffered_step_events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let sink = std::sync::Arc::clone(&buffered_step_events);
            let forwarder = tokio::spawn(async move {
                while let Some(event) = step_rx.recv().await {
                    if let Some(mapped) = map_step_event(&event) {
                        sink.lock().unwrap().push(mapped.clone());
                        if let Some(sender) = &forward_live {
                            let _ = sender.try_send(mapped);
                        }
                    }
                }
            });

            // Opening retries: only before any stream event, per the driver's
            // typed OpeningFailed classification.
            let max_attempts = request.llm_retry.max_attempts();
            let mut attempt_ordinal = 1_u32;
            let step = loop {
                if let Some(dl) = deadline
                    && Instant::now() >= dl
                {
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
                match self.driver.run(&step_request, Some(&step_tx)).await {
                    Ok(step) => break Some(step),
                    Err(ModelStepError::OpeningFailed { message, retryable }) => {
                        let will_retry = retryable && attempt_ordinal < max_attempts;
                        let delay_ms =
                            will_retry.then(|| request.llm_retry.delay_for_retry(attempt_ordinal));
                        emit(
                            &mut all_events,
                            live,
                            AgentTurnEvent::LlmRequestError {
                                provider: request.provider.clone(),
                                model: request.model.clone(),
                                attempt: attempt_ordinal,
                                max_attempts,
                                error_kind: "transport".to_string(),
                                retryable,
                                will_retry,
                                delay_ms,
                                status_code: None,
                                message: message.clone(),
                            },
                        )
                        .await;
                        if !will_retry {
                            return AgentTurnResult {
                                messages: all_messages,
                                assistant_output: String::new(),
                                usage: accumulated_usage,
                                tool_results: tool_summaries,
                                status: AgentTurnStatus::Error,
                                terminal_reason: Some(format!("LLM error: {message}")),
                                events: all_events,
                            };
                        }
                        if let Some(delay) = delay_ms {
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                        }
                        if is_cancelled(&request.cancel_flag) {
                            return cancelled(
                                messages,
                                all_messages,
                                accumulated_usage,
                                tool_summaries,
                                all_events,
                            );
                        }
                        attempt_ordinal = attempt_ordinal.saturating_add(1);
                    }
                    Err(ModelStepError::ToolResultWithoutProviderCallId { .. }) => {
                        // Pre-cut transcripts correlate tool messages by
                        // provider id; reaching here means a corrupted
                        // buffer, which fails closed.
                        return AgentTurnResult {
                            messages: all_messages,
                            assistant_output: String::new(),
                            usage: accumulated_usage,
                            tool_results: tool_summaries,
                            status: AgentTurnStatus::Error,
                            terminal_reason: Some(
                                "recorded tool closure has no provider call id".to_string(),
                            ),
                            events: all_events,
                        };
                    }
                }
            };
            drop(step_tx);
            let _ = forwarder.await;
            all_events.extend(buffered_step_events.lock().unwrap().drain(..));
            let Some(step) = step else {
                unreachable!("the retry loop only breaks on Some");
            };

            if let Some(usage) = &step.usage {
                accumulated_usage.prompt_tokens += usage.prompt_tokens;
                accumulated_usage.completion_tokens += usage.completion_tokens;
                accumulated_usage.total_tokens += usage.total_tokens;
            }

            match step.status {
                ModelStepStatus::Interrupted(interruption) => {
                    use lattice_conversation::AttemptInterruption::*;
                    let (status, reason) = match interruption {
                        Cancelled => (
                            AgentTurnStatus::Cancelled,
                            "cancelled by caller".to_string(),
                        ),
                        Timeout | ProcessInterrupted => {
                            (AgentTurnStatus::Error, "timeout".to_string())
                        }
                        StreamError => (
                            AgentTurnStatus::Error,
                            match &step.diagnostic {
                                Some(diagnostic) => format!("LLM error: {diagnostic}"),
                                None => "LLM stream ended without completion".to_string(),
                            },
                        ),
                        OutputLimitExceeded {
                            kind,
                            limit,
                            actual,
                        } => (
                            AgentTurnStatus::Error,
                            format!("output limit exceeded ({kind:?}): {actual} > {limit}"),
                        ),
                    };
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: String::new(),
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status,
                        terminal_reason: Some(reason),
                        events: all_events,
                    };
                }
                ModelStepStatus::Rejected(rejection) => {
                    // ADR 0005 §7: an invalid complete batch produces no
                    // MessageComplete and no transcript content.
                    return AgentTurnResult {
                        messages: all_messages,
                        assistant_output: String::new(),
                        usage: accumulated_usage,
                        tool_results: tool_summaries,
                        status: AgentTurnStatus::Error,
                        terminal_reason: Some(format!("invalid tool call: {rejection:?}")),
                        events: all_events,
                    };
                }
                ModelStepStatus::Complete => {}
            }

            let message = step.message.expect("Complete carries a message");
            let finish_reason = step.finish_reason.unwrap_or_else(|| "stop".to_string());
            emit(
                &mut all_events,
                live,
                AgentTurnEvent::ModelFinished {
                    finish_reason: finish_reason.clone(),
                    usage: step.usage.clone(),
                },
            )
            .await;

            let calls: Vec<(usize, &UnboundToolCall)> = message.tool_calls().collect();
            let assistant_msg = attempt_message_to_graph(&message);

            if calls.is_empty() {
                let assistant_output = assistant_msg.content.clone();
                messages.push(assistant_msg.clone());
                all_messages.push(assistant_msg);
                return AgentTurnResult {
                    messages: all_messages,
                    assistant_output,
                    usage: accumulated_usage,
                    tool_results: tool_summaries,
                    status: AgentTurnStatus::Completed,
                    terminal_reason: Some(finish_reason),
                    events: all_events,
                };
            }

            messages.push(assistant_msg.clone());
            all_messages.push(assistant_msg);

            if is_cancelled(&request.cancel_flag) {
                return cancelled(
                    messages,
                    all_messages,
                    accumulated_usage,
                    tool_summaries,
                    all_events,
                );
            }

            for (_, call) in calls {
                if is_cancelled(&request.cancel_flag) {
                    return cancelled(
                        messages,
                        all_messages,
                        accumulated_usage,
                        tool_summaries,
                        all_events,
                    );
                }

                emit(
                    &mut all_events,
                    live,
                    AgentTurnEvent::ToolCallStarted {
                        id: call.provider_call_id.clone().unwrap_or_default(),
                        name: call.name.clone(),
                    },
                )
                .await;

                let executor_call = ToolCall {
                    id: call.provider_call_id.clone().unwrap_or_default(),
                    function: FunctionCall {
                        name: call.name.clone(),
                        arguments: lattice_conversation::canonical_json(call.arguments()),
                    },
                };

                let dispatch_result = match deadline {
                    Some(dl) => {
                        match tokio::time::timeout_at(dl, self.dispatch.execute(&executor_call))
                            .await
                        {
                            Ok(result) => result,
                            Err(_) => {
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
                    None => self.dispatch.execute(&executor_call).await,
                };
                let (ok, tool_content, cep_error) = match dispatch_result {
                    Ok(output) => (true, output, None),
                    Err(error) => (false, error.message.clone(), error.cep_error.clone()),
                };

                let error_summary = if ok {
                    None
                } else {
                    Some(truncate_to_width(&tool_content, ERROR_SUMMARY_MAX_COLS))
                };

                emit(
                    &mut all_events,
                    live,
                    AgentTurnEvent::ToolCallFinished {
                        id: executor_call.id.clone(),
                        name: call.name.clone(),
                        ok,
                        output_len: tool_content.chars().count(),
                        error_summary: error_summary.clone(),
                    },
                )
                .await;

                tool_summaries.push(AgentToolResultSummary {
                    tool_call_id: executor_call.id.clone(),
                    name: call.name.clone(),
                    ok,
                    output_len: tool_content.chars().count(),
                    error_summary,
                    cep_error,
                });

                let tool_msg = GraphMessage {
                    role: "tool".to_string(),
                    content: tool_content,
                    reasoning_content: None,
                    tool_calls: None,
                    tool_call_id: Some(executor_call.id.clone()),
                    name: Some(call.name.clone()),
                };
                messages.push(tool_msg.clone());
                all_messages.push(tool_msg);

                if is_cancelled(&request.cancel_flag) {
                    return cancelled(
                        messages,
                        all_messages,
                        accumulated_usage,
                        tool_summaries,
                        all_events,
                    );
                }
            }
        }
    }
}

fn is_cancelled(flag: &Option<Arc<AtomicBool>>) -> bool {
    flag.as_ref()
        .map(|flag| flag.load(Ordering::Relaxed))
        .unwrap_or(false)
}

async fn emit(
    buffered: &mut Vec<AgentTurnEvent>,
    live: Option<&tokio::sync::mpsc::Sender<AgentTurnEvent>>,
    event: AgentTurnEvent,
) {
    buffered.push(event.clone());
    if let Some(sender) = live {
        let _ = sender.try_send(event);
    }
}

fn build_initial_messages(request: &AgentTurnRequest) -> Vec<GraphMessage> {
    let mut messages = Vec::new();
    if let Some(system_prompt) = &request.system_prompt {
        messages.push(GraphMessage {
            role: "system".to_string(),
            content: system_prompt.clone(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }
    messages.extend(request.transcript.messages.iter().cloned());
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

/// Convert the pre-cut `GraphMessage` buffer into the bound native history
/// the driver consumes.
///
/// The `InvocationId`s minted here are request-assembly-only: they never
/// become durable, are never sent on the wire, and disappear with the
/// buffer. The #139-B coordinator replaces this whole conversion when
/// reservation lands (plan §5 Commit 6).
fn graph_buffer_to_history(
    messages: &[GraphMessage],
) -> Result<Vec<NativeMessage<BoundToolCall>>, String> {
    // provider call id -> invocation id, for joining tool messages.
    let mut invocation_of: std::collections::BTreeMap<String, InvocationId> =
        std::collections::BTreeMap::new();
    let mut history = Vec::with_capacity(messages.len());
    for message in messages {
        match message.role.as_str() {
            "system" => history.push(NativeMessage::new(
                MessageRole::System,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: message.content.clone(),
                })],
            )),
            "user" => history.push(NativeMessage::new(
                MessageRole::User,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: message.content.clone(),
                })],
            )),
            "assistant" => {
                let mut parts = Vec::new();
                if let Some(reasoning) = &message.reasoning_content {
                    parts.push(MessagePart::semantic_only(MessageSemantic::Reasoning {
                        text: reasoning.clone(),
                    }));
                }
                if !message.content.is_empty() {
                    parts.push(MessagePart::semantic_only(MessageSemantic::Text {
                        text: message.content.clone(),
                    }));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        let invocation = fresh_local_invocation();
                        invocation_of.insert(call.id.clone(), invocation);
                        parts.push(MessagePart::semantic_only(MessageSemantic::ToolCall {
                            call: UnboundToolCall {
                                provider_call_id: Some(call.id.clone()),
                                name: call.name.clone(),
                                arguments: serde_json::from_str(&call.arguments)
                                    .unwrap_or(serde_json::json!({})),
                            }
                            .bind(invocation, local_address()),
                        }));
                    }
                }
                history.push(NativeMessage::new(MessageRole::Assistant, None, parts));
            }
            "tool" => {
                let invocation = match message
                    .tool_call_id
                    .as_ref()
                    .and_then(|id| invocation_of.get(id))
                {
                    Some(invocation) => *invocation,
                    None => {
                        // A tool message with no matching assistant call cannot
                        // be correlated; fail closed rather than guessing.
                        return Err(format!(
                            "tool message '{}' has no matching assistant tool call",
                            message.name.as_deref().unwrap_or("?")
                        ));
                    }
                };
                history.push(NativeMessage::new(
                    MessageRole::Tool,
                    None,
                    vec![MessagePart::semantic_only(MessageSemantic::ToolResult {
                        invocation_id: invocation,
                        name: message.name.clone().unwrap_or_default(),
                        outcome: ToolResultOutcome::Succeeded,
                        content: Some(message.content.clone()),
                    })],
                ));
            }
            other => return Err(format!("unknown message role '{other}'")),
        }
    }
    Ok(history)
}

/// Attempt message → the pre-cut commit-buffer shape.
fn attempt_message_to_graph(message: &NativeMessage<UnboundToolCall>) -> GraphMessage {
    let mut content = String::new();
    let mut reasoning_content = None;
    let mut tool_calls = Vec::new();
    for part in &message.parts {
        match &part.semantic {
            MessageSemantic::Text { text } => content.push_str(text),
            MessageSemantic::Reasoning { text } => {
                let slot = reasoning_content.get_or_insert_with(String::new);
                slot.push_str(text);
            }
            MessageSemantic::ToolCall { call } => {
                tool_calls.push(GraphToolCall {
                    id: call.provider_call_id.clone().unwrap_or_default(),
                    name: call.name.clone(),
                    arguments: lattice_conversation::canonical_json(call.arguments()),
                });
            }
            MessageSemantic::ToolResult { .. } => {}
        }
    }
    GraphMessage {
        role: "assistant".to_string(),
        content,
        reasoning_content,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
        tool_call_id: None,
        name: None,
    }
}

/// Request-assembly-only invocation identity (never durable, never on the
/// wire; see [`graph_buffer_to_history`]).
fn fresh_local_invocation() -> InvocationId {
    InvocationId::parse(&uuid::Uuid::new_v4().to_string())
        .expect("uuid v4 display is canonical hyphenated lowercase")
}

/// Structural address for a locally bound call; content-free, used only to
/// satisfy the bound typestate.
fn local_address() -> ToolCallAddress {
    ToolCallAddress {
        message: lattice_conversation::ModelMessageAddress {
            session_id: lattice_conversation::SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c")
                .expect("placeholder session id"),
            turn_id: lattice_conversation::TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c")
                .expect("placeholder turn id"),
            model_step_ordinal: 0,
        },
        part_ordinal: 0,
    }
}

/// Truncate `s` to at most `max_width` display columns, appending `…` if cut.
fn truncate_to_width(s: &str, max_width: usize) -> String {
    if s.width() <= max_width {
        return s.to_string();
    }
    let mut width = 0_usize;
    let mut end = 0_usize;
    let ellipsis_width = '…'.width().unwrap_or(1);
    let limit = max_width.saturating_sub(ellipsis_width);
    for (i, c) in s.char_indices() {
        let cw = c.width().unwrap_or(0);
        if width + cw > limit {
            break;
        }
        width += cw;
        end = i + c.len_utf8();
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}
