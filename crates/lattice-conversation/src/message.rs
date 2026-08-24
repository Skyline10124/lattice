// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Ordered provider-neutral message model with ToolCall typestates
//! (ADR 0005 §7).
//!
//! The ordered part vector is durable conversation order: a part's index in
//! `parts` is its `part_ordinal`. Replay state is part-local and opaque:
//! Text (including empty Text), Reasoning, ToolCall, and ToolResult parts may
//! all carry it when the provider contract requires it, and a part that
//! carries replay state is never filtered out, even when its semantic payload
//! is empty.
//!
//! `NativeMessage<UnboundToolCall>` only exists before binding (a PendingTurn
//! `MessageComplete`/`BindingPlanned` stage); completed conversation stores
//! `NativeMessage<BoundToolCall>`. There is no `Option<InvocationId>`
//! typestate shortcut.

use std::fmt;

use lancet_protocol::InvocationId;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use thiserror::Error;

use crate::outcome::ToolResultOutcome;

/// Maximum byte length of one part-local opaque replay state (64 KiB).
pub const MAX_OPAQUE_REPLAY_STATE_BYTES: usize = 64 * 1024;

/// The provider wire family a message or replay state originates from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOrigin {
    /// OpenAI-compatible chat-completions wire.
    OpenAiCompatible,
    /// Anthropic messages wire.
    Anthropic,
    /// Gemini generateContent wire.
    Gemini,
}

impl ProviderOrigin {
    /// Stable wire slug used in digests and replay metadata.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }
}

impl fmt::Display for ProviderOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A part-local opaque provider replay state (signature, thought state, or
/// equivalent protocol-required token).
///
/// The value is preserved verbatim for same-provider replay and never
/// interpreted, synthesized, or projected across providers. Bounded to
/// [`MAX_OPAQUE_REPLAY_STATE_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OpaqueProviderState(String);

/// Error constructing or deserializing an [`OpaqueProviderState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OpaqueReplayStateError {
    /// The value exceeds the protocol bound.
    #[error("opaque replay state too large: limit {limit} bytes, got {actual}")]
    TooLarge {
        /// Protocol limit in bytes.
        limit: usize,
        /// Actual byte length.
        actual: usize,
    },
}

impl OpaqueProviderState {
    /// Wrap a provider-supplied value verbatim, enforcing the protocol bound.
    pub fn new(value: impl Into<String>) -> Result<Self, OpaqueReplayStateError> {
        let value = value.into();
        if value.len() > MAX_OPAQUE_REPLAY_STATE_BYTES {
            return Err(OpaqueReplayStateError::TooLarge {
                limit: MAX_OPAQUE_REPLAY_STATE_BYTES,
                actual: value.len(),
            });
        }
        Ok(Self(value))
    }

    /// The verbatim provider-supplied value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OpaqueProviderState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for OpaqueProviderState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OpaqueProviderState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// Marker for the ToolCall binding state of a message: sealed so only
/// [`crate::UnboundToolCall`] and [`crate::BoundToolCall`] can instantiate it.
pub trait ToolCallState:
    sealed::Sealed + Clone + fmt::Debug + PartialEq + Serialize + DeserializeOwned
{
    /// The Tool method name.
    fn name(&self) -> &str;
    /// The canonical arguments object.
    fn arguments(&self) -> &serde_json::Value;
    /// The provider-native call ID, when the provider supplied one.
    fn provider_call_id(&self) -> Option<&str>;
    /// Drop the provider-native call ID (cross-provider projection only;
    /// ADR 0005 §7). Every other field is untouched.
    fn clear_provider_call_id(&mut self);
}

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// The role of a conversation participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    /// System prompt content.
    System,
    /// User input.
    User,
    /// Provider-produced assistant message.
    Assistant,
    /// Tool result closure message.
    Tool,
}

/// The provider-neutral semantic payload of one part.
///
/// This is the only content that enters the semantic digest and the only
/// content available to cross-provider projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "snake_case",
    bound(serialize = "S: Serialize", deserialize = "S: DeserializeOwned")
)]
pub enum MessageSemantic<S: ToolCallState> {
    /// Visible assistant/user text.
    Text {
        /// The text payload.
        text: String,
    },
    /// Complete provider reasoning. Stored and digested, but never replayed
    /// across providers and never interpreted by LANCET.
    Reasoning {
        /// The reasoning payload.
        text: String,
    },
    /// A Tool call requested by the model.
    ToolCall {
        /// The call, in the message's binding state.
        call: S,
    },
    /// A provider-neutral Tool result closing a bound ToolCall.
    ToolResult {
        /// The invocation this result closes.
        invocation_id: InvocationId,
        /// The Tool method name.
        name: String,
        /// The execution outcome closure.
        outcome: ToolResultOutcome,
        /// Model-facing result content; `None` when the result material is
        /// unavailable (the outcome remains the true execution outcome).
        content: Option<String>,
    },
}

/// One ordered part of a message: neutral semantics plus optional part-local
/// opaque replay state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "S: Serialize", deserialize = "S: DeserializeOwned"))]
pub struct MessagePart<S: ToolCallState> {
    /// The provider-neutral semantic payload.
    pub semantic: MessageSemantic<S>,
    /// Part-local opaque replay state, preserved verbatim for same-provider
    /// replay and excluded from every semantic view.
    #[serde(default)]
    pub replay_state: Option<OpaqueProviderState>,
}

impl<S: ToolCallState> MessagePart<S> {
    /// A part with no replay state.
    pub fn semantic_only(semantic: MessageSemantic<S>) -> Self {
        Self {
            semantic,
            replay_state: None,
        }
    }
}

/// An ordered provider-neutral message. The binding state parameter makes
/// "completed conversation is fully bound" a type-level fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "S: Serialize", deserialize = "S: DeserializeOwned"))]
pub struct NativeMessage<S: ToolCallState> {
    /// Participant role.
    pub role: MessageRole,
    /// Provider wire family that produced this message, when provider-produced.
    #[serde(default)]
    pub provider_origin: Option<ProviderOrigin>,
    /// Ordered parts; a part's index is its `part_ordinal`.
    pub parts: Vec<MessagePart<S>>,
}

impl<S: ToolCallState> NativeMessage<S> {
    /// Construct a message from its parts.
    pub fn new(
        role: MessageRole,
        provider_origin: Option<ProviderOrigin>,
        parts: Vec<MessagePart<S>>,
    ) -> Self {
        Self {
            role,
            provider_origin,
            parts,
        }
    }

    /// Iterate over ToolCall parts with their part ordinals.
    pub fn tool_calls(&self) -> impl Iterator<Item = (usize, &S)> {
        self.parts
            .iter()
            .enumerate()
            .filter_map(|(ordinal, part)| match &part.semantic {
                MessageSemantic::ToolCall { call } => Some((ordinal, call)),
                _ => None,
            })
    }
}

impl NativeMessage<crate::UnboundToolCall> {
    /// Convert a complete zero-Tool message into the durable bound typestate.
    pub fn bind_zero_tool(self) -> Result<NativeMessage<crate::BoundToolCall>, Self> {
        if self.tool_calls().next().is_some() {
            return Err(self);
        }
        let parts = self
            .parts
            .into_iter()
            .map(|part| MessagePart {
                semantic: match part.semantic {
                    MessageSemantic::Text { text } => MessageSemantic::Text { text },
                    MessageSemantic::Reasoning { text } => MessageSemantic::Reasoning { text },
                    MessageSemantic::ToolResult {
                        invocation_id,
                        name,
                        outcome,
                        content,
                    } => MessageSemantic::ToolResult {
                        invocation_id,
                        name,
                        outcome,
                        content,
                    },
                    MessageSemantic::ToolCall { .. } => unreachable!("checked above"),
                },
                replay_state: part.replay_state,
            })
            .collect();
        Ok(NativeMessage::new(self.role, self.provider_origin, parts))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call::UnboundToolCall;

    fn text_part(text: &str) -> MessagePart<UnboundToolCall> {
        MessagePart::semantic_only(MessageSemantic::Text {
            text: text.to_string(),
        })
    }

    #[test]
    fn opaque_state_enforces_bound() {
        let ok = OpaqueProviderState::new("sig").expect("small state");
        assert_eq!(ok.as_str(), "sig");
        let oversized = "x".repeat(MAX_OPAQUE_REPLAY_STATE_BYTES + 1);
        assert!(matches!(
            OpaqueProviderState::new(oversized),
            Err(OpaqueReplayStateError::TooLarge { .. })
        ));
        // Exact boundary is accepted.
        let exact = "x".repeat(MAX_OPAQUE_REPLAY_STATE_BYTES);
        assert!(OpaqueProviderState::new(exact).is_ok());
    }

    #[test]
    fn opaque_state_deserialize_enforces_bound() {
        let oversized = "x".repeat(MAX_OPAQUE_REPLAY_STATE_BYTES + 1);
        let json = serde_json::to_string(&oversized).expect("serialize");
        assert!(serde_json::from_str::<OpaqueProviderState>(&json).is_err());
    }

    #[test]
    fn empty_text_part_with_replay_state_is_preserved() {
        // Grill R7: a part carrying replay state must not be filtered out even
        // when its semantic payload is empty (Gemini thought signatures sit on
        // otherwise-empty text parts).
        let part = MessagePart::<UnboundToolCall> {
            semantic: MessageSemantic::Text {
                text: String::new(),
            },
            replay_state: Some(OpaqueProviderState::new("thought-sig").expect("state")),
        };
        let message = NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::Gemini),
            vec![part],
        );
        assert_eq!(message.parts.len(), 1);
        let json = serde_json::to_string(&message).expect("serialize");
        let back: NativeMessage<UnboundToolCall> =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(message, back);
    }

    #[test]
    fn message_serde_roundtrip_with_all_part_kinds() {
        let invocation =
            InvocationId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("invocation id");
        let message = NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::Anthropic),
            vec![
                text_part("hello"),
                MessagePart::semantic_only(MessageSemantic::Reasoning {
                    text: "thinking".to_string(),
                }),
                MessagePart::semantic_only(MessageSemantic::ToolResult {
                    invocation_id: invocation,
                    name: "observe.workspace".to_string(),
                    outcome: ToolResultOutcome::Succeeded,
                    content: Some("{}".to_string()),
                }),
            ],
        );
        let json = serde_json::to_string(&message).expect("serialize");
        let back: NativeMessage<UnboundToolCall> =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(message, back);
    }
}
