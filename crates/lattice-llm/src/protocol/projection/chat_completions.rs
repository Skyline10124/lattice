// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Chat Completions 的助手消息编解码。
//!
//! 读取 content 字符串、content 分块与 reasoning_content，保留真实的可选 call ID。
//! 请求按 reasoning_replay 省略 reasoning、写入 reasoning_content，或放在 content 的
//! thinking 块中。该协议不接收逐片段签名状态，携带这类状态的同来源消息编码时拒绝。
//!
//! 参数以 canonical JSON 写回，同类正文与 reasoning 各自合并；content 分块中的
//! thinking 块排在正文之前，工具调用保留在 tool_calls 中。原始空白及正文与工具的
//! 交错不能由请求格式还原。完整 wire 解码与流式路径共用累积器和相同限额。

use lattice_conversation::{
    MessageSemantic, NativeMessage, Protocol, ReplayOrigin, TokenUsage, ToolCallState,
    UnboundToolCall,
};
use serde_json::{Value, json};

use super::{ProjectionError, ToolResultClosure};
use crate::protocol::ReasoningReplay;
use crate::protocol::chat_completions::{ContentError, visit_content};
use crate::reply::{PartDelta, PartKind, StringPatch};

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: Protocol::ChatCompletions,
        detail: detail.into(),
    }
}

impl From<ContentError> for ProjectionError {
    fn from(error: ContentError) -> Self {
        match error {
            ContentError::Unsupported { index, block_type } => Self::Protocol(
                crate::ReplyProtocolError::unsupported("chat-completions", index, &block_type),
            ),
            ContentError::Malformed(detail) => malformed(detail),
        }
    }
}

pub fn wire_to_native(
    origin: &ReplayOrigin,
    wire: &Value,
) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let message = wire
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| malformed("missing choices[0].message"))?;
    let mut reply = super::WireReply::new(origin);
    let mut index = 0;
    {
        let mut emit = |kind, text: &str| {
            reply.push(PartDelta::Begin { index, kind })?;
            reply.push(if kind == PartKind::Text {
                PartDelta::Text {
                    index,
                    delta: text.into(),
                }
            } else {
                PartDelta::Reasoning {
                    index,
                    delta: text.into(),
                }
            })?;
            reply.push(PartDelta::End { index })?;
            index += 1;
            Ok::<_, ProjectionError>(())
        };
        match message.get("reasoning_content") {
            Some(Value::String(text)) => emit(PartKind::Reasoning, text)?,
            Some(Value::Null) | None => {}
            _ => {
                return Err(malformed(
                    "message reasoning_content is not a string or null",
                ));
            }
        }
        if let Some(content) = message.get("content") {
            visit_content(content, &mut emit)?;
        }
    }
    match message.get("tool_calls") {
        Some(Value::Array(calls)) => {
            for (call_index, call) in calls.iter().enumerate() {
                if let Some(kind) = call.get("type")
                    && kind.as_str() != Some("function")
                {
                    return Err(ProjectionError::Protocol(
                        crate::ReplyProtocolError::unsupported(
                            "chat-completions",
                            call_index,
                            kind.as_str().unwrap_or("<non-string>"),
                        ),
                    ));
                }
                let function = call
                    .get("function")
                    .ok_or_else(|| malformed("tool_calls[] missing function"))?;
                let name = function["name"]
                    .as_str()
                    .ok_or_else(|| malformed("tool_calls[].function missing name"))?;
                let arguments = function["arguments"]
                    .as_str()
                    .ok_or_else(|| malformed("tool_calls[].function missing arguments"))?;
                let provider_call_id = match call.get("id") {
                    Some(Value::String(id)) => Some(StringPatch::Set(id.clone())),
                    Some(Value::Null) | None => None,
                    _ => return Err(malformed("tool_calls[].id is not a string")),
                };
                reply.push(PartDelta::Begin {
                    index,
                    kind: PartKind::ToolCall,
                })?;
                reply.push(PartDelta::ToolMetadata {
                    index,
                    name: Some(StringPatch::Set(name.into())),
                    provider_call_id,
                })?;
                reply.push(PartDelta::ToolArguments {
                    index,
                    delta: arguments.into(),
                })?;
                reply.push(PartDelta::End { index })?;
                index += 1;
            }
        }
        Some(Value::Null) | None => {}
        _ => return Err(malformed("message.tool_calls is not an array")),
    }
    reply.finish()
}

pub fn native_to_wire<S: ToolCallState>(
    message: &NativeMessage<S>,
    replay: ReasoningReplay,
) -> Result<Value, ProjectionError> {
    let mut reasoning = String::new();
    let mut saw_reasoning = false;
    let mut text = String::new();
    let mut saw_text = false;
    let mut tool_calls: Vec<Value> = Vec::new();

    for (ordinal, part) in message.parts.iter().enumerate() {
        // OpenAI wire has no signature slots; replay state has nowhere to go.
        if part.replay_state.is_some() {
            return Err(ProjectionError::UnsupportedPart {
                provider: Protocol::ChatCompletions,
                part_ordinal: ordinal,
            });
        }
        match &part.semantic {
            MessageSemantic::Text { text: delta } => {
                saw_text = true;
                text.push_str(delta);
            }
            MessageSemantic::Reasoning { text: delta } => {
                saw_reasoning = true;
                reasoning.push_str(delta);
            }
            MessageSemantic::ToolCall { call } => {
                let mut function_call = json!({
                    "type": "function",
                    "function": {
                        "name": call.name(),
                        "arguments": lattice_conversation::canonical_json(call.arguments()),
                    },
                });
                if let Some(id) = call.provider_call_id() {
                    function_call["id"] = json!(id);
                }
                tool_calls.push(function_call);
            }
        }
    }

    let mut wire = json!({
        "role": "assistant",
        "content": if saw_text { Value::String(text) } else { Value::Null },
    });
    if saw_reasoning {
        match replay {
            ReasoningReplay::None => {}
            ReasoningReplay::ReasoningContent => {
                wire["reasoning_content"] = Value::String(reasoning)
            }
            ReasoningReplay::ContentChunks => {
                let mut chunks = vec![
                    json!({"type":"thinking", "thinking":[{"type":"text", "text":reasoning}]}),
                ];
                if saw_text {
                    chunks.push(json!({"type":"text", "text":wire["content"]}));
                }
                wire["content"] = Value::Array(chunks);
            }
        }
    }
    if !tool_calls.is_empty() {
        wire["tool_calls"] = Value::Array(tool_calls);
    }
    Ok(wire)
}

/// One `role: "tool"` request message per closure. The wire correlates a
/// result to its call only through `tool_call_id`, so a closing call with no
/// provider ID cannot be encoded — a typed error, never a synthesized ID.
pub(crate) fn tool_result_messages(
    closures: &[ToolResultClosure],
) -> Result<Vec<Value>, ProjectionError> {
    let mut messages = Vec::with_capacity(closures.len());
    for closure in closures {
        let Some(tool_call_id) = closure.provider_call_id.as_deref() else {
            return Err(ProjectionError::ToolResultWithoutProviderCallId {
                provider: Protocol::ChatCompletions,
                invocation_id: closure.invocation_id,
            });
        };
        messages.push(json!({
            "role": "tool",
            "tool_call_id": tool_call_id,
            "content": closure.content,
        }));
    }
    Ok(messages)
}

/// 完整 wire 与流式响应共用原生分项的换算。
pub(crate) fn usage(wire: &Value) -> Result<Option<TokenUsage>, ProjectionError> {
    super::super::usage::UsageSnapshot::default().update(Protocol::ChatCompletions, wire)
}
