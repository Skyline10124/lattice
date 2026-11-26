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
        images: Arc<dyn lattice_llm::ImageSource>,
        control: StreamControl,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>>;
}

#[async_trait::async_trait]
impl LlmClient for lattice_llm::ResolvedRequest {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        images: Arc<dyn lattice_llm::ImageSource>,
        control: StreamControl,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        Ok(lattice_llm::ResolvedRequest::stream_chat(self, request, images, control).await?)
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
        usage_kind: lattice_conversation::UsageCompleteness,
        cost: Option<lattice_conversation::CostEstimate>,
        finish_reason: Option<String>,
    },
    Stopped {
        cause: ModelStopCause,
        usage: Option<lattice_conversation::TokenUsage>,
        usage_kind: lattice_conversation::UsageCompleteness,
        cost: Option<lattice_conversation::CostEstimate>,
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
        provider: Option<lattice_conversation::ProviderId>,
        model: lattice_conversation::ModelId,
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
    pub model: lattice_conversation::ModelId,
    /// 模型引用指定的 provider 标识。
    pub provider: lattice_conversation::ProviderId,
    pub effort: Option<lattice_conversation::Effort>,
    /// Frozen tool definitions.
    pub tools: Vec<ToolDefinition>,
    /// Cooperative cancellation flag.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Cooperative deadline for this attempt only.
    pub deadline: Option<Instant>,
    /// 本 Turn 所属 SessionId，作为 `x-client-request-id` 发给 provider。
    pub session_id: Option<String>,
}

impl ModelStepRequest {
    /// 由步骤请求得到发给模型的完整请求；准入预检与实际发送共用这一个构造入口。
    pub fn chat_request(&self) -> ChatRequest {
        let mut messages = Vec::with_capacity(self.history.len() + 1);
        if let Some(system) = &self.system_prompt {
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
        messages.extend(self.history.iter().cloned());
        ChatRequest {
            messages,
            tools: self.tools.clone(),
            target: ModelTarget {
                model: lattice_conversation::ModelRef::new(
                    self.provider.clone(),
                    self.model.clone(),
                ),
                effort: self.effort,
            },
            client_request_id: self.session_id.clone(),
        }
    }
}

/// 单次尝试的模型步骤驱动，持有可注入的客户端与图片来源。
pub struct ModelStepDriver {
    llm: Arc<dyn LlmClient>,
    images: Arc<dyn lattice_llm::ImageSource>,
}

impl ModelStepDriver {
    /// 基于可注入的客户端与图片来源构造驱动。
    pub fn new(llm: Arc<dyn LlmClient>, images: Arc<dyn lattice_llm::ImageSource>) -> Self {
        Self { llm, images }
    }

    /// 执行一次provider尝试；打开失败保持类型化错误，流内结果由ModelStepAttempt表达。
    /// 观察回调必须非阻塞。协调器在回调内编号并尽力投递，不能让慢客户端反压模型消费。
    pub async fn run(
        &self,
        request: &ModelStepRequest,
        live: Option<&mut (dyn FnMut(ModelStepEvent) + Send)>,
    ) -> Result<ModelStepAttempt, ModelStepError> {
        let chat = request.chat_request();

        let control = StreamControl {
            cancel: request.cancel.clone(),
            deadline: request.deadline,
        };
        if let Some(reason) = control.stop() {
            return Ok(stopped(
                ReplyStop::Stream(reason),
                None,
                lattice_conversation::UsageCompleteness::Partial,
                None,
            ));
        }

        let opening = self
            .llm
            .stream_chat(chat, self.images.clone(), control.clone());
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
                    if let Some(reason) = control.stop() { return Ok(stopped(ReplyStop::Stream(reason), None, lattice_conversation::UsageCompleteness::Partial, None)); }
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
                            usage_kind: report.usage_kind,
                            cost: report.cost,
                            finish_reason: report.finish_reason,
                        },
                        ReplyOutcome::Rejected {
                            rejection: AttemptRejection::InvalidToolCall { validation },
                        } => ModelStepAttempt::Stopped {
                            cause: ModelStopCause::InvalidBatch,
                            usage: report.usage,
                            usage_kind: report.usage_kind,
                            cost: report.cost,
                            diagnostic: ModelDiagnostic {
                                provider_error: None,
                                invalid_batch: Some(validation),
                            },
                        },
                        ReplyOutcome::Interrupted { reason } => {
                            stopped(reason, report.usage, report.usage_kind, report.cost)
                        }
                    });
                }
            }
        }
        Ok(stopped(
            ReplyStop::Stream(StreamStop::UnexpectedEof),
            None,
            lattice_conversation::UsageCompleteness::Partial,
            None,
        ))
    }
}

fn stopped(
    reason: ReplyStop,
    usage: Option<lattice_conversation::TokenUsage>,
    usage_kind: lattice_conversation::UsageCompleteness,
    cost: Option<lattice_conversation::CostEstimate>,
) -> ModelStepAttempt {
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
        usage_kind,
        cost,
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

#[cfg(test)]
mod opening_error_tests {
    use super::*;

    struct FailingClient(lattice_llm::LlmError);

    #[async_trait::async_trait]
    impl LlmClient for FailingClient {
        async fn stream_chat(
            &self,
            _request: ChatRequest,
            _images: Arc<dyn lattice_llm::ImageSource>,
            _control: StreamControl,
        ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
            Err(Box::new(self.0.clone()))
        }
    }

    #[tokio::test]
    async fn opening_budget_and_413_errors_are_non_retryable_and_preserve_messages() {
        let budget_msg = "编码后的模型请求超过 LATTICE 本地请求体预算（32 MiB），请减少图片，或执行 lattice session context-op 精简历史内容与图片、lattice session rewind 退回较早节点或新建会话";
        let http_413_msg = "HTTP 413 request_too_large: payload exceeds limit；目标服务拒绝了本次请求大小（HTTP 413），请减少图片，或执行 lattice session context-op 精简历史内容与图片、lattice session rewind 退回较早节点或新建会话";

        for (label, llm_error, expected_msg) in [
            (
                "本地请求体预算超限",
                lattice_llm::LlmError::RequestBudgetExceeded {
                    message: budget_msg.into(),
                },
                budget_msg,
            ),
            (
                "远端 HTTP 413 拒绝",
                lattice_llm::LlmError::ProviderHttp {
                    status_code: 413,
                    message: http_413_msg.into(),
                    retryable: false,
                    retry_after: None,
                },
                http_413_msg,
            ),
        ] {
            let driver = ModelStepDriver::new(
                Arc::new(FailingClient(llm_error)),
                Arc::new(lattice_llm::NoImages),
            );
            let step_request = ModelStepRequest {
                effort: None,
                system_prompt: None,
                history: Vec::new(),
                model: lattice_conversation::ModelId::parse("test-model").unwrap(),
                provider: lattice_conversation::ProviderId::parse("test").unwrap(),
                tools: Vec::new(),
                cancel: None,
                deadline: None,
                session_id: Some("session-1".into()),
            };
            let err = driver
                .run(&step_request, None)
                .await
                .expect_err("打开阶段错误必须返回 Err");
            assert_eq!(
                err,
                ModelStepError::OpeningFailed {
                    message: expected_msg.into(),
                    retryable: false,
                    retry_after: None,
                },
                "{label} 应映射为不可重试且消息原样保留的 OpeningFailed"
            );
        }
    }
}

#[cfg(test)]
mod chat_request_tests {
    use super::*;
    use lattice_conversation::{
        ConversationItem, Effort, ModelId, ModelRef, Protocol, ProviderId, ReplayOrigin,
        StepContent,
    };

    fn sample_history(provider: &ProviderId) -> Vec<ConversationItem> {
        vec![
            ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: "第一条用户消息".into(),
                })],
            )),
            ConversationItem::Step(StepContent {
                assistant: NativeMessage::new(
                    MessageRole::Assistant,
                    Some(ReplayOrigin {
                        protocol: Protocol::ChatCompletions,
                        provider_id: provider.clone(),
                    }),
                    vec![MessagePart::semantic_only(MessageSemantic::Text {
                        text: "第一条助手回复".into(),
                    })],
                ),
                results: Vec::new(),
            }),
            ConversationItem::Message(NativeMessage::new(
                MessageRole::User,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: "第二条用户消息".into(),
                })],
            )),
        ]
    }

    fn sample_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::new(
                "read_file",
                "读取文件内容",
                serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            ),
            ToolDefinition::new(
                "search_code",
                "搜索代码内容",
                serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            ),
        ]
    }

    #[test]
    fn chat_request_prepends_system_prompt_once_and_preserves_fields() {
        let provider = ProviderId::parse("openai").expect("解析测试 provider 标识");
        let model = ModelId::parse("gpt-4o").expect("解析测试模型标识");
        let history = sample_history(&provider);
        let tools = sample_tools();
        let request = ModelStepRequest {
            system_prompt: Some("你是严谨的编程助手".into()),
            history: history.clone(),
            model: model.clone(),
            provider: provider.clone(),
            effort: Some(Effort::High),
            tools: tools.clone(),
            cancel: Some(Arc::new(AtomicBool::new(false))),
            deadline: Some(Instant::now()),
            session_id: Some("session-with-system".into()),
        };

        let chat = request.chat_request();
        let system_count = chat
            .messages
            .iter()
            .filter(|item| {
                matches!(
                    item,
                    ConversationItem::Message(message) if message.role == MessageRole::System
                )
            })
            .count();
        assert_eq!(system_count, 1, "系统提示消息必须只出现一次");
        assert_eq!(
            chat.messages.first(),
            Some(&ConversationItem::Message(NativeMessage::new(
                MessageRole::System,
                None,
                vec![MessagePart::semantic_only(MessageSemantic::Text {
                    text: "你是严谨的编程助手".into(),
                })],
            ))),
            "系统提示必须位于消息列表最前"
        );
        assert_eq!(
            &chat.messages[1..],
            history.as_slice(),
            "系统提示之后必须原样保留历史消息顺序"
        );
        assert_eq!(chat.tools, tools, "工具定义必须与输入一致");
        assert_eq!(
            chat.target,
            ModelTarget {
                model: ModelRef::new(provider, model),
                effort: Some(Effort::High),
            },
            "模型目标与推理档位必须与输入一致"
        );
        assert_eq!(
            chat.client_request_id.as_deref(),
            Some("session-with-system"),
            "client_request_id 必须与 session_id 一致"
        );
    }

    #[test]
    fn chat_request_without_system_prompt_preserves_history_and_target() {
        let provider = ProviderId::parse("anthropic").expect("解析测试 provider 标识");
        let model = ModelId::parse("claude-sonnet").expect("解析测试模型标识");
        let history = sample_history(&provider);
        let tools = sample_tools();
        let request = ModelStepRequest {
            system_prompt: None,
            history: history.clone(),
            model: model.clone(),
            provider: provider.clone(),
            effort: None,
            tools: tools.clone(),
            cancel: None,
            deadline: None,
            session_id: None,
        };

        let chat = request.chat_request();
        assert!(
            !chat.messages.iter().any(|item| {
                matches!(
                    item,
                    ConversationItem::Message(message) if message.role == MessageRole::System
                )
            }),
            "无系统提示时不应插入 System 消息"
        );
        assert_eq!(chat.messages, history, "无系统提示时消息必须与历史完全一致");
        assert_eq!(chat.tools, tools, "工具定义必须与输入一致");
        assert_eq!(
            chat.target,
            ModelTarget {
                model: ModelRef::new(provider, model),
                effort: None,
            },
            "无系统提示时模型目标与推理档位必须与输入一致"
        );
        assert_eq!(
            chat.client_request_id, None,
            "未提供 session_id 时 client_request_id 必须为空"
        );
    }
}

#[cfg(all(test, feature = "test-utils"))]
mod tests {
    use super::*;
    use crate::testing::FakeStreamingLlmClient;
    use lattice_conversation::MAX_ASSISTANT_CONTENT_BYTES;
    use lattice_llm::testing::ScriptEvent;

    #[tokio::test]
    async fn effort_is_preserved_in_chat_request() {
        struct Capturing(std::sync::Mutex<Vec<Option<lattice_conversation::Effort>>>);
        #[async_trait::async_trait]
        impl LlmClient for Capturing {
            async fn stream_chat(
                &self,
                request: ChatRequest,
                _images: Arc<dyn lattice_llm::ImageSource>,
                control: StreamControl,
            ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>>
            {
                self.0.lock().unwrap().push(request.target.effort);
                Ok(lattice_llm::testing::script_stream(
                    lattice_conversation::ReplayOrigin {
                        protocol: lattice_conversation::Protocol::ChatCompletions,
                        provider_id: request.target.model.provider().clone(),
                    },
                    vec![],
                    control,
                ))
            }
        }
        let client = Arc::new(Capturing(std::sync::Mutex::new(vec![])));
        let driver = ModelStepDriver::new(client.clone(), Arc::new(lattice_llm::NoImages));
        for effort in [
            None,
            Some(lattice_conversation::Effort::Off),
            Some(lattice_conversation::Effort::High),
        ] {
            let mut input = request();
            input.effort = effort;
            driver.run(&input, None).await.unwrap();
        }
        assert_eq!(
            *client.0.lock().unwrap(),
            [
                None,
                Some(lattice_conversation::Effort::Off),
                Some(lattice_conversation::Effort::High)
            ]
        );
    }

    fn driver(events: Vec<ScriptEvent>) -> ModelStepDriver {
        ModelStepDriver::new(
            Arc::new(
                FakeStreamingLlmClient::builder()
                    .push_events(events)
                    .build(),
            ),
            Arc::new(lattice_llm::NoImages),
        )
    }

    fn request() -> ModelStepRequest {
        ModelStepRequest {
            effort: None,
            system_prompt: None,
            history: Vec::new(),
            model: lattice_conversation::ModelId::parse("test-model").unwrap(),
            provider: lattice_conversation::ProviderId::parse("test").unwrap(),
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
            let driver = ModelStepDriver::new(client.clone(), Arc::new(lattice_llm::NoImages));
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
            ModelStepDriver::new(
                Arc::new(
                    FakeStreamingLlmClient::builder()
                        .push_error("connection refused")
                        .build(),
                ),
                Arc::new(lattice_llm::NoImages),
            )
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

    #[tokio::test]
    async fn driver_forwards_injected_image_source_to_client_on_each_run() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingImageSource(AtomicUsize);
        impl lattice_llm::ImageSource for CountingImageSource {
            fn with_reader(
                &self,
                encode: &mut dyn FnMut(
                    &dyn lattice_llm::ImageReader,
                ) -> Result<(), lattice_llm::LlmError>,
            ) -> Result<(), lattice_llm::LlmError> {
                self.0.fetch_add(1, Ordering::SeqCst);
                lattice_llm::NoImages.with_reader(encode)
            }
        }

        struct VerifyingClient {
            expected: Arc<dyn lattice_llm::ImageSource>,
        }
        #[async_trait::async_trait]
        impl LlmClient for VerifyingClient {
            async fn stream_chat(
                &self,
                request: ChatRequest,
                images: Arc<dyn lattice_llm::ImageSource>,
                control: StreamControl,
            ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>>
            {
                assert!(
                    Arc::ptr_eq(&self.expected, &images),
                    "驱动必须原样传递构造时注入的图片来源实例"
                );
                images.with_reader(&mut |_reader| Ok(()))?;
                Ok(lattice_llm::testing::script_stream(
                    lattice_conversation::ReplayOrigin {
                        protocol: lattice_conversation::Protocol::ChatCompletions,
                        provider_id: request.target.model.provider().clone(),
                    },
                    vec![ScriptEvent::Done {
                        finish_reason: "stop".into(),
                        usage: None,
                    }],
                    control,
                ))
            }
        }

        let counter = Arc::new(CountingImageSource(AtomicUsize::new(0)));
        let expected_dyn: Arc<dyn lattice_llm::ImageSource> = counter.clone();
        let driver = ModelStepDriver::new(
            Arc::new(VerifyingClient {
                expected: expected_dyn.clone(),
            }),
            expected_dyn,
        );
        driver
            .run(&request(), None)
            .await
            .expect("第一次尝试应成功");
        driver
            .run(&request(), None)
            .await
            .expect("第二次尝试应成功");
        assert_eq!(
            counter.0.load(Ordering::SeqCst),
            2,
            "每次调用都应向下游传递同一图片来源"
        );
    }
}
