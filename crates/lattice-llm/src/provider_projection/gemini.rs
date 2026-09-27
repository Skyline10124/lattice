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
    AttemptAssembly, MessagePart, MessageRole, MessageSemantic, NativeMessage, OpaqueProviderState,
    ProviderOrigin, RawToolCall, TokenUsage, ToolCallState, ToolResultOutcome, UnboundToolCall,
    validate_tool_call_batch,
};
use serde_json::{Value, json};

use super::{ProjectionError, ToolResultClosure};

fn limit_exceeded(interruption: lattice_conversation::AttemptInterruption) -> ProjectionError {
    ProjectionError::OutputLimitExceeded {
        provider: ProviderOrigin::Gemini,
        interruption,
    }
}

fn malformed(detail: impl Into<String>) -> ProjectionError {
    ProjectionError::MalformedWire {
        provider: ProviderOrigin::Gemini,
        detail: detail.into(),
    }
}

fn replay_state_of(part: &Value) -> Result<Option<OpaqueProviderState>, ProjectionError> {
    match part.get("thoughtSignature") {
        Some(Value::String(signature)) => Ok(Some(
            OpaqueProviderState::new(signature.clone())
                .map_err(|_| malformed("thoughtSignature exceeds replay-state bound"))?,
        )),
        Some(Value::Null) | None => Ok(None),
        _ => Err(malformed("thoughtSignature is not a string")),
    }
}

pub fn wire_to_native(wire: &Value) -> Result<NativeMessage<UnboundToolCall>, ProjectionError> {
    let wire_parts = wire
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .ok_or_else(|| malformed("missing candidates[0].content.parts"))?;

    // Two-phase assembly, same as the Anthropic projector: parse in wire
    // order, validate ToolCalls as one batch, splice back at wire positions.
    enum Pending {
        Ready(MessagePart<UnboundToolCall>),
        Call(usize),
        /// A call part carrying its own thoughtSignature.
        SignedCall(usize, OpaqueProviderState),
    }

    let mut pending: Vec<Pending> = Vec::new();
    let mut raw_calls: Vec<RawToolCall> = Vec::new();
    let mut assembly = AttemptAssembly::new();
    for part in wire_parts {
        let replay_state = replay_state_of(part)?;
        let thought = part.get("thought").and_then(Value::as_bool) == Some(true);

        if let Some(function_call) = part.get("functionCall") {
            let name = function_call
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("functionCall missing name"))?;
            let arguments = match function_call.get("args") {
                Some(args) => serde_json::to_string(args)
                    .map_err(|_| malformed("functionCall args not serializable"))?,
                // Gemini may omit `args` for a parameterless function call; its
                // documented meaning is the empty input object.
                None => "{}".to_string(),
            };
            let provider_call_id = match function_call.get("id") {
                Some(Value::String(id)) => Some(id.to_string()),
                Some(Value::Null) | None => None,
                _ => return Err(malformed("functionCall id is not a string")),
            };
            assembly.begin_tool_call();
            assembly
                .push_tool_arguments(&arguments)
                .map_err(limit_exceeded)?;
            raw_calls.push(RawToolCall {
                provider_call_id,
                name: name.to_string(),
                arguments,
            });
            // A signature on a call part stays attached to that call part.
            let index = raw_calls.len() - 1;
            match replay_state {
                Some(state) => pending.push(Pending::SignedCall(index, state)),
                None => pending.push(Pending::Call(index)),
            }
            continue;
        }

        match part.get("text") {
            Some(Value::String(text)) => {
                let semantic = if thought {
                    assembly.push_reasoning(text).map_err(limit_exceeded)?;
                    MessageSemantic::Reasoning { text: text.clone() }
                } else {
                    assembly.push_assistant(text).map_err(limit_exceeded)?;
                    MessageSemantic::Text { text: text.clone() }
                };
                // An empty text part with a signature is preserved, not
                // filtered (grill R7).
                pending.push(Pending::Ready(MessagePart {
                    semantic,
                    replay_state,
                }));
            }
            Some(_) => return Err(malformed("text part is not a string")),
            None => return Err(malformed("part has neither text nor functionCall")),
        }
    }

    let validated = validate_tool_call_batch(&raw_calls)?;
    let mut parts: Vec<MessagePart<UnboundToolCall>> = Vec::with_capacity(pending.len());
    for slot in pending {
        match slot {
            Pending::Ready(part) => parts.push(part),
            Pending::Call(index) => {
                parts.push(MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: validated[index].clone(),
                }))
            }
            Pending::SignedCall(index, state) => parts.push(MessagePart {
                semantic: MessageSemantic::ToolCall {
                    call: validated[index].clone(),
                },
                replay_state: Some(state),
            }),
        }
    }

    Ok(NativeMessage::new(
        MessageRole::Assistant,
        Some(ProviderOrigin::Gemini),
        parts,
    ))
}

pub fn native_to_wire<S: ToolCallState>(
    message: &NativeMessage<S>,
) -> Result<Value, ProjectionError> {
    let mut wire_parts: Vec<Value> = Vec::new();
    for (ordinal, part) in message.parts.iter().enumerate() {
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
            MessageSemantic::ToolResult { .. } => {
                return Err(ProjectionError::UnsupportedPart {
                    provider: ProviderOrigin::Gemini,
                    part_ordinal: ordinal,
                });
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
/// normally. `Failed`/`Rejected` closures use the conventional
/// `response: {"error": …}` spelling; other outcomes pass an object content
/// through and wrap everything else as `{"result": …}`.
pub(crate) fn tool_result_messages(
    closures: &[ToolResultClosure],
) -> Result<Vec<Value>, ProjectionError> {
    let mut parts: Vec<Value> = Vec::with_capacity(closures.len());
    for closure in closures {
        let content = &closure.content;
        let response = match closure.outcome {
            ToolResultOutcome::Failed | ToolResultOutcome::Rejected => {
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
