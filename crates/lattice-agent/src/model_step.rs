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
    AttemptAssembly, AttemptInterruption, AttemptRejection, MessagePart, MessageRole,
    MessageSemantic, NativeMessage, UnboundToolCall,
};
use lattice_llm::{ChatRequest, LlmEvent, ModelTarget, ToolDefinition};
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
    /// Execute one chat completion as a provider stream.
    ///
    /// Startup or dispatch failures propagate through `Result::Err`; failures
    /// after opening use in-band typed [`LlmEvent`] values.
    async fn stream_chat(
        &self,
        request: ChatRequest,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>>;
}

/// Why a model step could not even start streaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStepError {
    /// Provider request projection failed before transport; exact typed cause is
    /// retained so missing IDs and replay-state failures are not flattened.
    Projection(lattice_llm::provider_projection::ProjectionError),
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
            Self::Projection(error) => write!(f, "provider request projection failed: {error}"),
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

    /// 执行一次provider尝试；打开失败保持类型化错误，流内结果由ModelStepAttempt表达。
    /// 观察回调必须非阻塞。协调器在回调内编号并尽力投递，不能让慢客户端反压模型消费。
    pub async fn run(
        &self,
        request: &ModelStepRequest,
        live: Option<&mut (dyn FnMut(ModelStepEvent) + Send)>,
    ) -> Result<ModelStepAttempt, ModelStepError> {
        let mut messages = Vec::with_capacity(request.history.len() + 1);
        if let Some(system) = &request.system_prompt {
            messages.push(NativeMessage::<lattice_conversation::BoundToolCall>::new(
                MessageRole::System,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: system.clone(),
                })],
            ));
        }
        messages.extend(request.history.iter().cloned());
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

        let opening = self.llm.stream_chat(chat);
        tokio::pin!(opening);
        let stream = loop {
            tokio::select! {
                result = &mut opening => {
                    break match result {
                        Ok(stream) => stream,
                        Err(err) => return Err(model_step_opening_error(err)),
                    };
                }
                _ = tokio::time::sleep(CANCEL_POLL_INTERVAL), if request.deadline.is_some() || request.cancel.is_some() => {
                    if let Some(flag) = &request.cancel
                        && flag.load(Ordering::Relaxed)
                    {
                        return Ok(interrupted(
                            AttemptInterruption::Cancelled,
                            None,
                            None,
                            None,
                        ));
                    }
                    if let Some(deadline) = request.deadline
                        && Instant::now() >= deadline
                    {
                        return Ok(interrupted(
                            AttemptInterruption::Timeout,
                            None,
                            None,
                            None,
                        ));
                    }
                }
            }
        };

        self.consume(stream, request, live).await
    }

    /// Consume one opened provider stream to its typed outcome.
    async fn consume(
        &self,
        mut stream: lattice_llm::LlmStream,
        request: &ModelStepRequest,
        mut live: Option<&mut (dyn FnMut(ModelStepEvent) + Send)>,
    ) -> Result<ModelStepAttempt, ModelStepError> {
        // 同步交给协调器的非阻塞观察入口，避免在它编号之前无声丢弃增量。
        let mut emit = |event: ModelStepEvent| {
            if let Some(observer) = live.as_deref_mut() {
                observer(event);
            }
        };

        let mut assembly = AttemptAssembly::new();
        let mut tool_arguments = BTreeMap::<usize, AttemptAssembly>::new();
        let mut complete_message: Option<NativeMessage<UnboundToolCall>> = None;
        let mut rejection: Option<AttemptRejection> = None;
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
                emit(ModelStepEvent::Opened {
                    provider: request.provider.clone(),
                    model: request.model.clone(),
                });
            }

            match event {
                LlmEvent::Started => {}
                LlmEvent::Token { content } => {
                    if let Err(interruption) = assembly.push_assistant(&content) {
                        return Ok(interrupted(interruption, usage, finish_reason, None));
                    }
                    emit(ModelStepEvent::TokenDelta { content });
                }
                LlmEvent::Reasoning { content } => {
                    if let Err(interruption) = assembly.push_reasoning(&content) {
                        return Ok(interrupted(interruption, usage, finish_reason, None));
                    }
                    emit(ModelStepEvent::ReasoningDelta { content });
                }
                LlmEvent::ToolCallStart { .. } => {}
                LlmEvent::ToolCallDelta {
                    index,
                    arguments_delta,
                } => {
                    // Deltas may interleave or precede metadata; Start must not
                    // reset an index's allowance. Keep the attempt total shared.
                    if let Err(interruption) = tool_arguments
                        .entry(index)
                        .or_default()
                        .push_tool_arguments(&arguments_delta)
                    {
                        return Ok(interrupted(interruption, usage, finish_reason, None));
                    }
                    // The per-index accountant checked the call limit above.
                    // Reset only this fragment counter, not the shared total.
                    assembly.begin_tool_call();
                    if let Err(interruption) = assembly.push_tool_arguments(&arguments_delta) {
                        return Ok(interrupted(interruption, usage, finish_reason, None));
                    }
                }
                LlmEvent::ToolCallEnd { .. } => {}
                LlmEvent::MessageComplete { message } => {
                    if complete_message.replace(message).is_some() {
                        return Ok(interrupted(
                            AttemptInterruption::StreamError,
                            usage,
                            finish_reason,
                            Some("provider emitted more than one complete message".to_string()),
                        ));
                    }
                }
                LlmEvent::MessageRejected {
                    rejection: rejected,
                } => {
                    if rejection.replace(rejected).is_some() {
                        return Ok(interrupted(
                            AttemptInterruption::StreamError,
                            usage,
                            finish_reason,
                            Some("provider emitted more than one rejection".to_string()),
                        ));
                    }
                }
                LlmEvent::Done {
                    finish_reason: reason,
                    usage: reported,
                } => {
                    usage = reported;
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
                | LlmEvent::SafetyRating { .. } => {}
            }
        }

        if let Some(rejection) = rejection {
            return Ok(ModelStepAttempt {
                status: ModelStepStatus::Rejected(rejection),
                message: None,
                usage,
                finish_reason,
                diagnostic: None,
            });
        }

        let Some(message) = complete_message else {
            return Ok(interrupted(
                AttemptInterruption::StreamError,
                usage,
                finish_reason,
                Some("provider completed without an authoritative message".to_string()),
            ));
        };
        Ok(ModelStepAttempt {
            status: ModelStepStatus::Complete,
            message: Some(message),
            usage,
            finish_reason,
            diagnostic: None,
        })
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

fn model_step_opening_error(error: Box<dyn std::error::Error + Send + Sync>) -> ModelStepError {
    if let Some(lattice_llm::LlmError::Projection(projection)) =
        error.downcast_ref::<lattice_llm::LlmError>()
    {
        return ModelStepError::Projection(projection.clone());
    }
    ModelStepError::OpeningFailed {
        message: error.to_string(),
        retryable: classify_retryable(error.as_ref()),
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

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::testing::FakeStreamingLlmClient;
    use lattice_conversation::{
        InvocationId, MAX_ASSISTANT_CONTENT_BYTES, MAX_REASONING_CONTENT_BYTES,
        MAX_TOOL_CALL_ARGUMENTS_BYTES, ToolCallAddress, ToolResultOutcome,
    };
    use lattice_llm::LlmEvent;

    struct NeverOpeningClient;

    #[async_trait::async_trait]
    impl LlmClient for NeverOpeningClient {
        async fn stream_chat(
            &self,
            _request: ChatRequest,
        ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
            std::future::pending().await
        }
    }

    struct ProjectionCheckingClient;

    #[async_trait::async_trait]
    impl LlmClient for ProjectionCheckingClient {
        async fn stream_chat(
            &self,
            request: ChatRequest,
        ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
            lattice_llm::provider_projection::request_messages_to_wire(
                lattice_conversation::ProviderOrigin::OpenAiCompatible,
                &request.messages,
            )
            .map_err(|error| {
                Box::new(lattice_llm::LlmError::Projection(error))
                    as Box<dyn std::error::Error + Send + Sync>
            })?;
            Ok(Box::pin(futures::stream::empty()))
        }
    }
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
                        index: 0,
                        provider_call_id: Some("call_1".to_string()),
                        name: "observe.workspace".to_string(),
                    },
                    LlmEvent::ToolCallDelta {
                        index: 0,
                        arguments_delta: r#"{"path":"a"}"#.to_string(),
                    },
                    LlmEvent::ToolCallEnd { index: 0 },
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
                        index: 0,
                        provider_call_id: Some("call_1".to_string()),
                        name: "observe.workspace".to_string(),
                    },
                    LlmEvent::ToolCallDelta {
                        index: 0,
                        arguments_delta: "not json".to_string(),
                    },
                    LlmEvent::ToolCallEnd { index: 0 },
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
                        index: 0,
                        provider_call_id: Some("c".to_string()),
                        name: "t".to_string(),
                    },
                    LlmEvent::ToolCallDelta {
                        index: 0,
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
    fn deadline_interrupts_while_the_provider_stream_is_still_opening() {
        let attempt = runtime()
            .block_on(ModelStepDriver::new(Arc::new(NeverOpeningClient)).run(
                &ModelStepRequest {
                    deadline: Some(Instant::now() + Duration::from_millis(100)),
                    ..request()
                },
                None,
            ))
            .expect("attempt runs");
        assert!(matches!(
            attempt.status,
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
            ModelStepDriver::new(Arc::new(ProjectionCheckingClient)).run(
                &ModelStepRequest {
                    history: vec![assistant, tool],
                    ..request()
                },
                None,
            ),
        );
        assert!(matches!(
            result,
            Err(ModelStepError::Projection(
                lattice_llm::provider_projection::ProjectionError::ToolResultWithoutProviderCallId {
                    invocation_id,
                    ..
                },
            )) if invocation_id == invocation(0)
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
        let mut seen = Vec::new();
        let mut events = vec![LlmEvent::Started];
        events.extend((0..1600).map(|_| LlmEvent::Token {
            content: "hi".into(),
        }));
        events.push(LlmEvent::Done {
            finish_reason: "stop".into(),
            usage: None,
        });
        runtime().block_on(async {
            driver(events)
                .run(&request(), Some(&mut |event| seen.push(event)))
                .await
                .expect("attempt runs");
        });
        assert!(matches!(seen[0], ModelStepEvent::Opened { .. }));
        assert_eq!(seen.len(), 1601);
        assert!(seen[1..].iter().all(
            |event| matches!(event, ModelStepEvent::TokenDelta { content } if content == "hi")
        ));
    }
}
