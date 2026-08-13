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

use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, ProviderOrigin, RawToolCall,
    UnboundToolCall, validate_tool_call_batch,
};
use serde_json::{Value, json};

use super::ProjectionError;

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: ProviderOrigin::OpenAiCompatible,
        detail: detail.into(),
    }
}

pub fn wire_to_native(wire: &Value) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let message = wire
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| malformed("missing choices[0].message"))?;

    let mut parts: Vec<MessagePart<UnboundToolCall>> = Vec::new();

    if let Some(reasoning) = message.get("reasoning_content").and_then(Value::as_str) {
        parts.push(MessagePart::semantic_only(MessageSemantic::Reasoning {
            text: reasoning.to_string(),
        }));
    }
    match message.get("content") {
        Some(Value::String(text)) => {
            parts.push(MessagePart::semantic_only(MessageSemantic::Text {
                text: text.clone(),
            }));
        }
        // Explicit null or absent content carries no text part.
        Some(Value::Null) | None => {}
        _ => return Err(malformed("message.content is not a string or null")),
    }

    let mut raw_calls: Vec<RawToolCall> = Vec::new();
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
                raw_calls.push(RawToolCall {
                    provider_call_id,
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                });
            }
        }
        Some(Value::Null) | None => {}
        _ => return Err(malformed("message.tool_calls is not an array")),
    }
    for call in validate_tool_call_batch(&raw_calls)? {
        parts.push(MessagePart::semantic_only(MessageSemantic::ToolCall {
            call,
        }));
    }

    Ok(NativeMessage::new(
        MessageRole::Assistant,
        Some(ProviderOrigin::OpenAiCompatible),
        parts,
    ))
}

pub fn native_to_wire(message: &NativeMessage<UnboundToolCall>) -> Result<Value, ProjectionError> {
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
                        "name": call.name,
                        "arguments": lattice_conversation::canonical_json(&call.arguments),
                    },
                });
                if let Some(id) = &call.provider_call_id {
                    function_call["id"] = json!(id);
                }
                tool_calls.push(function_call);
            }
            MessageSemantic::ToolResult { .. } => {
                return Err(ProjectionError::UnsupportedPart {
                    provider: ProviderOrigin::OpenAiCompatible,
                    part_ordinal: ordinal,
                });
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
