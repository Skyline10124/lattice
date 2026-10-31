// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Ordered provider-neutral message model with ToolCall typestates
//! (ADR 0005 §7).
//!
//! The ordered part vector is durable conversation order: a part's index in
//! `parts` is its `part_ordinal`. Replay state is part-local and opaque:
//! Text（含空文本）、Reasoning、ToolCall 可带 provider 所需的片段回放状态；
//! 即使片段正文为空，也不因过滤丢弃其回放状态。
//!
//! 未绑定调用只用于请求完成到登记之前；持久消息使用 BoundToolCall，
//! 结果只保存在 StepContent 的结构化结果中，不保存 provider 派生文字。

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::DeserializeOwned};
use thiserror::Error;

/// Maximum byte length of one part-local opaque replay state (64 KiB).
pub const MAX_OPAQUE_REPLAY_STATE_BYTES: usize = 64 * 1024;

/// provider 标识的最大字节数。
pub const MAX_PROVIDER_ID_BYTES: usize = 64;

/// provider 标识不符合语法规则的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProviderIdError {
    /// 标识为空。
    #[error("provider id must not be empty")]
    Empty,
    /// 标识超过字节上限。
    #[error("provider id must be at most {MAX_PROVIDER_ID_BYTES} bytes")]
    TooLong,
    /// 首字符不是 ASCII 小写字母或数字。
    #[error("provider id must start with [a-z0-9]")]
    InvalidStart,
    /// 标识含 ASCII 小写字母、数字和连字符以外的字符。
    #[error("provider id must contain only [a-z0-9-]")]
    InvalidCharacter,
}

/// 校验 provider 标识的长度、首字符与 ASCII 字符范围。
pub fn validate_provider_id(id: &str) -> Result<(), ProviderIdError> {
    let Some(first) = id.as_bytes().first() else {
        return Err(ProviderIdError::Empty);
    };
    if id.len() > MAX_PROVIDER_ID_BYTES {
        return Err(ProviderIdError::TooLong);
    }
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(ProviderIdError::InvalidStart);
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(ProviderIdError::InvalidCharacter);
    }
    Ok(())
}

/// 模型请求与回放材料使用的协议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    ChatCompletions,
    Responses,
    #[serde(rename = "anthropic-messages")]
    Messages,
    GenerateContent,
    Interactions,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat-completions",
            Self::Responses => "responses",
            Self::Messages => "anthropic-messages",
            Self::GenerateContent => "generate-content",
            Self::Interactions => "interactions",
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一次请求的实际来源；不含模型、endpoint、认证或 features。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayOrigin {
    pub protocol: Protocol,
    pub provider_id: String,
}

impl ReplayOrigin {
    /// 完整来源相同才保留 reasoning 与逐片段回放状态。
    pub fn same_provider(&self, other: &Self) -> bool {
        self == other
    }

    /// 协议相同即可保留 provider call ID。
    pub fn same_protocol(&self, other: &Self) -> bool {
        self.protocol == other.protocol
    }
}

/// 逐片段保存签名、加密推理等协议回放材料，只有产生它的协议 codec 解释其编码。
/// 其他层按不透明字符串保存，同完整来源回放，不跨来源投影或补造。
/// 编码后的长度不超过 [`MAX_OPAQUE_REPLAY_STATE_BYTES`]。
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
    /// 保存协议 codec 给出的状态编码，并核对编码后的长度上限。
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

    /// 原样返回协议 codec 的状态编码。
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
    /// 跨协议投影时清除 provider call ID，其他字段保持原样。
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
/// This is the only content available to cross-provider projection.
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
    /// 产生这条助手消息的完整来源；系统和用户消息没有来源。
    #[serde(default)]
    pub provider_origin: Option<ReplayOrigin>,
    /// Ordered parts; a part's index is its `part_ordinal`.
    pub parts: Vec<MessagePart<S>>,
}

impl<S: ToolCallState> NativeMessage<S> {
    /// Construct a message from its parts.
    pub fn new(
        role: MessageRole,
        provider_origin: Option<ReplayOrigin>,
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

    #[test]
    fn provider_ids_enforce_alphabet_and_inclusive_byte_limit() {
        for id in ["a".into(), "0".into(), "a-b-9-".into(), "a".repeat(64)] {
            crate::validate_provider_id(&id).unwrap();
        }
        for (id, error) in [
            ("".into(), ProviderIdError::Empty),
            ("-a".into(), ProviderIdError::InvalidStart),
            ("A".into(), ProviderIdError::InvalidStart),
            ("aB".into(), ProviderIdError::InvalidCharacter),
            ("a_b".into(), ProviderIdError::InvalidCharacter),
            ("a.b".into(), ProviderIdError::InvalidCharacter),
            ("a b".into(), ProviderIdError::InvalidCharacter),
            ("é".into(), ProviderIdError::InvalidStart),
            ("aé".into(), ProviderIdError::InvalidCharacter),
            ("a/".into(), ProviderIdError::InvalidCharacter),
            ("a".repeat(65), ProviderIdError::TooLong),
        ] {
            assert_eq!(crate::validate_provider_id(&id), Err(error), "{id:?}");
        }
    }

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
            Some(ReplayOrigin {
                protocol: Protocol::GenerateContent,
                provider_id: "test".into(),
            }),
            vec![part],
        );
        assert_eq!(message.parts.len(), 1);
        let json = serde_json::to_string(&message).expect("serialize");
        let back: NativeMessage<UnboundToolCall> =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(message, back);
    }

    #[test]
    fn text_and_reasoning_serde_roundtrip() {
        let message = NativeMessage::new(
            MessageRole::Assistant,
            Some(ReplayOrigin {
                protocol: Protocol::Messages,
                provider_id: "test".into(),
            }),
            vec![
                text_part("hello"),
                MessagePart::semantic_only(MessageSemantic::Reasoning {
                    text: "thinking".to_string(),
                }),
            ],
        );
        let json = serde_json::to_string(&message).expect("serialize");
        let back: NativeMessage<UnboundToolCall> =
            serde_json::from_str(&json).expect("deserialize");
        assert_eq!(message, back);
    }
}
