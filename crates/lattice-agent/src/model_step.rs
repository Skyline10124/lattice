// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! One-attempt model step driver (ADR 0005 §10, #139-B Commit 4).
//!
//! `ModelStepDriver` executes exactly one provider attempt per call: it
//! assembles the frozen request, streams one provider response, meters every
//! delta against the ADR 0005 §10 protocol ceilings through
//! [`lattice_conversation::AttemptAssembly`], and returns one typed attempt
//! outcome with the authoritative ordered message assembly.
//!
//! It owns no session or invocation store, no UUID generation, no multi-step
//! loop, no tool executor, no retry policy, and no Turn-commit authority:
//! those moved to the composition layer. Opening failures are surfaced as
//! [`ModelStepStatus::OpeningFailed`] with a retryable *hint*; whether to
//! retry — and only before any stream event — is the caller's decision.
//!
//! Every provider Tool Call ID is preserved verbatim when the provider
//! supplies one and is never synthesized. A conversation ToolResult closure
//! whose closing call carries no provider ID fails the request assembly with
//! a typed error (plan decision 9) instead of inventing an ID.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt;
use lattice_conversation::{
    AttemptAssembly, AttemptInterruption, AttemptRejection, InvocationId, MessagePart, MessageRole,
    MessageSemantic, NativeMessage, RawToolCall, ToolCallState, ToolResultOutcome, UnboundToolCall,
    validate_tool_call_batch,
};
use lattice_llm::{
    ChatRequest, FunctionCall, LlmEvent, Message, ModelTarget, Role, ToolCall, ToolDefinition,
};
use tokio::time::Instant;

/// Polling interval for cooperative cancellation while a provider stream is
/// stalled (no events arriving). The driver races `stream.next()` against
/// both the deadline and this poll so a cancel during a stall is observed
/// within this bound even when no deadline is configured.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Injectable LLM client seam: the provider boundary the driver streams one
/// attempt through.
///
/// Production implementations call real providers through `lattice-llm`.
/// Test implementations return deterministic fake responses
/// ([`crate::testing::FakeStreamingLlmClient`]).
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Execute a chat completion.
    async fn chat(
        &self,
        request: ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>;

    /// Execute a chat completion as a stream.
    ///
    /// The default is a single-blob fallback over [`Self::chat`]: startup or
    /// dispatch failures propagate through `Result::Err`, while provider
    /// implementations stream real deltas and may use in-band
    /// [`LlmEvent::Error`] values for failures after the stream opened.
    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        let response = self.chat(request).await?;
        let mut events: Vec<LlmEvent> = Vec::new();

        if let Some(reasoning) = &response.reasoning_content
            && !reasoning.is_empty()
        {
            events.push(LlmEvent::Reasoning {
                content: reasoning.clone(),
            });
        }

        if !response.content.is_empty() {
            events.push(LlmEvent::Token {
                content: response.content.clone(),
            });
        }

        if let Some(tool_calls) = &response.tool_calls {
            for tool_call in tool_calls {
                events.push(LlmEvent::ToolCallStart {
                    id: tool_call.id.clone(),
                    name: tool_call.function.name.clone(),
                });

                if !tool_call.function.arguments.is_empty() {
                    events.push(LlmEvent::ToolCallDelta {
                        id: tool_call.id.clone(),
                        arguments_delta: tool_call.function.arguments.clone(),
                    });
                }

                events.push(LlmEvent::ToolCallEnd {
                    id: tool_call.id.clone(),
                });
            }
        }

        events.push(LlmEvent::Done {
            finish_reason: response.finish_reason.clone(),
            usage: response.usage.clone(),
        });

        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// Why a model step could not even start streaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStepError {
    /// A recorded ToolResult closure cannot be encoded because its closing
    /// call carries no provider call ID and the provider wire correlates
    /// results by ID (plan decision 9). No ID is ever synthesized.
    ToolResultWithoutProviderCallId { invocation_id: InvocationId },
    /// The provider request failed before any stream event. `retryable` is a
    /// hint from the transport classification; the caller owns retry policy.
    OpeningFailed {
        message: String,
        /// Transport-classified retryability hint.
        retryable: bool,
    },
}

impl std::fmt::Display for ModelStepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolResultWithoutProviderCallId { invocation_id } => write!(
                f,
                "tool result for invocation {invocation_id} cannot be encoded: \
                 the closing call has no provider call id"
            ),
            Self::OpeningFailed { message, .. } => write!(
                f,
                "provider request failed before the stream opened: {message}"
            ),
        }
    }
}

impl std::error::Error for ModelStepError {}

/// Terminal classification of one provider attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStepStatus {
    /// The attempt produced a complete valid message (zero-Tool, or
    /// Tool-bearing and ready to enter binding).
    Complete,
    /// The provider finished but the complete output is invalid.
    Rejected(AttemptRejection),
    /// The attempt was interrupted before a complete valid message.
    Interrupted(AttemptInterruption),
}

/// The result of one model step attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelStepAttempt {
    /// Terminal classification.
    pub status: ModelStepStatus,
    /// The complete valid message, present only for
    /// [`ModelStepStatus::Complete`]. Tool calls are unbound: binding is the
    /// coordinator's job.
    pub message: Option<NativeMessage<UnboundToolCall>>,
    /// Provider-reported usage; `None` is unknown, never zero. Retained on
    /// non-complete attempts when the provider supplied it.
    pub usage: Option<lattice_conversation::TokenUsage>,
    /// The provider's finish reason, when the attempt reached `Done`.
    pub finish_reason: Option<String>,
    /// Bounded diagnostic for non-complete attempts (the provider's error
    /// text). Diagnostics never enter the conversation.
    pub diagnostic: Option<String>,
}

/// Live per-attempt events for best-effort display streaming.
///
/// The authoritative assembly lives in [`ModelStepAttempt`]; these events are
/// a mirror, never a second authority.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStepEvent {
    /// The first provider stream event arrived (the no-retry boundary).
    Opened {
        provider: Option<String>,
        model: String,
    },
    /// A reasoning delta.
    ReasoningDelta { content: String },
    /// A visible assistant text delta.
    TokenDelta { content: String },
    /// A tool call started streaming.
    ToolCallStarted { id: String, name: String },
    /// A tool call arguments delta.
    ToolCallDelta { id: String, arguments_delta: String },
    /// A tool call finished streaming.
    ToolCallEnded { id: String },
}

/// Frozen request data for one model step (ADR 0005 §8: the coordinator
/// freezes the effective configuration; the driver re-decides nothing).
#[derive(Debug, Clone)]
pub struct ModelStepRequest {
    /// Optional system prompt, prepended once.
    pub system_prompt: Option<String>,
    /// Assembled provider-neutral history of completed (bound) messages.
    pub history: Vec<NativeMessage<lattice_conversation::BoundToolCall>>,
    /// Model alias.
    pub model: String,
    /// Optional provider override.
    pub provider: Option<String>,
    /// Frozen tool definitions.
    pub tools: Vec<ToolDefinition>,
    /// Sampling temperature, when configured.
    pub temperature: Option<f64>,
    /// Output token bound, when configured.
    pub max_tokens: Option<u32>,
    /// Cooperative cancellation flag.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Cooperative deadline for this attempt only.
    pub deadline: Option<Instant>,
}

/// The one-attempt model step driver.
pub struct ModelStepDriver {
    llm: Arc<dyn LlmClient>,
}

impl ModelStepDriver {
    /// Build a driver over one injectable LLM client.
    pub fn new(llm: Arc<dyn LlmClient>) -> Self {
        Self { llm }
    }

    /// Execute one provider attempt.
    ///
    /// Fails before streaming only for typed request-assembly errors; every
    /// in-flight outcome is a [`ModelStepAttempt`], never an `Err`.
    pub async fn run(
        &self,
        request: &ModelStepRequest,
        live: Option<&tokio::sync::mpsc::Sender<ModelStepEvent>>,
    ) -> Result<ModelStepAttempt, ModelStepError> {
        let messages = assemble_request_messages(request)?;
        let chat = ChatRequest {
            messages,
            tools: request.tools.clone(),
            target: ModelTarget {
                model: request.model.clone(),
                provider: request.provider.clone(),
            },
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            max_completion_tokens: None,
        };

        let stream = match self.llm.stream_chat(chat).await {
            Ok(stream) => stream,
            Err(err) => {
                let message = err.to_string();
                return Err(ModelStepError::OpeningFailed {
                    message,
                    retryable: classify_retryable(err.as_ref()),
                });
            }
        };

        self.consume(stream, request, live).await
    }

    /// Consume one opened provider stream to its typed outcome.
    async fn consume(
        &self,
        mut stream: lattice_llm::LlmStream,
        request: &ModelStepRequest,
        live: Option<&tokio::sync::mpsc::Sender<ModelStepEvent>>,
    ) -> Result<ModelStepAttempt, ModelStepError> {
        let emit = |event: ModelStepEvent,
                    live: Option<&tokio::sync::mpsc::Sender<ModelStepEvent>>| {
            if let Some(sender) = live {
                let _ = sender.try_send(event);
            }
        };

        let mut assembly = AttemptAssembly::new();
        let mut reasoning = String::new();
        let mut text = String::new();
        let mut raw_calls: Vec<RawToolCall> = Vec::new();
        let mut saw_stream_event = false;
        let mut usage: Option<lattice_conversation::TokenUsage> = None;
        let mut finish_reason: Option<String> = None;

        loop {
            let next = tokio::select! {
                event = stream.next() => event,
                _ = tokio::time::sleep(CANCEL_POLL_INTERVAL), if request.deadline.is_some() || request.cancel.is_some() => {
                    if let Some(flag) = &request.cancel
                        && flag.load(Ordering::Relaxed)
                    {
                        return Ok(interrupted(
                            AttemptInterruption::Cancelled,
                            usage,
                            finish_reason,
                            None,
                        ));
                    }
                    if let Some(deadline) = request.deadline
                        && Instant::now() >= deadline
                    {
                        return Ok(interrupted(
                            AttemptInterruption::Timeout,
                            usage,
                            finish_reason,
                            None,
                        ));
                    }
                    continue;
                }
            };

            let Some(event) = next else {
                // Stream ended without Done: an interruption, never a
                // complete attempt.
                return Ok(interrupted(
                    AttemptInterruption::StreamError,
                    usage,
                    finish_reason,
                    None,
                ));
            };

            // `Done` is never a cancel point: the attempt finalizes before
            // any post-event observation (pre-cut loop parity).
            if !matches!(event, LlmEvent::Done { .. }) {
                if let Some(deadline) = request.deadline
                    && Instant::now() >= deadline
                {
                    return Ok(interrupted(
                        AttemptInterruption::Timeout,
                        usage,
                        finish_reason,
                        None,
                    ));
                }
                if let Some(flag) = &request.cancel
                    && flag.load(Ordering::Relaxed)
                {
                    return Ok(interrupted(
                        AttemptInterruption::Cancelled,
                        usage,
                        finish_reason,
                        None,
                    ));
                }
            }

            if !saw_stream_event {
                saw_stream_event = true;
                emit(
                    ModelStepEvent::Opened {
                        provider: request.provider.clone(),
                        model: request.model.clone(),
                    },
                    live,
                );
            }

            match event {
                LlmEvent::Started => {}
                LlmEvent::Token { content } => {
                    if let Err(interruption) = assembly.push_assistant(&content) {
                        return Ok(interrupted(interruption, usage, finish_reason, None));
                    }
                    text.push_str(&content);
                    emit(ModelStepEvent::TokenDelta { content }, live);
                }
                LlmEvent::Reasoning { content } => {
                    if let Err(interruption) = assembly.push_reasoning(&content) {
                        return Ok(interrupted(interruption, usage, finish_reason, None));
                    }
                    reasoning.push_str(&content);
                    emit(ModelStepEvent::ReasoningDelta { content }, live);
                }
                LlmEvent::ToolCallStart { id, name } => {
                    assembly.begin_tool_call();
                    raw_calls.push(RawToolCall {
                        provider_call_id: Some(id.clone()),
                        name: name.clone(),
                        arguments: String::new(),
                    });
                    emit(ModelStepEvent::ToolCallStarted { id, name }, live);
                }
                LlmEvent::ToolCallDelta {
                    id,
                    arguments_delta,
                } => {
                    if let Err(interruption) = assembly.push_tool_arguments(&arguments_delta) {
                        return Ok(interrupted(interruption, usage, finish_reason, None));
                    }
                    // Wires stream one call's deltas contiguously; match from
                    // the end so repeated ids still land on the newest call.
                    if let Some(call) = raw_calls
                        .iter_mut()
                        .rev()
                        .find(|call| call.provider_call_id.as_deref() == Some(id.as_str()))
                    {
                        call.arguments.push_str(&arguments_delta);
                    }
                    emit(
                        ModelStepEvent::ToolCallDelta {
                            id,
                            arguments_delta,
                        },
                        live,
                    );
                }
                LlmEvent::ToolCallEnd { id } => {
                    emit(ModelStepEvent::ToolCallEnded { id }, live);
                }
                LlmEvent::Done {
                    finish_reason: reason,
                    usage: reported,
                } => {
                    usage = reported.map(|tokens| lattice_conversation::TokenUsage {
                        prompt_tokens: u64::from(tokens.prompt_tokens),
                        completion_tokens: u64::from(tokens.completion_tokens),
                        total_tokens: u64::from(tokens.total_tokens),
                    });
                    finish_reason = Some(reason);
                    break;
                }
                LlmEvent::Error { message } => {
                    return Ok(interrupted(
                        AttemptInterruption::StreamError,
                        usage,
                        finish_reason,
                        Some(message),
                    ));
                }
                LlmEvent::Citation { .. }
                | LlmEvent::Signature { .. }
                | LlmEvent::GroundingChunk { .. }
                | LlmEvent::SafetyRating { .. } => {
                    // Provider metadata events are not conversation parts.
                }
            }
        }

        // Assemble the authoritative ordered message.
        let mut parts: Vec<MessagePart<UnboundToolCall>> = Vec::new();
        if !reasoning.is_empty() {
            parts.push(MessagePart::semantic_only(MessageSemantic::Reasoning {
                text: reasoning,
            }));
        }
        if !text.is_empty() {
            parts.push(MessagePart::semantic_only(MessageSemantic::Text { text }));
        }
        match validate_tool_call_batch(&raw_calls) {
            Ok(validated) => {
                for call in validated {
                    parts.push(MessagePart::semantic_only(MessageSemantic::ToolCall {
                        call,
                    }));
                }
                Ok(ModelStepAttempt {
                    status: ModelStepStatus::Complete,
                    message: Some(NativeMessage::new(
                        MessageRole::Assistant,
                        request.provider.as_deref().map(provider_origin),
                        parts,
                    )),
                    usage,
                    finish_reason,
                    diagnostic: None,
                })
            }
            Err(validation) => Ok(ModelStepAttempt {
                status: ModelStepStatus::Rejected(AttemptRejection::InvalidToolCall { validation }),
                message: None,
                usage,
                finish_reason,
                diagnostic: None,
            }),
        }
    }
}

fn interrupted(
    interruption: AttemptInterruption,
    usage: Option<lattice_conversation::TokenUsage>,
    finish_reason: Option<String>,
    diagnostic: Option<String>,
) -> ModelStepAttempt {
    ModelStepAttempt {
        status: ModelStepStatus::Interrupted(interruption),
        message: None,
        usage,
        finish_reason,
        diagnostic,
    }
}

/// Transport retryability classification for an opening failure. The caller
/// may still choose never to retry; this is a hint, not a policy.
fn classify_retryable(err: &(dyn std::error::Error + Send + Sync + 'static)) -> bool {
    // Structured LLM errors carry their classification; opaque errors are
    // conservatively non-retryable.
    if let Some(llm_error) = err.downcast_ref::<lattice_llm::LlmError>() {
        return llm_error.retryable();
    }
    false
}

/// Map the frozen provider override onto the conversation origin family.
fn provider_origin(provider: &str) -> lattice_conversation::ProviderOrigin {
    match provider {
        "anthropic" => lattice_conversation::ProviderOrigin::Anthropic,
        "gemini" => lattice_conversation::ProviderOrigin::Gemini,
        _ => lattice_conversation::ProviderOrigin::OpenAiCompatible,
    }
}

/// Model-facing content of one closure: available material verbatim, else the
/// honest outcome spelling (same convention as the provider projection).
fn closure_content(outcome: ToolResultOutcome, content: &Option<String>) -> String {
    match content {
        Some(text) => text.clone(),
        None => format!("{{\"outcome\":\"{}\"}}", outcome.as_str()),
    }
}

/// Assemble the provider request messages from the frozen history.
///
/// ToolResult closures join to their closing bound ToolCalls by canonical
/// `InvocationId`; the provider call ID the wire needs comes from that call.
/// A closure whose call carries none fails typed rather than synthesizing an
/// ID (plan decision 9).
fn assemble_request_messages(request: &ModelStepRequest) -> Result<Vec<Message>, ModelStepError> {
    let mut messages = Vec::new();
    if let Some(system) = &request.system_prompt {
        messages.push(Message::system(system.clone()));
    }

    // invocation -> provider call id, from every bound assistant message.
    let mut closing_ids: BTreeMap<InvocationId, Option<String>> = BTreeMap::new();
    for message in &request.history {
        for (_, call) in message.tool_calls() {
            closing_ids.insert(call.invocation_id, call.provider_call_id.clone());
        }
    }

    for message in &request.history {
        match message.role {
            MessageRole::System => {
                let content = flat_text(message);
                messages.push(Message::system(content));
            }
            MessageRole::User => {
                messages.push(Message::user(flat_text(message)));
            }
            MessageRole::Assistant => {
                let mut content = String::new();
                let mut reasoning_content = None;
                let mut tool_calls: Vec<ToolCall> = Vec::new();
                for part in &message.parts {
                    match &part.semantic {
                        MessageSemantic::Text { text } => content.push_str(text),
                        MessageSemantic::Reasoning { text } => {
                            let slot = reasoning_content.get_or_insert_with(String::new);
                            slot.push_str(text);
                        }
                        MessageSemantic::ToolCall { call } => {
                            // A bound call without a provider ID cannot be
                            // replayed to an ID-correlating wire; fail typed.
                            tool_calls.push(ToolCall {
                                id: call.provider_call_id.clone().ok_or(
                                    ModelStepError::ToolResultWithoutProviderCallId {
                                        invocation_id: call.invocation_id,
                                    },
                                )?,
                                function: FunctionCall {
                                    name: call.name.clone(),
                                    arguments: lattice_conversation::canonical_json(
                                        call.arguments(),
                                    ),
                                },
                            });
                        }
                        MessageSemantic::ToolResult { .. } => {}
                    }
                }
                messages.push(Message {
                    role: Role::Assistant,
                    content,
                    reasoning_content,
                    tool_calls: if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    tool_call_id: None,
                    name: None,
                });
            }
            MessageRole::Tool => {
                for part in &message.parts {
                    let MessageSemantic::ToolResult {
                        invocation_id,
                        name,
                        outcome,
                        content,
                    } = &part.semantic
                    else {
                        continue;
                    };
                    let tool_call_id = closing_ids.get(invocation_id).cloned().flatten().ok_or(
                        ModelStepError::ToolResultWithoutProviderCallId {
                            invocation_id: *invocation_id,
                        },
                    )?;
                    messages.push(Message {
                        role: Role::Tool,
                        content: closure_content(*outcome, content),
                        reasoning_content: None,
                        tool_calls: None,
                        tool_call_id: Some(tool_call_id),
                        name: Some(name.clone()),
                    });
                }
            }
        }
    }
    Ok(messages)
}

fn flat_text(message: &NativeMessage<lattice_conversation::BoundToolCall>) -> String {
    let mut text = String::new();
    for part in &message.parts {
        if let MessageSemantic::Text { text: delta } = &part.semantic {
            text.push_str(delta);
        }
    }
    text
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::testing::FakeStreamingLlmClient;
    use lattice_conversation::{
        MAX_ASSISTANT_CONTENT_BYTES, MAX_REASONING_CONTENT_BYTES, MAX_TOOL_CALL_ARGUMENTS_BYTES,
        ToolCallAddress,
    };
    use lattice_llm::LlmEvent;

    fn driver(events: Vec<LlmEvent>) -> ModelStepDriver {
        ModelStepDriver::new(Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(events)
                .build(),
        ))
    }

    fn request() -> ModelStepRequest {
        ModelStepRequest {
            system_prompt: None,
            history: Vec::new(),
            model: "test-model".to_string(),
            provider: None,
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            cancel: None,
            deadline: None,
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn bound_assistant(
        provider_call_id: Option<&str>,
        invocation: InvocationId,
    ) -> NativeMessage<lattice_conversation::BoundToolCall> {
        let address = ToolCallAddress {
            message: lancet_protocol_marker(),
            part_ordinal: 0,
        };
        let call = lattice_conversation::UnboundToolCall {
            provider_call_id: provider_call_id.map(str::to_string),
            name: "observe.workspace".to_string(),
            arguments: serde_json::json!({}),
        }
        .bind(invocation, address);
        NativeMessage::new(
            MessageRole::Assistant,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                call,
            })],
        )
    }

    fn lancet_protocol_marker() -> lattice_conversation::ModelMessageAddress {
        lattice_conversation::ModelMessageAddress {
            session_id: lattice_conversation::SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c")
                .unwrap(),
            turn_id: lattice_conversation::TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c")
                .unwrap(),
            model_step_ordinal: 0,
        }
    }

    fn invocation(index: u64) -> InvocationId {
        InvocationId::parse(&format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
    }

    #[test]
    fn zero_tool_attempt_completes_with_ordered_message() {
        let attempt = runtime()
            .block_on(
                driver(vec![
                    LlmEvent::Reasoning {
                        content: "thinking".to_string(),
                    },
                    LlmEvent::Token {
                        content: "hello".to_string(),
                    },
                    LlmEvent::Done {
                        finish_reason: "stop".to_string(),
                        usage: Some(lattice_llm::TokenUsage {
                            prompt_tokens: 1,
                            completion_tokens: 2,
                            total_tokens: 3,
                        }),
                    },
                ])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(attempt.status, ModelStepStatus::Complete));
        let message = attempt.message.expect("complete message");
        assert_eq!(message.parts.len(), 2);
        assert!(matches!(
            message.parts[0].semantic,
            MessageSemantic::Reasoning { .. }
        ));
        assert!(matches!(
            message.parts[1].semantic,
            MessageSemantic::Text { .. }
        ));
        assert_eq!(attempt.usage.as_ref().map(|u| u.total_tokens), Some(3));
        assert_eq!(attempt.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn tool_bearing_attempt_validates_the_whole_batch() {
        let attempt = runtime()
            .block_on(
                driver(vec![
                    LlmEvent::ToolCallStart {
                        id: "call_1".to_string(),
                        name: "observe.workspace".to_string(),
                    },
                    LlmEvent::ToolCallDelta {
                        id: "call_1".to_string(),
                        arguments_delta: r#"{"path":"a"}"#.to_string(),
                    },
                    LlmEvent::ToolCallEnd {
                        id: "call_1".to_string(),
                    },
                    LlmEvent::Done {
                        finish_reason: "tool_calls".to_string(),
                        usage: None,
                    },
                ])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(attempt.status, ModelStepStatus::Complete));
        let message = attempt.message.expect("complete message");
        let calls: Vec<_> = message.tool_calls().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1.provider_call_id.as_deref(), Some("call_1"));
        // Usage is unknown, not zero.
        assert_eq!(attempt.usage, None);
    }

    #[test]
    fn one_invalid_call_rejects_the_whole_attempt() {
        let attempt = runtime()
            .block_on(
                driver(vec![
                    LlmEvent::ToolCallStart {
                        id: "call_1".to_string(),
                        name: "observe.workspace".to_string(),
                    },
                    LlmEvent::ToolCallDelta {
                        id: "call_1".to_string(),
                        arguments_delta: "not json".to_string(),
                    },
                    LlmEvent::ToolCallEnd {
                        id: "call_1".to_string(),
                    },
                    LlmEvent::Done {
                        finish_reason: "tool_calls".to_string(),
                        usage: None,
                    },
                ])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(
            attempt.status,
            ModelStepStatus::Rejected(AttemptRejection::InvalidToolCall { .. })
        ));
        assert!(attempt.message.is_none());
    }

    #[test]
    fn assistant_content_ceiling_interrupts_on_plus_one() {
        let exact = runtime()
            .block_on(
                driver(vec![
                    LlmEvent::Token {
                        content: "x".repeat(MAX_ASSISTANT_CONTENT_BYTES),
                    },
                    LlmEvent::Done {
                        finish_reason: "stop".to_string(),
                        usage: None,
                    },
                ])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(exact.status, ModelStepStatus::Complete));

        let over = runtime()
            .block_on(
                driver(vec![
                    LlmEvent::Token {
                        content: "x".repeat(MAX_ASSISTANT_CONTENT_BYTES),
                    },
                    LlmEvent::Token {
                        content: "y".to_string(),
                    },
                    LlmEvent::Done {
                        finish_reason: "stop".to_string(),
                        usage: None,
                    },
                ])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(
            over.status,
            ModelStepStatus::Interrupted(AttemptInterruption::OutputLimitExceeded {
                kind: lattice_conversation::LimitKind::AssistantContent,
                ..
            })
        ));
        assert!(over.message.is_none());
    }

    #[test]
    fn reasoning_and_tool_argument_ceilings_interrupt() {
        let over_reasoning = runtime()
            .block_on(
                driver(vec![
                    LlmEvent::Reasoning {
                        content: "r".repeat(MAX_REASONING_CONTENT_BYTES + 1),
                    },
                    LlmEvent::Done {
                        finish_reason: "stop".to_string(),
                        usage: None,
                    },
                ])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(
            over_reasoning.status,
            ModelStepStatus::Interrupted(AttemptInterruption::OutputLimitExceeded { .. })
        ));

        let overhead = r#"{"a":""}"#.len();
        let over_args = runtime()
            .block_on(
                driver(vec![
                    LlmEvent::ToolCallStart {
                        id: "c".to_string(),
                        name: "t".to_string(),
                    },
                    LlmEvent::ToolCallDelta {
                        id: "c".to_string(),
                        arguments_delta: format!(
                            "{{\"a\":\"{}\"}}",
                            "z".repeat(MAX_TOOL_CALL_ARGUMENTS_BYTES - overhead + 1)
                        ),
                    },
                    LlmEvent::Done {
                        finish_reason: "tool_calls".to_string(),
                        usage: None,
                    },
                ])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(
            over_args.status,
            ModelStepStatus::Interrupted(AttemptInterruption::OutputLimitExceeded { .. })
        ));
    }

    #[test]
    fn mid_stream_error_and_missing_done_interrupt() {
        let error = runtime()
            .block_on(
                driver(vec![
                    LlmEvent::Token {
                        content: "par".to_string(),
                    },
                    LlmEvent::Error {
                        message: "boom".to_string(),
                    },
                ])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(
            error.status,
            ModelStepStatus::Interrupted(AttemptInterruption::StreamError)
        ));

        let no_done = runtime()
            .block_on(
                driver(vec![LlmEvent::Token {
                    content: "par".to_string(),
                }])
                .run(&request(), None),
            )
            .expect("attempt runs");
        assert!(matches!(
            no_done.status,
            ModelStepStatus::Interrupted(AttemptInterruption::StreamError)
        ));
    }

    #[test]
    fn cancellation_and_deadline_interrupt_from_the_poll_race() {
        let cancel = Arc::new(AtomicBool::new(true));
        let cancelled = runtime()
            .block_on(
                ModelStepDriver::new(Arc::new(
                    FakeStreamingLlmClient::builder()
                        .push_events(vec![LlmEvent::Started])
                        .build(),
                ))
                .run(
                    &ModelStepRequest {
                        cancel: Some(cancel),
                        ..request()
                    },
                    None,
                ),
            )
            .expect("attempt runs");
        assert!(matches!(
            cancelled.status,
            ModelStepStatus::Interrupted(AttemptInterruption::Cancelled)
        ));

        let past = Instant::now() - Duration::from_millis(1);
        let timed_out = runtime()
            .block_on(
                ModelStepDriver::new(Arc::new(
                    FakeStreamingLlmClient::builder()
                        .push_events(vec![LlmEvent::Started])
                        .build(),
                ))
                .run(
                    &ModelStepRequest {
                        deadline: Some(past),
                        ..request()
                    },
                    None,
                ),
            )
            .expect("attempt runs");
        assert!(matches!(
            timed_out.status,
            ModelStepStatus::Interrupted(AttemptInterruption::Timeout)
        ));
    }

    #[test]
    fn opening_failure_is_typed_with_a_retry_hint() {
        let result = runtime().block_on(
            ModelStepDriver::new(Arc::new(
                FakeStreamingLlmClient::builder()
                    .push_error("connection refused")
                    .build(),
            ))
            .run(&request(), None),
        );
        assert!(matches!(
            result,
            Err(ModelStepError::OpeningFailed {
                retryable: false,
                ..
            })
        ));
    }

    #[test]
    fn history_tool_results_without_provider_ids_fail_typed() {
        let assistant = bound_assistant(None, invocation(0));
        let tool = NativeMessage::new(
            MessageRole::Tool,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::ToolResult {
                invocation_id: invocation(0),
                name: "observe.workspace".to_string(),
                outcome: ToolResultOutcome::Succeeded,
                content: Some("{}".to_string()),
            })],
        );
        let result = runtime().block_on(
            driver(vec![LlmEvent::Done {
                finish_reason: "stop".to_string(),
                usage: None,
            }])
            .run(
                &ModelStepRequest {
                    history: vec![assistant, tool],
                    ..request()
                },
                None,
            ),
        );
        assert!(matches!(
            result,
            Err(ModelStepError::ToolResultWithoutProviderCallId { invocation_id })
                if invocation_id == invocation(0)
        ));
    }

    #[test]
    fn history_with_real_provider_ids_assembles_the_request() {
        let assistant = bound_assistant(Some("call_9"), invocation(0));
        let tool = NativeMessage::new(
            MessageRole::Tool,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::ToolResult {
                invocation_id: invocation(0),
                name: "observe.workspace".to_string(),
                outcome: ToolResultOutcome::Succeeded,
                content: Some("{\"ok\":true}".to_string()),
            })],
        );
        let attempt = runtime()
            .block_on(
                driver(vec![LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                }])
                .run(
                    &ModelStepRequest {
                        history: vec![assistant, tool],
                        ..request()
                    },
                    None,
                ),
            )
            .expect("assembles");
        assert!(matches!(attempt.status, ModelStepStatus::Complete));
    }

    #[test]
    fn live_events_mirror_the_stream() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(16);
        runtime().block_on(async move {
            driver(vec![
                LlmEvent::Started,
                LlmEvent::Token {
                    content: "hi".to_string(),
                },
                LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ])
            .run(&request(), Some(&sender))
            .await
            .expect("attempt runs");
        });
        let mut seen = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            seen.push(event);
        }
        assert!(matches!(seen[0], ModelStepEvent::Opened { .. }));
        assert!(matches!(seen[1], ModelStepEvent::TokenDelta { .. }));
    }
}
