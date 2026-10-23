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
//! [`ModelStepError::OpeningFailed`]；流内失败同样保留可重试性，由调用方按预算决定。
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
        /// provider 通过 Retry-After 给出的建议等待；是否采纳由调用方的重试策略决定。
        retry_after: Option<Duration>,
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

/// 单次尝试的停止原因；是否重试由协调层及预算决定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStopCause {
    ProviderError { retryable: bool },
    Cancelled,
    Deadline,
    OutputLimit,
    InvalidBatch,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelDiagnostic {
    pub provider_error: Option<String>,
    pub invalid_batch: Option<lattice_conversation::ToolCallValidation>,
}

/// 完整回复必有消息；停止尝试的诊断绝不进入对话历史。
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStepAttempt {
    Complete {
        message: NativeMessage<UnboundToolCall>,
        usage: Option<lattice_conversation::TokenUsage>,
        finish_reason: Option<String>,
    },
    Stopped {
        cause: ModelStopCause,
        usage: Option<lattice_conversation::TokenUsage>,
        diagnostic: ModelDiagnostic,
    },
}

/// Live per-attempt events for best-effort display streaming.
///
/// The authoritative assembly lives in [`ModelStepAttempt`]; these events are
/// a mirror, never a second authority.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelStepEvent {
    /// 收到首个 provider 事件；不改变错误的重试属性。
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
    pub history: Vec<lattice_conversation::ConversationItem>,
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
    /// 本 Turn 所属 SessionId，作为 `x-client-request-id` 发给 provider。
    pub session_id: Option<String>,
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
            messages.push(lattice_conversation::ConversationItem::Message(
                NativeMessage::new(
                    MessageRole::System,
                    None,
                    vec![MessagePart::semantic_only(MessageSemantic::Text {
                        text: system.clone(),
                    })],
                ),
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
            client_request_id: request.session_id.clone(),
        };

        // 预构造请求及任务调度也可能耗尽预算，首次打开不能依赖后续轮询补救。
        if request
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Ok(interrupted(
                AttemptInterruption::Cancelled,
                None,
                None,
                None,
            ));
        }
        if request
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Ok(interrupted(AttemptInterruption::Timeout, None, None, None));
        }

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
                // 流在结束事件之前中断，通常是连接被切断；已收到多少内容不影响重试资格。
                return Ok(ModelStepAttempt::Stopped {
                    cause: ModelStopCause::ProviderError { retryable: true },
                    usage,
                    diagnostic: ModelDiagnostic {
                        provider_error: Some("provider stream ended before completion".into()),
                        invalid_batch: None,
                    },
                });
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
                LlmEvent::Error { failure } => {
                    return Ok(ModelStepAttempt::Stopped {
                        cause: ModelStopCause::ProviderError {
                            retryable: failure.retryable,
                        },
                        usage,
                        diagnostic: ModelDiagnostic {
                            provider_error: Some(lattice_llm::bound_diagnostic(&failure.message)),
                            invalid_batch: None,
                        },
                    });
                }
                LlmEvent::OutputLimit { interruption } => {
                    return Ok(interrupted(interruption, usage, finish_reason, None));
                }
                LlmEvent::Citation { .. }
                | LlmEvent::Signature { .. }
                | LlmEvent::GroundingChunk { .. }
                | LlmEvent::SafetyRating { .. } => {}
            }
        }

        if let Some(AttemptRejection::InvalidToolCall { validation }) = rejection {
            return Ok(ModelStepAttempt::Stopped {
                cause: ModelStopCause::InvalidBatch,
                usage,
                diagnostic: ModelDiagnostic {
                    provider_error: None,
                    invalid_batch: Some(validation),
                },
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
        Ok(ModelStepAttempt::Complete {
            message,
            usage,
            finish_reason,
        })
    }
}

fn interrupted(
    interruption: AttemptInterruption,
    usage: Option<lattice_conversation::TokenUsage>,
    _finish_reason: Option<String>,
    diagnostic: Option<String>,
) -> ModelStepAttempt {
    let cause = match interruption {
        AttemptInterruption::Cancelled => ModelStopCause::Cancelled,
        AttemptInterruption::Timeout => ModelStopCause::Deadline,
        AttemptInterruption::OutputLimitExceeded { .. } => ModelStopCause::OutputLimit,
        AttemptInterruption::StreamError => ModelStopCause::ProviderError { retryable: false },
    };
    let provider_error = matches!(cause, ModelStopCause::ProviderError { .. }).then(|| {
        lattice_llm::bound_diagnostic(
            &diagnostic.unwrap_or_else(|| "provider stream ended without completion".into()),
        )
    });
    ModelStepAttempt::Stopped {
        cause,
        usage,
        diagnostic: ModelDiagnostic {
            provider_error,
            invalid_batch: None,
        },
    }
}

fn model_step_opening_error(error: Box<dyn std::error::Error + Send + Sync>) -> ModelStepError {
    if let Some(lattice_llm::LlmError::Projection(projection)) =
        error.downcast_ref::<lattice_llm::LlmError>()
    {
        return ModelStepError::Projection(projection.clone());
    }
    // Structured LLM errors carry their classification; opaque errors are
    // conservatively non-retryable.
    let llm_error = error.downcast_ref::<lattice_llm::LlmError>();
    ModelStepError::OpeningFailed {
        message: lattice_llm::bound_diagnostic(&error.to_string()),
        retryable: llm_error.is_some_and(lattice_llm::LlmError::retryable),
        retry_after: llm_error.and_then(lattice_llm::LlmError::retry_after),
    }
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::testing::FakeStreamingLlmClient;
    use lattice_conversation::{
        InvocationId, MAX_ASSISTANT_CONTENT_BYTES, MAX_REASONING_CONTENT_BYTES,
        MAX_TOOL_CALL_ARGUMENTS_BYTES, ToolCallAddress,
    };
    use lattice_llm::LlmEvent;

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
            session_id: None,
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
            name: "observe__workspace".to_string(),
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

    #[tokio::test]
    async fn stopped_requests_respect_cancellation_and_deadline() {
        enum Deadline {
            None,
            Now,
            Past,
        }
        for (
            name,
            events,
            cancel,
            deadline,
            expected_cause,
            expected_call_count,
            check_usage_none,
        ) in [
            (
                "deadline_with_uncancelled_flag",
                vec![LlmEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                }],
                Some(false),
                Deadline::Now,
                ModelStopCause::Deadline,
                Some(0),
                true,
            ),
            (
                "cancel_takes_priority_over_expired_deadline",
                vec![LlmEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                }],
                Some(true),
                Deadline::Now,
                ModelStopCause::Cancelled,
                Some(0),
                true,
            ),
            (
                "cancel_only",
                vec![LlmEvent::Started],
                Some(true),
                Deadline::None,
                ModelStopCause::Cancelled,
                None,
                false,
            ),
            (
                "past_deadline_only",
                vec![LlmEvent::Started],
                None,
                Deadline::Past,
                ModelStopCause::Deadline,
                None,
                false,
            ),
        ] {
            let client = Arc::new(
                FakeStreamingLlmClient::builder()
                    .push_events(events)
                    .build(),
            );
            let driver = ModelStepDriver::new(client.clone());
            let mut request = request();
            request.cancel = cancel.map(|cancelled| Arc::new(AtomicBool::new(cancelled)));
            request.deadline = match deadline {
                Deadline::None => None,
                Deadline::Now => Some(Instant::now()),
                Deadline::Past => Some(Instant::now() - Duration::from_millis(1)),
            };
            let result = driver
                .run(&request, None)
                .await
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            if let Some(count) = expected_call_count {
                assert_eq!(
                    client.call_count(),
                    count,
                    "{name}: stopped request must not open provider"
                );
            }
            assert!(
                matches!(&result, ModelStepAttempt::Stopped { cause, .. } if cause == &expected_cause),
                "{name}: expected {expected_cause:?}"
            );
            if check_usage_none {
                assert!(
                    matches!(result, ModelStepAttempt::Stopped { usage: None, .. }),
                    "{name}: usage must be unknown"
                );
            }
        }
    }

    #[test]
    fn tool_bearing_attempt_validates_the_whole_batch() {
        enum Expected {
            CompleteToolCall,
            InvalidBatch,
        }
        for (name, arguments_delta, expected) in [
            ("valid_json", r#"{"path":"a"}"#, Expected::CompleteToolCall),
            ("invalid_json", "not json", Expected::InvalidBatch),
        ] {
            let attempt = runtime()
                .block_on(
                    driver(vec![
                        LlmEvent::ToolCallStart {
                            index: 0,
                            provider_call_id: Some("call_1".to_string()),
                            name: "observe__workspace".to_string(),
                        },
                        LlmEvent::ToolCallDelta {
                            index: 0,
                            arguments_delta: arguments_delta.to_string(),
                        },
                        LlmEvent::ToolCallEnd { index: 0 },
                        LlmEvent::Done {
                            finish_reason: "tool_calls".to_string(),
                            usage: None,
                        },
                    ])
                    .run(&request(), None),
                )
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            match expected {
                Expected::CompleteToolCall => {
                    let ModelStepAttempt::Complete { message, usage, .. } = attempt else {
                        panic!("{name}: complete message");
                    };
                    let calls: Vec<_> = message.tool_calls().collect();
                    assert_eq!(calls.len(), 1, "{name}");
                    assert_eq!(
                        calls[0].1.provider_call_id.as_deref(),
                        Some("call_1"),
                        "{name}"
                    );
                    // Usage is unknown, not zero.
                    assert_eq!(usage, None, "{name}");
                }
                Expected::InvalidBatch => assert!(
                    matches!(
                        attempt,
                        ModelStepAttempt::Stopped {
                            cause: ModelStopCause::InvalidBatch,
                            diagnostic: ModelDiagnostic {
                                invalid_batch: Some(_),
                                ..
                            },
                            ..
                        }
                    ),
                    "{name}: expected invalid batch"
                ),
            }
        }
    }

    #[test]
    fn stream_completion_respects_limits_and_provider_failures() {
        let overhead = r#"{"a":""}"#.len();
        for (name, events, expected_cause) in [
            (
                "assistant_exact_limit",
                vec![
                    LlmEvent::Token {
                        content: "x".repeat(MAX_ASSISTANT_CONTENT_BYTES),
                    },
                    LlmEvent::Done {
                        finish_reason: "stop".to_string(),
                        usage: None,
                    },
                ],
                None,
            ),
            (
                "assistant_limit_then_one_byte",
                vec![
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
                ],
                Some(ModelStopCause::OutputLimit),
            ),
            (
                "reasoning_limit_plus_one",
                vec![
                    LlmEvent::Reasoning {
                        content: "r".repeat(MAX_REASONING_CONTENT_BYTES + 1),
                    },
                    LlmEvent::Done {
                        finish_reason: "stop".to_string(),
                        usage: None,
                    },
                ],
                Some(ModelStopCause::OutputLimit),
            ),
            (
                "tool_arguments_limit_plus_one",
                vec![
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
                ],
                Some(ModelStopCause::OutputLimit),
            ),
            (
                "provider_error_after_partial",
                vec![
                    LlmEvent::Token {
                        content: "par".to_string(),
                    },
                    LlmEvent::Error {
                        failure: lattice_llm::ProviderFailure {
                            message: "boom".into(),
                            retryable: false,
                        },
                    },
                ],
                Some(ModelStopCause::ProviderError { retryable: false }),
            ),
            (
                "stream_ends_without_done",
                vec![LlmEvent::Token {
                    content: "par".to_string(),
                }],
                Some(ModelStopCause::ProviderError { retryable: true }),
            ),
        ] {
            let attempt = runtime()
                .block_on(driver(events).run(&request(), None))
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            match expected_cause {
                None => assert!(
                    matches!(attempt, ModelStepAttempt::Complete { .. }),
                    "{name}: expected completion"
                ),
                Some(expected) => assert!(
                    matches!(attempt, ModelStepAttempt::Stopped { cause, .. } if cause == expected),
                    "{name}: expected {expected:?}"
                ),
            }
        }
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
        let tool = lattice_conversation::StoredToolResult {
            invocation_id: invocation(0),
            name: "observe__workspace".into(),
            payload: lattice_conversation::ResultPayload::Succeeded(
                lattice_conversation::SuccessContent::Json {
                    value: serde_json::json!({}),
                    digest: lattice_conversation::tool_content_digest("{}"),
                    byte_length: 2,
                },
            ),
        };
        let result = runtime().block_on(
            ModelStepDriver::new(Arc::new(ProjectionCheckingClient)).run(
                &ModelStepRequest {
                    history: vec![lattice_conversation::ConversationItem::Step(
                        lattice_conversation::StepContent {
                            assistant,
                            results: vec![tool],
                        },
                    )],
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
