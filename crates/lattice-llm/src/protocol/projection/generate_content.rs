// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Gemini generateContent assistant wire projection.
//!
//! Wire shape (official response body):
//!
//! ```json
//! {"candidates": [{"content": {"role": "model", "parts": [
//!     {"text": "…", "thought": true, "thoughtSignature": "…"},
//!     {"text": "…", "thoughtSignature": "…"},
//!     {"functionCall": {"name": "…", "args": {…}, "id": "…"?},
//!      "thoughtSignature": "…"}
//! ]}}]}
//! ```
//!
//! `thoughtSignature` may sit on any part — including an otherwise-empty
//! text part — and is preserved verbatim in place as part-local opaque
//! replay state. A real `functionCall.id` is preserved when present; when
//! absent the call stays `None` and no `tc_*`-style synthetic ID is ever
//! generated.
//!
//! Assembly is metered against the ADR 0005 §10 protocol limits; crossing one
//! fails the whole body closed rather than returning a partial message.

use lattice_conversation::{
    MessageSemantic, NativeMessage, ProviderOrigin, TokenUsage, ToolCallState, ToolResultOutcome,
    UnboundToolCall,
};
use serde_json::{Value, json};

use super::{ProjectionError, ToolResultClosure};
use crate::protocol::generate_content::unsupported_part_type;
use crate::reply::{PartDelta, PartKind, StringPatch};

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: ProviderOrigin::Gemini,
        detail: detail.into(),
    }
}

pub fn wire_to_native(wire: &Value) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let parts = wire
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("missing candidates[0].content.parts"))?;
    let mut reply = super::WireReply::new(ProviderOrigin::Gemini);
    for (index, part) in parts.iter().enumerate() {
        if part.get("text").is_some() && part.get("functionCall").is_some() {
            return Err(malformed("one part contains text and functionCall"));
        }
        if part.get("text").is_none() && part.get("functionCall").is_none() {
            return Err(ProjectionError::Protocol(
                crate::ReplyProtocolError::unsupported(
                    "generate-content",
                    index,
                    unsupported_part_type(part),
                ),
            ));
        }
        let kind = if part.get("functionCall").is_some() {
            PartKind::ToolCall
        } else if part["thought"].as_bool() == Some(true) {
            PartKind::Reasoning
        } else {
            PartKind::Text
        };
        reply.push(PartDelta::Begin { index, kind })?;
        if let Some(function) = part.get("functionCall") {
            let name = function["name"]
                .as_str()
                .ok_or_else(|| malformed("functionCall missing name"))?;
            let arguments = function
                .get("args")
                .map_or_else(|| "{}".into(), ToString::to_string);
            let provider_call_id = match function.get("id") {
                Some(Value::String(id)) => Some(StringPatch::Set(id.clone())),
                Some(Value::Null) | None => None,
                _ => return Err(malformed("functionCall id is not a string")),
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
        } else {
            let text = part["text"]
                .as_str()
                .ok_or_else(|| malformed("part has no string text or functionCall"))?;
            reply.push(if kind == PartKind::Reasoning {
                PartDelta::Reasoning {
                    index,
                    delta: text.into(),
                }
            } else {
                PartDelta::Text {
                    index,
                    delta: text.into(),
                }
            })?;
        }
        match part.get("thoughtSignature") {
            Some(Value::String(signature)) => reply.push(PartDelta::ReplayState {
                index,
                patch: StringPatch::Set(signature.clone()),
            })?,
            Some(Value::Null) | None => {}
            _ => return Err(malformed("thoughtSignature is not a string")),
        }
        reply.push(PartDelta::End { index })?;
    }
    reply.finish()
}

pub fn native_to_wire<S: ToolCallState>(
    message: &NativeMessage<S>,
) -> Result<Value, ProjectionError> {
    let mut wire_parts: Vec<Value> = Vec::new();
    for part in &message.parts {
        let thought_signature = part.replay_state.as_ref().map(|state| state.as_str());
        match &part.semantic {
            MessageSemantic::Text { text } => {
                let mut wire_part = json!({ "text": text });
                if let Some(signature) = thought_signature {
                    wire_part["thoughtSignature"] = json!(signature);
                }
                wire_parts.push(wire_part);
            }
            MessageSemantic::Reasoning { text } => {
                let mut wire_part = json!({ "text": text, "thought": true });
                if let Some(signature) = thought_signature {
                    wire_part["thoughtSignature"] = json!(signature);
                }
                wire_parts.push(wire_part);
            }
            MessageSemantic::ToolCall { call } => {
                let mut function_call = json!({
                    "name": call.name(),
                    "args": call.arguments(),
                });
                if let Some(id) = call.provider_call_id() {
                    function_call["id"] = json!(id);
                }
                let mut wire_part = json!({ "functionCall": function_call });
                if let Some(signature) = thought_signature {
                    wire_part["thoughtSignature"] = json!(signature);
                }
                wire_parts.push(wire_part);
            }
        }
    }
    Ok(json!({
        "role": "model",
        "parts": wire_parts,
    }))
}

/// One user turn whose parts carry every `functionResponse`, in closure
/// order. The Gemini result shape correlates by Tool name and carries no
/// provider call ID, so closures whose calls have no provider ID encode
/// normally. `Failed` closures use the conventional
/// `response: {"error": …}` spelling; other outcomes pass an object content
/// through and wrap everything else as `{"result": …}`.
pub(crate) fn tool_result_messages(
    closures: &[ToolResultClosure],
) -> Result<Vec<Value>, ProjectionError> {
    let mut parts: Vec<Value> = Vec::with_capacity(closures.len());
    for closure in closures {
        let content = &closure.content;
        let response = match closure.outcome {
            ToolResultOutcome::Failed => {
                json!({ "error": content })
            }
            _ => match serde_json::from_str::<Value>(content) {
                Ok(Value::Object(object)) => Value::Object(object),
                _ => json!({ "result": content }),
            },
        };
        parts.push(json!({
            "functionResponse": {
                "name": closure.name,
                "response": response,
            }
        }));
    }
    Ok(vec![json!({
        "role": "user",
        "parts": parts,
    })])
}

/// `usageMetadata: {promptTokenCount, candidatesTokenCount, totalTokenCount}`;
/// the neutral total is the sum when the body omits `totalTokenCount`.
pub(crate) fn usage(wire: &Value) -> Result<Option<TokenUsage>, ProjectionError> {
    let Some(usage) = wire.get("usageMetadata") else {
        return Ok(None);
    };
    if usage.is_null() {
        return Ok(None);
    }
    let malformed = |detail: String| ProjectionError::MalformedUsage {
        provider: ProviderOrigin::Gemini,
        detail,
    };
    let field = |name: &str| -> Result<u64, ProjectionError> {
        usage
            .get(name)
            .and_then(Value::as_u64)
            .ok_or_else(|| malformed(format!("usageMetadata.{name} is not an integer")))
    };
    let prompt_tokens = field("promptTokenCount")?;
    let completion_tokens = field("candidatesTokenCount")?;
    let total_tokens = match usage.get("totalTokenCount") {
        Some(total) => total.as_u64().ok_or_else(|| {
            malformed("usageMetadata.totalTokenCount is not an integer".to_string())
        })?,
        None => prompt_tokens.saturating_add(completion_tokens),
    };
    Ok(Some(TokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    }))
}
