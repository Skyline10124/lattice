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
    AttemptAssembly, MessagePart, MessageRole, MessageSemantic, NativeMessage, ProviderOrigin,
    RawToolCall, TokenUsage, ToolCallState, UnboundToolCall, validate_tool_call_batch,
};
use serde_json::{Value, json};

use super::{ProjectionError, ToolResultClosure};

fn limit_exceeded(interruption: lattice_conversation::AttemptInterruption) -> ProjectionError {
    ProjectionError::OutputLimitExceeded {
        provider: ProviderOrigin::OpenAiCompatible,
        interruption,
    }
}

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: ProviderOrigin::OpenAiCompatible,
        detail: detail.into(),
    }
}

/// 流解析器按首次到达位置保存的原始段；调用参数在整批验证前保持原文。
pub(crate) enum OpenAiPart {
    Text(String),
    Reasoning(String),
    ToolCall(RawToolCall),
}

pub(crate) fn parts_to_native(
    parts: Vec<OpenAiPart>,
) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let mut assembly = AttemptAssembly::new();
    let mut raw_calls = Vec::new();
    for part in &parts {
        match part {
            OpenAiPart::Text(text) => assembly.push_assistant(text).map_err(limit_exceeded)?,
            OpenAiPart::Reasoning(text) => assembly.push_reasoning(text).map_err(limit_exceeded)?,
            OpenAiPart::ToolCall(call) => {
                assembly.begin_tool_call();
                assembly
                    .push_tool_arguments(&call.arguments)
                    .map_err(limit_exceeded)?;
                raw_calls.push(call.clone());
            }
        }
    }
    let mut calls = validate_tool_call_batch(&raw_calls)?.into_iter();
    let parts = parts
        .into_iter()
        .map(|part| {
            MessagePart::semantic_only(match part {
                OpenAiPart::Text(text) => MessageSemantic::Text { text },
                OpenAiPart::Reasoning(text) => MessageSemantic::Reasoning { text },
                OpenAiPart::ToolCall(_) => MessageSemantic::ToolCall {
                    call: calls.next().expect("one validated call per raw part"),
                },
            })
        })
        .collect();
    Ok(NativeMessage::new(
        MessageRole::Assistant,
        Some(ProviderOrigin::OpenAiCompatible),
        parts,
    ))
}

pub fn wire_to_native(wire: &Value) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let message = wire
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| malformed("missing choices[0].message"))?;

    // 完整 response body 不携带交错信息；仅 SSE 路径能保留到达顺序。
    let mut parts = Vec::new();
    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
        parts.push(OpenAiPart::Reasoning(reasoning.to_string()));
    }
    match message.get("content") {
        Some(Value::String(text)) => {
            parts.push(OpenAiPart::Text(text.clone()));
        }
        // Explicit null or absent content carries no text part.
        Some(Value::Null) | None => {}
        _ => return Err(malformed("message.content is not a string or null")),
    }

    match message.get("tool_calls") {
        Some(Value::Array(calls)) => {
            for call in calls {
                let function = call
                    .get("function")
                    .ok_or_else(|| malformed("tool_calls[] missing function"))?;
                let name = function
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("tool_calls[].function missing name"))?;
                let arguments = function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("tool_calls[].function missing arguments"))?;
                let provider_call_id = match call.get("id") {
                    Some(Value::String(id)) => Some(id.to_string()),
                    Some(Value::Null) | None => None,
                    _ => return Err(malformed("tool_calls[].id is not a string")),
                };
                parts.push(OpenAiPart::ToolCall(RawToolCall {
                    provider_call_id,
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                }));
            }
        }
        Some(Value::Null) | None => {}
        _ => return Err(malformed("message.tool_calls is not an array")),
    }
    parts_to_native(parts)
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
