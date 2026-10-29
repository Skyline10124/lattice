// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 单次模型尝试驱动，职责边界见 ADR 0005 §10。
//!
//! `ModelStepDriver` 每次调用执行一次模型尝试：
//! 组装冻结请求，消费 lattice-llm 共同累积器的实时投影与唯一终态，
//! 返回一次尝试的类型化结果；流式计量和完整消息组装由 lattice-llm 负责。
//!
//! 本模块不负责会话或调用存储、UUID 生成、多步循环、工具执行、重试策略或 Turn 提交。
//! 打开失败通过 [`ModelStepError::OpeningFailed`] 返回；
//! 流内失败同样保留可重试性，由调用方按预算决定。
//!
//! provider 提供的工具调用 ID 原样保留，缺失时不补造。
//! 若对话中的 ToolResult 对应的调用缺少 provider ID，请求组装返回类型化错误。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use futures::StreamExt;
use lattice_conversation::{
    AttemptRejection, MessagePart, MessageRole, MessageSemantic, NativeMessage, UnboundToolCall,
};
use lattice_llm::{
    CONTROL_POLL_INTERVAL, ChatRequest, LiveDelta, LlmEvent, ModelTarget, ReplyOutcome, ReplyStop,
    StreamControl, StreamStop, ToolDefinition,
};
use tokio::time::Instant;

/// 可注入的 LLM 客户端接口，供驱动发起一次模型尝试。
///
/// 生产实现通过 `lattice-llm` 调用真实 provider；
/// 测试实现返回确定性响应，见 [`crate::testing::FakeStreamingLlmClient`]。
/// 实现必须把 `control` 交给 `assemble_reply_stream`；流中的取消与 deadline 由包装器处理。
/// 打开阶段的取消与 deadline 由驱动轮询。
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Execute one chat completion as a provider stream.
    ///
    /// Startup or dispatch failures propagate through `Result::Err`; failures
    /// after opening use in-band typed [`LlmEvent`] values.
    async fn stream_chat(
        &self,
        request: ChatRequest,
        control: StreamControl,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>>;
}

#[async_trait::async_trait]
impl LlmClient for lattice_llm::ResolvedRequest {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        control: StreamControl,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        Ok(lattice_llm::ResolvedRequest::stream_chat(self, request, control).await?)
    }
}

/// Why a model step could not even start streaming.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelStepError {
    /// Provider request projection failed before transport; exact typed cause is
    /// retained so missing IDs and replay-state failures are not flattened.
    Projection(lattice_llm::protocol::projection::ProjectionError),
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
    /// provider 内的模型标识。
    pub model: String,
    /// 模型引用指定的 provider 标识。
    pub provider: String,
    /// Frozen tool definitions.
    pub tools: Vec<ToolDefinition>,
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
            client_request_id: request.session_id.clone(),
        };

        let control = StreamControl {
            cancel: request.cancel.clone(),
            deadline: request.deadline,
        };
        if let Some(reason) = control.stop() {
            return Ok(stopped(ReplyStop::Stream(reason), None));
        }

        let opening = self.llm.stream_chat(chat, control.clone());
        tokio::pin!(opening);
        let stream = loop {
            tokio::select! {
                result = &mut opening => {
                    break match result {
                        Ok(stream) => stream,
                        Err(err) => return Err(model_step_opening_error(err)),
                    };
                }
                _ = tokio::time::sleep(CONTROL_POLL_INTERVAL), if request.deadline.is_some() || request.cancel.is_some() => {
                    if let Some(reason) = control.stop() { return Ok(stopped(ReplyStop::Stream(reason), None)); }
                }
            }
        };

        self.consume(stream, request, live).await
    }

    /// 只转交共同包装器的实时投影与唯一终态。
    async fn consume(
        &self,
        mut stream: lattice_llm::LlmStream,
        request: &ModelStepRequest,
        mut live: Option<&mut (dyn FnMut(ModelStepEvent) + Send)>,
    ) -> Result<ModelStepAttempt, ModelStepError> {
        let mut opened = false;
        while let Some(event) = stream.next().await {
            if !opened {
                opened = true;
                if let Some(observer) = live.as_deref_mut() {
                    observer(ModelStepEvent::Opened {
                        provider: Some(request.provider.clone()),
                        model: request.model.clone(),
                    });
                }
            }
            match event {
                LlmEvent::Started => {}
                LlmEvent::Delta(delta) => {
                    if let Some(observer) = live.as_deref_mut() {
                        observer(match delta {
                            LiveDelta::Text(content) => ModelStepEvent::TokenDelta { content },
                            LiveDelta::Reasoning(content) => {
                                ModelStepEvent::ReasoningDelta { content }
                            }
                        });
                    }
                }
                LlmEvent::Finished { outcome, report } => {
                    return Ok(match outcome {
                        ReplyOutcome::Complete { message } => ModelStepAttempt::Complete {
                            message: message.into_message(),
                            usage: report.usage,
                            finish_reason: report.finish_reason,
                        },
                        ReplyOutcome::Rejected {
                            rejection: AttemptRejection::InvalidToolCall { validation },
                        } => ModelStepAttempt::Stopped {
                            cause: ModelStopCause::InvalidBatch,
                            usage: report.usage,
                            diagnostic: ModelDiagnostic {
                                provider_error: None,
                                invalid_batch: Some(validation),
                            },
                        },
                        ReplyOutcome::Interrupted { reason } => stopped(reason, report.usage),
                    });
                }
            }
        }
        Ok(stopped(ReplyStop::Stream(StreamStop::UnexpectedEof), None))
    }
}

fn stopped(reason: ReplyStop, usage: Option<lattice_conversation::TokenUsage>) -> ModelStepAttempt {
    let (cause, message) = match reason {
        ReplyStop::OutputLimit(_) => (ModelStopCause::OutputLimit, None),
        ReplyStop::Stream(StreamStop::Cancelled) => (ModelStopCause::Cancelled, None),
        ReplyStop::Stream(StreamStop::Deadline) => (ModelStopCause::Deadline, None),
        ReplyStop::Stream(StreamStop::Provider(failure)) => (
            ModelStopCause::ProviderError {
                retryable: failure.retryable,
            },
            Some(failure.message),
        ),
        ReplyStop::Stream(StreamStop::UnexpectedEof) => (
            ModelStopCause::ProviderError { retryable: true },
            Some("provider stream ended before completion".into()),
        ),
        ReplyStop::Stream(StreamStop::Protocol(error)) => (
            ModelStopCause::ProviderError { retryable: false },
            Some(error.to_string()),
        ),
    };
    ModelStepAttempt::Stopped {
        cause,
        usage,
        diagnostic: ModelDiagnostic {
            provider_error: message.map(|message| lattice_llm::bound_diagnostic(&message)),
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
    use lattice_conversation::{InvocationId, MAX_ASSISTANT_CONTENT_BYTES, ToolCallAddress};
    use lattice_llm::testing::ScriptEvent;

    struct ProjectionCheckingClient;

    #[async_trait::async_trait]
    impl LlmClient for ProjectionCheckingClient {
        async fn stream_chat(
            &self,
            request: ChatRequest,
            control: StreamControl,
        ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
            let origin = lattice_conversation::ReplayOrigin {
                protocol: lattice_conversation::Protocol::ChatCompletions,
                provider_id: request.target.provider.clone(),
            };
            lattice_llm::protocol::projection::request_messages_to_wire(&origin, &request.messages)
                .map_err(|error| {
                    Box::new(lattice_llm::LlmError::Projection(error))
                        as Box<dyn std::error::Error + Send + Sync>
                })?;
            Ok(lattice_llm::testing::script_stream(origin, vec![], control))
        }
    }
    fn driver(events: Vec<ScriptEvent>) -> ModelStepDriver {
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
            provider: "test".into(),
            tools: Vec::new(),
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
                vec![ScriptEvent::Done {
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
                vec![ScriptEvent::Done {
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
                vec![],
                Some(true),
                Deadline::None,
                ModelStopCause::Cancelled,
                None,
                false,
            ),
            (
                "past_deadline_only",
                vec![],
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
                        ScriptEvent::ToolMetadata {
                            index: 0,
                            provider_call_id: Some("call_1".to_string()),
                            name: "observe__workspace".to_string(),
                        },
                        ScriptEvent::ToolArguments {
                            index: 0,
                            arguments_delta: arguments_delta.to_string(),
                        },
                        ScriptEvent::ToolEnd { index: 0 },
                        ScriptEvent::Done {
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
    fn stream_stops_map_output_limit_provider_failure_and_eof() {
        for (name, events, expected_cause) in [
            (
                "assistant_limit_then_one_byte",
                vec![
                    ScriptEvent::Text {
                        content: "x".repeat(MAX_ASSISTANT_CONTENT_BYTES),
                    },
                    ScriptEvent::Text {
                        content: "y".to_string(),
                    },
                    ScriptEvent::Done {
                        finish_reason: "stop".to_string(),
                        usage: None,
                    },
                ],
                ModelStopCause::OutputLimit,
            ),
            (
                "provider_error_after_partial",
                vec![
                    ScriptEvent::Text {
                        content: "par".to_string(),
                    },
                    ScriptEvent::Failure {
                        failure: lattice_llm::ProviderFailure {
                            message: "boom".into(),
                            retryable: false,
                        },
                    },
                ],
                ModelStopCause::ProviderError { retryable: false },
            ),
            (
                "stream_ends_without_done",
                vec![ScriptEvent::Text {
                    content: "par".to_string(),
                }],
                ModelStopCause::ProviderError { retryable: true },
            ),
        ] {
            let attempt = runtime()
                .block_on(driver(events).run(&request(), None))
                .unwrap_or_else(|error| panic!("{name}: {error:?}"));
            assert!(
                matches!(attempt, ModelStepAttempt::Stopped { cause, .. } if cause == expected_cause),
                "{name}: expected {expected_cause:?}"
            );
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
                lattice_llm::protocol::projection::ProjectionError::ToolResultWithoutProviderCallId {
                    invocation_id,
                    ..
                },
            )) if invocation_id == invocation(0)
        ));
    }

    #[test]
    fn live_events_mirror_the_stream() {
        let mut seen = Vec::new();
        let mut events = vec![];
        events.extend((0..1600).map(|_| ScriptEvent::Text {
            content: "hi".into(),
        }));
        events.push(ScriptEvent::Done {
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
