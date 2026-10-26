// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! OpenAI-compatible chat-completions assistant wire projection.
//!
//! Wire shape (official response body):
//!
//! ```json
//! {"choices": [{"message": {
//!     "role": "assistant",
//!     "content": "…" | null,
//!     "reasoning_content": "…"?,
//!     "tool_calls": [{"id": "call_…", "type": "function",
//!                     "function": {"name": "…", "arguments": "{…}"}}]?
//! }}]}
//! ```
//!
//! OpenAI-compatible wire carries no signature state, so no part gains
//! replay state here. Tool call IDs are preserved verbatim when present and
//! left `None` when absent; they are never synthesized.
//!
//! Two lossy properties of the writer are deliberate and fixed here:
//!
//! 1. **Arguments are re-serialized canonically.** ADR 0005 §7 requires
//!    execution to use the canonical arguments object rather than the
//!    provider's textual spelling, so `native_to_wire` emits
//!    `canonical_json(arguments)`. Replay of a non-canonically spelled
//!    provider body is therefore *semantically* identical but **not**
//!    byte-identical (`{"b": 2, "a": 1}` replays as `{"a":1,"b":2}`).
//! 2. **Interleaved text parts merge.** The OpenAI wire has a single
//!    `content` string, so `[Text("A"), ToolCall, Text("B")]` writes back as
//!    `content: "AB"`. The text/tool interleaving is not recoverable from the
//!    wire. This is inherent to the format, not a defect of the mapping.
//!
//! Assembly is metered against the ADR 0005 §10 protocol limits; crossing one
//! fails the whole body closed rather than returning a partial message.

use lattice_conversation::{
    MessageSemantic, NativeMessage, ProviderOrigin, TokenUsage, ToolCallState, UnboundToolCall,
};
use serde_json::{Value, json};

use super::{ProjectionError, ToolResultClosure};
use crate::protocol::chat_completions::{ContentError, visit_content};
use crate::reply::{PartDelta, PartKind, StringPatch};

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: ProviderOrigin::OpenAiCompatible,
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

pub fn wire_to_native(wire: &Value) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let message = wire
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| malformed("missing choices[0].message"))?;
    let mut reply = super::WireReply::new(ProviderOrigin::OpenAiCompatible);
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
                provider: ProviderOrigin::OpenAiCompatible,
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
        wire["reasoning_content"] = Value::String(reasoning);
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
                provider: ProviderOrigin::OpenAiCompatible,
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

/// `usage: {prompt_tokens, completion_tokens, total_tokens}` (all official
/// fields; absent usage is unknown, not zero).
pub(crate) fn usage(wire: &Value) -> Result<Option<TokenUsage>, ProjectionError> {
    let Some(usage) = wire.get("usage") else {
        return Ok(None);
    };
    if usage.is_null() {
        return Ok(None);
    }
    let malformed = |detail: String| ProjectionError::MalformedUsage {
        provider: ProviderOrigin::OpenAiCompatible,
        detail,
    };
    let field = |name: &str| -> Result<u64, ProjectionError> {
        usage
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| malformed(format!("usage.{name} is not an integer")))
    };
    Ok(Some(TokenUsage {
        prompt_tokens: field("prompt_tokens")?,
        completion_tokens: field("completion_tokens")?,
        total_tokens: field("total_tokens")?,
    }))
}
