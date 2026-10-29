// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Anthropic Messages 的助手消息投影。
//!
//! 官方响应正文格式：
//!
//! ```json
//! {"role": "assistant", "content": [
//!     {"type": "text", "text": "…"},
//!     {"type": "thinking", "thinking": "…", "signature": "…"},
//!     {"type": "tool_use", "id": "toolu_…", "name": "…", "input": {…}}
//! ]}
//! ```
//!
//! `thinking` 块的 `signature` 是协议要求的回放状态，按部件原样保存，
//! 并在同一 provider 的请求中原位回放。缺少回放状态的 `thinking` 部件
//! 无法写回 Anthropic 消息，投影器不会补造签名。
//!
//! `signature` 仅属于受支持的 `thinking` 块。协议无法表达 `text` 部件的回放状态，
//! 因此写入时拒绝携带回放状态的 `text` 部件，不写出读回时会被静默丢弃的字段。
//!
//! 组装遵循 ADR 0005 §10 的协议限额；超限拒绝整个响应，不返回部分消息。

use lattice_conversation::{
    MessageSemantic, NativeMessage, Protocol, ReplayOrigin, TokenUsage, ToolCallState,
    UnboundToolCall,
};
use serde_json::{Value, json};

use super::{ProjectionError, ToolResultClosure};
use crate::reply::{PartDelta, PartKind, StringPatch};

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: Protocol::Messages,
        detail: detail.into(),
    }
}

pub fn wire_to_native(
    origin: &ReplayOrigin,
    wire: &Value,
) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let blocks = wire
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("missing content blocks"))?;
    let mut reply = super::WireReply::new(origin);
    for (index, block) in blocks.iter().enumerate() {
        let block_type = block
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| malformed("content block missing type"))?;
        let kind = match block_type {
            "text" => PartKind::Text,
            "thinking" => PartKind::Reasoning,
            "tool_use" => PartKind::ToolCall,
            other => {
                return Err(ProjectionError::Protocol(
                    crate::ReplyProtocolError::unsupported("anthropic-messages", index, other),
                ));
            }
        };
        reply.push(PartDelta::Begin { index, kind })?;
        match kind {
            PartKind::Text => {
                let text = block["text"]
                    .as_str()
                    .ok_or_else(|| malformed("text block missing text"))?;
                reply.push(PartDelta::Text {
                    index,
                    delta: text.into(),
                })?;
            }
            PartKind::Reasoning => {
                let text = block["thinking"]
                    .as_str()
                    .ok_or_else(|| malformed("thinking block missing thinking"))?;
                reply.push(PartDelta::Reasoning {
                    index,
                    delta: text.into(),
                })?;
                match block.get("signature") {
                    Some(Value::String(signature)) => reply.push(PartDelta::ReplayState {
                        index,
                        patch: StringPatch::Set(signature.clone()),
                    })?,
                    Some(Value::Null) | None => {}
                    _ => return Err(malformed("thinking signature is not a string")),
                }
            }
            PartKind::ToolCall => {
                let name = block["name"]
                    .as_str()
                    .ok_or_else(|| malformed("tool_use block missing name"))?;
                let arguments = block
                    .get("input")
                    .ok_or_else(|| malformed("tool_use block missing input"))?
                    .to_string();
                let provider_call_id = match block.get("id") {
                    Some(Value::String(id)) => Some(StringPatch::Set(id.clone())),
                    Some(Value::Null) | None => None,
                    _ => return Err(malformed("tool_use id is not a string")),
                };
                reply.push(PartDelta::ToolMetadata {
                    index,
                    name: Some(StringPatch::Set(name.into())),
                    provider_call_id,
                })?;
                reply.push(PartDelta::ToolArguments {
                    index,
                    delta: arguments,
                })?;
            }
        }
        reply.push(PartDelta::End { index })?;
    }
    reply.finish()
}

pub fn native_to_wire<S: ToolCallState>(
    message: &NativeMessage<S>,
) -> Result<Value, ProjectionError> {
    let mut blocks: Vec<Value> = Vec::new();
    for (ordinal, part) in message.parts.iter().enumerate() {
        match &part.semantic {
            MessageSemantic::Text { text } => {
                // `signature` exists only on thinking blocks. Emitting it on a
                // text block would produce a field the reader drops, silently
                // losing the state on round-trip.
                if part.replay_state.is_some() {
                    return Err(ProjectionError::UnsupportedPart {
                        provider: Protocol::Messages,
                        part_ordinal: ordinal,
                    });
                }
                blocks.push(json!({ "type": "text", "text": text }));
            }
            MessageSemantic::Reasoning { text } => {
                let state =
                    part.replay_state
                        .as_ref()
                        .ok_or(ProjectionError::MissingReplayState {
                            provider: Protocol::Messages,
                            part_ordinal: ordinal,
                        })?;
                blocks.push(json!({
                    "type": "thinking",
                    "thinking": text,
                    "signature": state.as_str(),
                }));
            }
            MessageSemantic::ToolCall { call } => {
                let mut block = json!({
                    "type": "tool_use",
                    "name": call.name(),
                    "input": call.arguments(),
                });
                if let Some(id) = call.provider_call_id() {
                    block["id"] = json!(id);
                }
                blocks.push(block);
            }
        }
    }
    Ok(json!({
        "role": "assistant",
        "content": blocks,
    }))
}

/// One user turn whose content carries every `tool_result` block, in closure
/// order. Blocks correlate to their `tool_use` through `tool_use_id`, so a
/// closing call with no provider ID cannot be encoded. `Failed` closures set
/// the official `is_error` flag.
pub(crate) fn tool_result_messages(
    closures: &[ToolResultClosure],
) -> Result<Vec<Value>, ProjectionError> {
    let mut blocks: Vec<Value> = Vec::with_capacity(closures.len());
    for closure in closures {
        let Some(tool_use_id) = closure.provider_call_id.as_deref() else {
            return Err(ProjectionError::ToolResultWithoutProviderCallId {
                provider: Protocol::Messages,
                invocation_id: closure.invocation_id,
            });
        };
        let mut block = json!({
            "type": "tool_result",
            "tool_use_id": tool_use_id,
            "content": closure.content,
        });
        if matches!(
            closure.outcome,
            lattice_conversation::ToolResultOutcome::Failed
        ) {
            block["is_error"] = json!(true);
        }
        blocks.push(block);
    }
    Ok(vec![json!({
        "role": "user",
        "content": blocks,
    })])
}

/// `usage: {input_tokens, output_tokens}`; the neutral total is the sum
/// (official bodies may add cache fields, which the neutral contract does not
/// carry).
pub(crate) fn usage(wire: &Value) -> Result<Option<TokenUsage>, ProjectionError> {
    let Some(usage) = wire.get("usage") else {
        return Ok(None);
    };
    if usage.is_null() {
        return Ok(None);
    }
    let malformed = |detail: String| ProjectionError::MalformedUsage {
        provider: Protocol::Messages,
        detail,
    };
    let field = |name: &str| -> Result<u64, ProjectionError> {
        usage
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| malformed(format!("usage.{name} is not an integer")))
    };
    let prompt_tokens = field("input_tokens")?;
    let completion_tokens = field("output_tokens")?;
    Ok(Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens.saturating_add(completion_tokens),
    }))
}
