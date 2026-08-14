// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Semantic and replay digests (ADR 0005 §7).
//!
//! Two digests stay distinct:
//!
//! - [`semantic_digest`]: provider-neutral text, complete reasoning, canonical
//!   Tool semantics, and their order. Conversation binding references this
//!   digest. Provider origin, provider call IDs, and opaque replay state never
//!   enter it.
//! - [`replay_state_digest`]: provider origin, real provider call IDs, and
//!   part-local opaque replay state, in order.
//!
//! Binding digests are computed over the message as stored — the provider's
//! own message, reasoning parts included. They are never computed over a
//! cross-provider projection: `provider_projection::project_cross_provider`
//! drops reasoning parts and replay metadata by design (ADR 0005 §7), so its
//! output is a wire-facing derivative and digesting it would bind a Turn to a
//! message the provider never produced. #139-B must observe this when it wires
//! reservation to the production path.
//!
//! Every digest is SHA-256 over a domain separator, an explicit byte length,
//! and the canonical JSON bytes of the projected value (the same construction
//! ADR 0005 §2 fixes for `ProjectStoreKey`; §7 fixes it for argument digests).

use lancet_protocol::canonical_json;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::message::{MessageSemantic, NativeMessage, ToolCallState};

const ARGUMENTS_DOMAIN: &[u8] = b"lattice-tool-arguments-digest-v1\0";
const SEMANTIC_DOMAIN: &[u8] = b"lattice-message-semantic-digest-v1\0";
const REPLAY_DOMAIN: &[u8] = b"lattice-message-replay-state-digest-v1\0";

fn digest_with_domain(domain: &[u8], canonical_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((canonical_bytes.len() as u64).to_be_bytes());
    hasher.update(canonical_bytes);
    hasher.finalize().into()
}

/// SHA-256 over the domain separator, explicit byte length, and the canonical
/// JSON bytes of a Tool arguments object (ADR 0005 §7).
///
/// This digest validates a complete binding; it is not an identity. Only the
/// canonical object is digested, never the provider's textual spelling.
pub fn arguments_digest(arguments: &Value) -> [u8; 32] {
    digest_with_domain(ARGUMENTS_DOMAIN, canonical_json(arguments).as_bytes())
}

fn semantic_projection<S: ToolCallState>(message: &NativeMessage<S>) -> Value {
    let parts: Vec<Value> = message
        .parts
        .iter()
        .map(|part| match &part.semantic {
            MessageSemantic::Text { text } => json!({ "text": text }),
            MessageSemantic::Reasoning { text } => json!({ "reasoning": text }),
            MessageSemantic::ToolCall { call } => json!({
                "tool_call": {
                    "name": call.name(),
                    "arguments": call.arguments(),
                }
            }),
            MessageSemantic::ToolResult {
                invocation_id,
                name,
                outcome,
                content,
            } => json!({
                "tool_result": {
                    "invocation_id": invocation_id.to_string(),
                    "name": name,
                    "outcome": outcome.as_str(),
                    "content": content,
                }
            }),
        })
        .collect();
    json!({
        "role": message.role,
        "parts": parts,
    })
}

/// Provider-neutral semantic digest: role, ordered text, complete reasoning,
/// canonical Tool call/result semantics. Excludes provider origin, provider
/// call IDs, and every part-local replay state.
pub fn semantic_digest<S: ToolCallState>(message: &NativeMessage<S>) -> [u8; 32] {
    digest_with_domain(
        SEMANTIC_DOMAIN,
        canonical_json(&semantic_projection(message)).as_bytes(),
    )
}

fn replay_projection<S: ToolCallState>(message: &NativeMessage<S>) -> Value {
    let parts: Vec<Value> = message
        .parts
        .iter()
        .map(|part| {
            let provider_call_id = match &part.semantic {
                MessageSemantic::ToolCall { call } => call.provider_call_id(),
                _ => None,
            };
            json!({
                "provider_call_id": provider_call_id,
                "replay_state": part.replay_state.as_ref().map(|state| state.as_str()),
            })
        })
        .collect();
    json!({
        "provider_origin": message.provider_origin.map(|origin| origin.as_str()),
        "parts": parts,
    })
}

/// Replay-state digest: provider origin, ordered real provider call IDs, and
/// part-local opaque replay state. Same-provider replay requires this digest
/// to match; cross-provider projection must change it.
pub fn replay_state_digest<S: ToolCallState>(message: &NativeMessage<S>) -> [u8; 32] {
    digest_with_domain(
        REPLAY_DOMAIN,
        canonical_json(&replay_projection(message)).as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{MessagePart, MessageRole, OpaqueProviderState, ProviderOrigin};
    use crate::tool_call::UnboundToolCall;

    fn call(name: &str, id: Option<&str>, arguments: Value) -> UnboundToolCall {
        UnboundToolCall {
            provider_call_id: id.map(str::to_string),
            name: name.to_string(),
            arguments,
        }
    }

    fn message_with(parts: Vec<MessagePart<UnboundToolCall>>) -> NativeMessage<UnboundToolCall> {
        NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::Anthropic),
            parts,
        )
    }

    #[test]
    fn arguments_digest_is_canonical_and_stable() {
        // Key order and whitespace do not change the digest.
        let a: Value = serde_json::from_str(r#"{ "b" : 2, "a" : 1 }"#).expect("value");
        let b: Value = serde_json::from_str(r#"{"a":1,"b":2}"#).expect("value");
        assert_eq!(arguments_digest(&a), arguments_digest(&b));
        // Golden fixture pins the construction (domain separator + length +
        // canonical bytes) against silent drift.
        let golden: Value = serde_json::from_str(r#"{"path":"a.txt"}"#).expect("value");
        let expected = "5fd582703776c899a2ecb12f3d699bd7244dff1f443ef5ae3f0067bcf3ce279e";
        let digest = arguments_digest(&golden);
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, expected);
    }

    #[test]
    fn semantic_digest_ignores_provider_metadata() {
        let mut with_state = message_with(vec![
            MessagePart {
                semantic: MessageSemantic::Text {
                    text: "hello".to_string(),
                },
                replay_state: Some(OpaqueProviderState::new("sig-1").expect("state")),
            },
            MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: call("read_file", Some("call_1"), json!({"path": "a"})),
            }),
        ]);
        let baseline = semantic_digest(&with_state);

        // Stripping replay state and the provider call ID must not move the
        // semantic digest.
        for part in &mut with_state.parts {
            part.replay_state = None;
            if let MessageSemantic::ToolCall { call } = &mut part.semantic {
                call.provider_call_id = None;
            }
        }
        with_state.provider_origin = Some(ProviderOrigin::Gemini);
        assert_eq!(semantic_digest(&with_state), baseline);
    }

    #[test]
    fn semantic_digest_covers_semantics_and_order() {
        let one = message_with(vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: "a".to_string(),
        })]);
        let two = message_with(vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: "b".to_string(),
        })]);
        assert_ne!(semantic_digest(&one), semantic_digest(&two));

        // Order is semantic: [text, reasoning] differs from [reasoning, text].
        let forward = message_with(vec![
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "t".to_string(),
            }),
            MessagePart::semantic_only(MessageSemantic::Reasoning {
                text: "r".to_string(),
            }),
        ]);
        let backward = message_with(vec![
            MessagePart::semantic_only(MessageSemantic::Reasoning {
                text: "r".to_string(),
            }),
            MessagePart::semantic_only(MessageSemantic::Text {
                text: "t".to_string(),
            }),
        ]);
        assert_ne!(semantic_digest(&forward), semantic_digest(&backward));
    }

    #[test]
    fn replay_state_digest_covers_origin_ids_and_state() {
        let base = message_with(vec![MessagePart {
            semantic: MessageSemantic::ToolCall {
                call: call("read_file", Some("call_1"), json!({"path": "a"})),
            },
            replay_state: Some(OpaqueProviderState::new("sig").expect("state")),
        }]);
        let baseline = replay_state_digest(&base);

        // Changing origin moves the digest.
        let mut other_origin = base.clone();
        other_origin.provider_origin = Some(ProviderOrigin::OpenAiCompatible);
        assert_ne!(replay_state_digest(&other_origin), baseline);

        // Dropping the replay state moves the digest.
        let mut stripped = base.clone();
        for part in &mut stripped.parts {
            part.replay_state = None;
        }
        assert_ne!(replay_state_digest(&stripped), baseline);

        // Changing the provider call ID moves the digest.
        let mut other_id = base.clone();
        for part in &mut other_id.parts {
            if let MessageSemantic::ToolCall { call } = &mut part.semantic {
                call.provider_call_id = Some("call_2".to_string());
            }
        }
        assert_ne!(replay_state_digest(&other_id), baseline);

        // Semantic changes that leave replay metadata identical do not move
        // it (arguments spelling is semantic, not replay state — but the
        // canonical arguments also live outside this digest).
        let mut same_replay = base.clone();
        for part in &mut same_replay.parts {
            part.semantic = MessageSemantic::Text {
                text: "different".to_string(),
            };
        }
        // Text parts carry no provider_call_id; replay state is unchanged.
        assert_eq!(replay_state_digest(&same_replay), {
            let mut expected_parts = base.clone();
            for part in &mut expected_parts.parts {
                part.semantic = MessageSemantic::Text {
                    text: "different".to_string(),
                };
            }
            replay_state_digest(&expected_parts)
        });
    }
}
