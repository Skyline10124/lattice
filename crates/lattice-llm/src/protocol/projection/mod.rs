// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 模型消息的回放与投影。
//!
//! 纯数据函数负责 provider 官方 JSON 格式与中立 [`NativeMessage`] 之间的转换。
//! 生产适配器在此投影请求消息；`wire_to_native` 只用于完整 wire 夹具的解码，
//! 并复用共同回复累积器。本模块负责以下边界：
//!
//! 1. 校验并保留 provider 原生助手消息中的调用 ID、签名与思考状态，
//!    将它们按部件保存为不透明回放状态；
//! 2. 完整来源相同时按协议与 features 回放；绑定前后的消息共用相同规则；
//! 3. 来源不同时去除 reasoning 与逐片段回放状态，协议不同时再去除 call ID；
//! 4. 切回原来源时使用历史中保留的原消息；
//! 5. 将对话中的 ToolResult 与其对应的绑定 ToolCall 关联，编码为下一次请求的格式，
//!    并从官方响应正文提取 provider 报告的用量。
//!
//! provider 工具调用 ID 不生成、不编码、不改写，也不补造目标 provider 的身份。
//! 缺少 provider 调用 ID 是有效的对话事实；若目标 provider 无法据此编码已记录的结果，
//! 投影返回类型化的 [`ProjectionError::ToolResultWithoutProviderCallId`] 错误。

mod anthropic_messages;
mod chat_completions;
mod generate_content;

use crate::reply::{PartDelta, ReplyAccumulator, ReplyOutcome, ReplyStop, StreamStop, Update};

use std::collections::BTreeSet;

use crate::protocol::ReasoningReplay;
use lattice_conversation::{
    AttemptInterruption, BoundToolCall, InvocationId, MessageRole, MessageSemantic, NativeMessage,
    Protocol, ReplayOrigin, TokenUsage, ToolCallState, ToolCallValidation, ToolResultOutcome,
    UnboundToolCall,
};
use lattice_conversation::{
    ConversationItem, MAX_TOOL_CONTENT_BYTES, ResultPayload, StoredToolResult, SuccessContent,
    canonical_json, tool_content_digest,
};
use serde_json::Value;
use thiserror::Error;

/// Why a projection failed. Every failure is fail-closed: no partial
/// message, no fabricated identity, no guessed replay state.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProjectionError {
    #[error("protocol {protocol} is not implemented")]
    UnsupportedProtocol { protocol: Protocol },
    #[error("{0}")]
    Protocol(crate::ReplyProtocolError),
    #[error("invalid structured step: {detail}")]
    InvalidStep { detail: &'static str },
    /// The wire JSON does not match the provider's official shape.
    #[error("malformed {provider} wire: {detail}")]
    MalformedWire {
        /// The provider being projected.
        provider: Protocol,
        /// What was missing or mistyped.
        detail: String,
    },
    /// A same-provider replay was requested for a message from another (or
    /// no) origin.
    #[error("cannot replay {expected:?} wire from a message with origin {found:?}")]
    WrongOrigin {
        /// The provider the wire belongs to.
        expected: ReplayOrigin,
        /// The message's recorded origin.
        found: Option<ReplayOrigin>,
    },
    /// A part requires replay state for same-provider replay (e.g. an
    /// Anthropic thinking block's signature) but carries none. The state is
    /// never fabricated.
    #[error("{provider} part ordinal {part_ordinal} is missing required replay state")]
    MissingReplayState {
        /// The provider being projected.
        provider: Protocol,
        /// Ordinal of the part in the message.
        part_ordinal: usize,
    },
    /// A part kind the provider's assistant wire cannot express.
    #[error("{provider} wire cannot express part ordinal {part_ordinal}")]
    UnsupportedPart {
        /// The provider being projected.
        provider: Protocol,
        /// Ordinal of the part in the message.
        part_ordinal: usize,
    },
    /// A ToolCall on the wire failed strict validation; the whole message is
    /// invalid (ADR 0005 §7).
    #[error("invalid tool call: {0}")]
    InvalidToolCall(#[from] ToolCallValidation),
    /// Assembling the wire body crossed a protocol hard limit (ADR 0005 §10).
    /// The message is rejected whole; no partial conversation is returned.
    #[error("{provider} wire exceeds a protocol limit: {interruption:?}")]
    OutputLimitExceeded {
        /// The provider being projected.
        provider: Protocol,
        /// The typed interruption recording which limit was crossed.
        interruption: AttemptInterruption,
    },
    /// A ToolResult closure names an invocation the assistant message never
    /// bound.
    #[error("{provider} tool result closes unknown invocation {invocation_id}")]
    UnmatchedToolResult {
        /// The provider being projected.
        provider: Protocol,
        /// The invocation the closure claimed to close.
        invocation_id: InvocationId,
    },
    /// The target provider's wire cannot encode a recorded closure without a
    /// provider call ID, and the closing ToolCall legitimately carries none
    /// (ADR 0005 §1: provider IDs are optional facts, never synthesized).
    #[error(
        "{provider} wire cannot encode the tool result for {invocation_id} without a provider call id"
    )]
    ToolResultWithoutProviderCallId {
        /// The provider being projected.
        provider: Protocol,
        /// The invocation whose closure cannot be encoded.
        invocation_id: InvocationId,
    },
    /// The usage block on an otherwise accepted body is malformed.
    #[error("malformed {provider} usage block: {detail}")]
    MalformedUsage {
        /// The provider being projected.
        provider: Protocol,
        /// What was missing or mistyped.
        detail: String,
    },
}

/// 完整 wire 也逐块走同一累积器；这里仅把其终态映射为投影错误。
struct WireReply {
    origin: Protocol,
    accumulator: ReplyAccumulator,
}
impl WireReply {
    fn new(origin: &ReplayOrigin) -> Self {
        Self {
            origin: origin.protocol,
            accumulator: ReplyAccumulator::new(Some(origin.clone())),
        }
    }
    fn push(&mut self, delta: PartDelta) -> Result<(), ProjectionError> {
        match self
            .accumulator
            .push(delta)
            .expect("wire decoding stops at the first failure")
        {
            Update::Accepted { .. } => Ok(()),
            Update::Ended => self.outcome().map(|_| ()),
        }
    }
    fn finish(mut self) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
        self.accumulator.finish().expect("open wire reply");
        self.outcome()
    }
    fn outcome(&mut self) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
        match self
            .accumulator
            .take_outcome()
            .expect("one terminal outcome")
        {
            ReplyOutcome::Complete { message } => Ok(message.into_message()),
            ReplyOutcome::Rejected {
                rejection: lattice_conversation::AttemptRejection::InvalidToolCall { validation },
            } => Err(ProjectionError::InvalidToolCall(validation)),
            ReplyOutcome::Interrupted {
                reason: ReplyStop::OutputLimit(interruption),
            } => Err(ProjectionError::OutputLimitExceeded {
                provider: self.origin,
                interruption,
            }),
            ReplyOutcome::Interrupted {
                reason: ReplyStop::Stream(StreamStop::Protocol(error)),
            } => Err(ProjectionError::Protocol(error)),
            ReplyOutcome::Interrupted { .. } => {
                unreachable!("synchronous wire decoder has no transport")
            }
        }
    }
}

/// 只用于完整 wire 夹具的解码，保留部件顺序与逐部件回放状态。
pub fn wire_to_native(
    origin: &ReplayOrigin,
    wire: &Value,
) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    match origin.protocol {
        Protocol::ChatCompletions => chat_completions::wire_to_native(origin, wire),
        Protocol::Messages => anthropic_messages::wire_to_native(origin, wire),
        Protocol::GenerateContent => generate_content::wire_to_native(origin, wire),
        protocol => Err(ProjectionError::UnsupportedProtocol { protocol }),
    }
}

/// 把完整 wire 夹具的消息还原为原生助手对象；Chat Completions 使用 reasoning_content。
/// 生产请求由 request_messages_to_wire 按当前 features 决定 reasoning 编码。
/// 绑定前后的消息共用规则，来源与协议必需的回放状态必须完整。
pub fn native_to_wire<S: ToolCallState>(
    origin: &ReplayOrigin,
    message: &NativeMessage<S>,
) -> Result<Value, ProjectionError> {
    native_to_wire_with_replay(origin, message, ReasoningReplay::ReasoningContent)
}

fn native_to_wire_with_replay<S: ToolCallState>(
    origin: &ReplayOrigin,
    message: &NativeMessage<S>,
    replay: ReasoningReplay,
) -> Result<Value, ProjectionError> {
    if message.provider_origin.as_ref() != Some(origin) {
        return Err(ProjectionError::WrongOrigin {
            expected: origin.clone(),
            found: message.provider_origin.clone(),
        });
    }
    match origin.protocol {
        Protocol::ChatCompletions => chat_completions::native_to_wire(message, replay),
        Protocol::Messages => anthropic_messages::native_to_wire(message),
        Protocol::GenerateContent => generate_content::native_to_wire(message),
        protocol => Err(ProjectionError::UnsupportedProtocol { protocol }),
    }
}

/// 按目标来源投影消息副本；完整来源相同保留所有材料，协议相同保留 call ID。
/// 来源 reasoning 与逐片段回放状态只在完整来源相同时保留。
/// 切回原来源时使用历史中的原消息，投影不修改它。
pub fn project_cross_provider<S: ToolCallState>(
    message: &NativeMessage<S>,
    target: &ReplayOrigin,
) -> NativeMessage<S> {
    let same_provider = message
        .provider_origin
        .as_ref()
        .is_some_and(|source| source.same_provider(target));
    let same_protocol = message
        .provider_origin
        .as_ref()
        .is_some_and(|source| source.same_protocol(target));
    let mut projected = message.clone();
    projected.provider_origin = Some(target.clone());
    if !same_provider {
        projected
            .parts
            .retain(|part| !matches!(part.semantic, MessageSemantic::Reasoning { .. }));
        for part in &mut projected.parts {
            part.replay_state = None;
        }
    }
    if !same_protocol {
        for part in &mut projected.parts {
            if let MessageSemantic::ToolCall { call } = &mut part.semantic {
                call.clear_provider_call_id();
            }
        }
    }
    projected
}

/// Provider request messages projected from durable bound conversation history.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestMessages {
    /// Provider system-instruction text, merged in source order.
    pub system: Option<String>,
    /// Provider-native non-system message objects in conversation order.
    pub messages: Vec<Value>,
}

/// 生产 ToolCall/ToolResult 的唯一请求编码入口；默认不回放 Chat Completions reasoning。
/// 按完整来源决定回放材料，按协议决定 call ID，不补造目标 provider 身份。
pub fn request_messages_to_wire(
    origin: &ReplayOrigin,
    history: &[ConversationItem],
) -> Result<RequestMessages, ProjectionError> {
    request_messages_with_replay(origin, history, ReasoningReplay::None)
}

pub(crate) fn request_messages_with_replay(
    origin: &ReplayOrigin,
    history: &[ConversationItem],
    replay: ReasoningReplay,
) -> Result<RequestMessages, ProjectionError> {
    if matches!(
        origin.protocol,
        Protocol::Responses | Protocol::Interactions
    ) {
        return Err(ProjectionError::UnsupportedProtocol {
            protocol: origin.protocol,
        });
    }
    let mut system = None;
    let mut messages = Vec::new();
    for item in history {
        match item {
            ConversationItem::Message(message) => match message.role {
                MessageRole::System => {
                    push_system_text(&mut system, &plain_text(origin.protocol, message)?)
                }
                MessageRole::User => messages.push(user_to_wire(
                    origin.protocol,
                    &plain_text(origin.protocol, message)?,
                )),
                _ => {
                    return Err(ProjectionError::InvalidStep {
                        detail: "standalone message must be system or user",
                    });
                }
            },
            ConversationItem::Step(step) => {
                let projected = project_cross_provider(&step.assistant, origin);
                // 先核完整配对，再生成 wire；不从相邻历史修补结果。
                let results = tool_results_to_wire(origin.protocol, &projected, &step.results)?;
                messages.push(native_to_wire_with_replay(origin, &projected, replay)?);
                messages.extend(results);
            }
        }
    }
    Ok(RequestMessages { system, messages })
}

fn plain_text(
    origin: Protocol,
    message: &NativeMessage<BoundToolCall>,
) -> Result<String, ProjectionError> {
    let mut text = String::new();
    for (ordinal, part) in message.parts.iter().enumerate() {
        if part.replay_state.is_some() {
            return Err(ProjectionError::UnsupportedPart {
                provider: origin,
                part_ordinal: ordinal,
            });
        }
        match &part.semantic {
            MessageSemantic::Text { text: delta } => text.push_str(delta),
            _ => {
                return Err(ProjectionError::UnsupportedPart {
                    provider: origin,
                    part_ordinal: ordinal,
                });
            }
        }
    }
    Ok(text)
}

fn push_system_text(system: &mut Option<String>, text: &str) {
    match system {
        Some(existing) => {
            existing.push_str("\n\n");
            existing.push_str(text);
        }
        None => *system = Some(text.to_string()),
    }
}

fn user_to_wire(origin: Protocol, text: &str) -> Value {
    match origin {
        Protocol::ChatCompletions => serde_json::json!({
            "role": "user",
            "content": text,
        }),
        Protocol::Messages => serde_json::json!({
            "role": "user",
            "content": [{
                "type": "text",
                "text": if text.is_empty() { "(empty message)" } else { text },
            }],
        }),
        Protocol::GenerateContent => serde_json::json!({
            "role": "user",
            "parts": [{"text": text}],
        }),
        Protocol::Responses | Protocol::Interactions => {
            unreachable!("unsupported protocol rejected before projection")
        }
    }
}

/// 已核验配对后的 provider 输入，仅存在于本次投影。
struct ToolResultClosure {
    invocation_id: InvocationId,
    provider_call_id: Option<String>,
    name: String,
    outcome: ToolResultOutcome,
    content: String,
}

fn closure_content(payload: &ResultPayload) -> String {
    use serde_json::json;
    let value = match payload {
        ResultPayload::Succeeded(SuccessContent::Json { value, .. }) => {
            return canonical_json(value);
        }
        ResultPayload::Succeeded(SuccessContent::OutputTooLarge {
            digest,
            byte_length,
        }) => json!({
            "outcome": "succeeded", "content_unavailable": "output_too_large",
            "digest": digest.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "byte_length": byte_length,
        }),
        ResultPayload::Failed(error) => {
            const BOUNDED_ERROR_CONTENT_BYTES: usize = 4_096;
            let mut end = error.message.len().min(BOUNDED_ERROR_CONTENT_BYTES);
            while !error.message.is_char_boundary(end) {
                end -= 1;
            }
            json!({"error":{"code":error.code.as_i32(), "message":&error.message[..end]}})
        }
        ResultPayload::NotExecuted => json!({"outcome":"not_executed"}),
        ResultPayload::Indeterminate(cause) => json!({"outcome":"indeterminate","cause":cause}),
    };
    canonical_json(&value)
}

/// 按助手调用顺序核验精确结果配对；provider ID 只来自同一个调用。
pub fn tool_results_to_wire(
    origin: Protocol,
    assistant: &NativeMessage<BoundToolCall>,
    results: &[StoredToolResult],
) -> Result<Vec<Value>, ProjectionError> {
    let invalid = |detail| ProjectionError::InvalidStep { detail };
    if assistant.role != MessageRole::Assistant {
        return Err(invalid(
            "step requires an assistant without embedded results",
        ));
    }
    let calls: Vec<_> = assistant.tool_calls().collect();
    if calls.len() != results.len() {
        return Err(invalid("result count mismatch"));
    }
    let mut ids = BTreeSet::new();
    let mut closures = Vec::with_capacity(results.len());
    for ((_, call), result) in calls.into_iter().zip(results) {
        if call.invocation_id != result.invocation_id
            || call.name != result.name
            || !ids.insert(result.invocation_id)
        {
            return Err(invalid("result identity, name or order mismatch"));
        }
        match &result.payload {
            ResultPayload::Succeeded(SuccessContent::Json {
                value,
                digest,
                byte_length,
            }) => {
                let bytes = canonical_json(value);
                if bytes.len() > MAX_TOOL_CONTENT_BYTES
                    || bytes.len() as u64 != *byte_length
                    || tool_content_digest(&bytes) != *digest
                {
                    return Err(invalid("success content identity mismatch"));
                }
            }
            ResultPayload::Succeeded(SuccessContent::OutputTooLarge { byte_length, .. })
                if *byte_length <= MAX_TOOL_CONTENT_BYTES as u64 =>
            {
                return Err(invalid("invalid oversized result length"));
            }
            _ => {}
        }
        closures.push(ToolResultClosure {
            invocation_id: result.invocation_id,
            provider_call_id: call.provider_call_id.clone(),
            name: result.name.clone(),
            outcome: result.payload.outcome(),
            content: closure_content(&result.payload),
        });
    }
    if closures.is_empty() {
        return Ok(Vec::new());
    }
    match origin {
        Protocol::ChatCompletions => chat_completions::tool_result_messages(&closures),
        Protocol::Messages => anthropic_messages::tool_result_messages(&closures),
        Protocol::GenerateContent => generate_content::tool_result_messages(&closures),
        protocol => Err(ProjectionError::UnsupportedProtocol { protocol }),
    }
}

/// Extract the provider-reported usage from an official response body.
///
/// `Ok(None)` when the body carries no usage block: missing usage is unknown,
/// never zero. A present but malformed block fails closed.
pub fn wire_usage(origin: Protocol, wire: &Value) -> Result<Option<TokenUsage>, ProjectionError> {
    match origin {
        Protocol::ChatCompletions => chat_completions::usage(wire),
        Protocol::Messages => anthropic_messages::usage(wire),
        Protocol::GenerateContent => generate_content::usage(wire),
        protocol => Err(ProjectionError::UnsupportedProtocol { protocol }),
    }
}
