// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 强类型上下文输入、受限替换文本与批量编辑原语（ADR 0014 第 2～3 节）。

use std::fmt;

use lattice_conversation::{
    BoundToolCall, ImageRef, InvocationId, MAX_TOOL_CONTENT_BYTES, MAX_USER_TEXT_BYTES,
    MessageRole, MessageSemantic, NativeMessage, NodeId, UserInputError, canonical_json,
    normalize_user_message,
};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::channel::Channel;

/// 将既有 [`NativeMessage`] 核验为规范化用户消息失败的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum InvalidUserMessageError {
    /// 消息角色不是 `User`。
    #[error("上下文输入消息角色必须为 user")]
    UnexpectedRole {
        /// 实际消息角色。
        actual: MessageRole,
    },
    /// 用户输入消息携带了 `provider_origin`。
    #[error("上下文输入消息不能携带 provider_origin")]
    UnexpectedProviderOrigin,
    /// 用户输入消息片段携带了 `replay_state`。
    #[error("第 {part} 个用户输入片段不能携带 replay_state")]
    UnexpectedReplayState {
        /// 从 1 开始的片段序号。
        part: usize,
    },
    /// 片段内容或上限不满足单轮用户输入规范。
    #[error(transparent)]
    InvalidInput(#[from] UserInputError),
    /// 片段未处于规范化形态（例如包含空文本或未合并的相邻文本片段）。
    #[error("用户输入消息未处于规范化形态（包含空文本或未合并的相邻文本片段）")]
    NotNormalized,
}

/// 经 `normalize_user_message` 核验的规范化用户多模态消息。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NormalizedUserMessage(NativeMessage<BoundToolCall>);

impl NormalizedUserMessage {
    /// 对输入片段执行单轮上限核验与保序规范化。
    pub fn from_parts(parts: Vec<MessageSemantic<BoundToolCall>>) -> Result<Self, UserInputError> {
        Ok(Self(normalize_user_message(parts)?))
    }

    /// 严格核验既有 [`NativeMessage`] 已处于规范化用户消息形态。
    pub fn try_from_message(
        message: NativeMessage<BoundToolCall>,
    ) -> Result<Self, InvalidUserMessageError> {
        if message.role != MessageRole::User {
            return Err(InvalidUserMessageError::UnexpectedRole {
                actual: message.role,
            });
        }
        if message.provider_origin.is_some() {
            return Err(InvalidUserMessageError::UnexpectedProviderOrigin);
        }
        for (index, part) in message.parts.iter().enumerate() {
            if part.replay_state.is_some() {
                return Err(InvalidUserMessageError::UnexpectedReplayState { part: index + 1 });
            }
        }
        let semantics: Vec<MessageSemantic<BoundToolCall>> = message
            .parts
            .iter()
            .map(|part| part.semantic.clone())
            .collect();
        let normalized = normalize_user_message(semantics)?;
        if normalized != message {
            return Err(InvalidUserMessageError::NotNormalized);
        }
        Ok(Self(normalized))
    }

    /// 返回内部规范化 [`NativeMessage`] 的只读引用。
    pub fn as_message(&self) -> &NativeMessage<BoundToolCall> {
        &self.0
    }

    /// 消费包装并取出内部规范化 [`NativeMessage`]。
    pub fn into_inner(self) -> NativeMessage<BoundToolCall> {
        self.0
    }
}

impl AsRef<NativeMessage<BoundToolCall>> for NormalizedUserMessage {
    fn as_ref(&self) -> &NativeMessage<BoundToolCall> {
        self.as_message()
    }
}

impl TryFrom<NativeMessage<BoundToolCall>> for NormalizedUserMessage {
    type Error = InvalidUserMessageError;

    fn try_from(message: NativeMessage<BoundToolCall>) -> Result<Self, Self::Error> {
        Self::try_from_message(message)
    }
}

impl<'de> Deserialize<'de> for NormalizedUserMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let message = NativeMessage::<BoundToolCall>::deserialize(deserializer)?;
        Self::try_from_message(message).map_err(serde::de::Error::custom)
    }
}

/// 替换文本违反非空或字节上限的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum TextBoundError {
    /// 替换文本为空字符串。
    #[error("替换文本不能为空")]
    Empty,
    /// 替换文本字节长度超过允许上限。
    #[error("替换文本为 {actual} 字节，不能超过 {limit} 字节")]
    TooLarge {
        /// 允许的最大字节长度。
        limit: usize,
        /// 实际字节长度（`ContextText` 按 UTF-8 原文计，`ToolResultText` 按规范 JSON 编码计）。
        actual: usize,
    },
}

/// 供 `ReplaceStep` 与 `ReplaceImage` 使用的非空受限文本（上限 `MAX_USER_TEXT_BYTES`，保留空白字符不裁剪）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ContextText(String);

impl ContextText {
    /// 校验文本非空且 UTF-8 字节长度不超过 [`MAX_USER_TEXT_BYTES`]。
    pub fn new(text: impl Into<String>) -> Result<Self, TextBoundError> {
        let text = text.into();
        if text.is_empty() {
            return Err(TextBoundError::Empty);
        }
        if text.len() > MAX_USER_TEXT_BYTES {
            return Err(TextBoundError::TooLarge {
                limit: MAX_USER_TEXT_BYTES,
                actual: text.len(),
            });
        }
        Ok(Self(text))
    }

    /// 返回文本切片。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费包装并取出内部字符串。
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ContextText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ContextText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for ContextText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// 供 `ReplaceToolResult` 使用的非空受限文本（按规范 JSON 字符串编码字节计，上限 `MAX_TOOL_CONTENT_BYTES` = 65,536 字节；
/// `Succeeded` 与 `Failed` 共享同一标尺，构造通过即保证协议投影零截断）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ToolResultText(String);

impl ToolResultText {
    /// 校验文本非空且其规范 JSON 字符串编码长度不超过 [`MAX_TOOL_CONTENT_BYTES`]。
    pub fn new(text: impl Into<String>) -> Result<Self, TextBoundError> {
        let text = text.into();
        if text.is_empty() {
            return Err(TextBoundError::Empty);
        }
        let value = serde_json::Value::String(text);
        let encoded_len = canonical_json(&value).len();
        if encoded_len > MAX_TOOL_CONTENT_BYTES {
            return Err(TextBoundError::TooLarge {
                limit: MAX_TOOL_CONTENT_BYTES,
                actual: encoded_len,
            });
        }
        let serde_json::Value::String(text) = value else {
            unreachable!("刚刚构造的 Value::String 必为字符串变体");
        };
        Ok(Self(text))
    }

    /// 返回文本切片。
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 消费包装并取出内部字符串。
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ToolResultText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ToolResultText {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for ToolResultText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

/// 一次通用多模态上下文输入，绑定来源通道与规范化用户消息。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextInput {
    channel: Channel,
    message: NormalizedUserMessage,
}

impl ContextInput {
    /// 按单轮上限核验并规范化输入片段，构造 [`ContextInput`]。
    pub fn new(
        channel: Channel,
        parts: Vec<MessageSemantic<BoundToolCall>>,
    ) -> Result<Self, UserInputError> {
        Ok(Self {
            channel,
            message: NormalizedUserMessage::from_parts(parts)?,
        })
    }

    /// 校验既有 [`NativeMessage`] 已处于规范化形态后构造 [`ContextInput`]。
    pub fn from_message(
        channel: Channel,
        message: NativeMessage<BoundToolCall>,
    ) -> Result<Self, InvalidUserMessageError> {
        Ok(Self {
            channel,
            message: NormalizedUserMessage::try_from_message(message)?,
        })
    }

    /// 返回来源通道标识。
    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// 返回规范化用户消息。
    pub fn message(&self) -> &NativeMessage<BoundToolCall> {
        self.message.as_message()
    }

    /// 拆分为来源通道与规范化用户消息。
    pub fn into_parts(self) -> (Channel, NativeMessage<BoundToolCall>) {
        (self.channel, self.message.into_inner())
    }
}

/// 构造或反序列化空 [`ContextOp`] 时的错误。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
#[error("上下文操作 ContextOp 至少包含一条编辑")]
pub struct EmptyContextOpError;

/// 一次不可变的批量上下文操作记录，编辑列表非空且按顺序生效。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ContextOpWire")]
pub struct ContextOp {
    channel: Channel,
    edits: Vec<ContextEdit>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextOpWire {
    channel: Channel,
    edits: Vec<ContextEdit>,
}

impl TryFrom<ContextOpWire> for ContextOp {
    type Error = EmptyContextOpError;

    fn try_from(wire: ContextOpWire) -> Result<Self, Self::Error> {
        Self::new(wire.channel, wire.edits)
    }
}

impl ContextOp {
    /// 构造非空批量上下文操作。
    pub fn new(channel: Channel, edits: Vec<ContextEdit>) -> Result<Self, EmptyContextOpError> {
        if edits.is_empty() {
            return Err(EmptyContextOpError);
        }
        Ok(Self { channel, edits })
    }

    /// 返回发起本次操作的来源通道。
    pub fn channel(&self) -> &Channel {
        &self.channel
    }

    /// 返回按生效顺序排列的编辑列表。
    pub fn edits(&self) -> &[ContextEdit] {
        &self.edits
    }

    /// 拆分为来源通道与编辑列表。
    pub fn into_parts(self) -> (Channel, Vec<ContextEdit>) {
        (self.channel, self.edits)
    }
}

/// 单条强类型上下文编辑原语（结构上禁止非法目标与替换载荷组合）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextEdit {
    /// 将整条 `ContextInput` 或整步 `Step`（回复与全部工具调用/结果同进同退）移出模型上下文。
    OmitNode { node: NodeId },
    /// 将整条 `ContextInput` 替换为新的规范化图文消息（仅作用于 `ContextInput` 节点）。
    ReplaceInput {
        node: NodeId,
        message: NormalizedUserMessage,
    },
    /// 将整步 `Step` 替换为一段非空纯助手结论文本，丢弃中间工具调用与推理（仅作用于已有回复的 `Step` 节点）。
    ReplaceStep { node: NodeId, text: ContextText },
    /// 成对移出单次工具调用（`ToolCall` 片段与对应 `StoredToolResult` 同时移出，不留悬空调用）。
    OmitInvocation { invocation: InvocationId },
    /// 仅替换单次工具调用的 `StoredToolResult` 文本载荷（保留原 `Succeeded`/`Failed` 结局、`ToolCall` 配对
    /// 以及 `assistant` 消息的同源回放状态 `provider_origin` 与逐片段 `replay_state`；
    /// `Succeeded` 与 `Failed` 共享 `MAX_TOOL_CONTENT_BYTES` 规范 JSON 字符串上限，`NotExecuted` 与 `Indeterminate` 不可替换）。
    ReplaceToolResult {
        invocation: InvocationId,
        text: ToolResultText,
    },
    /// 移出指定 `Step` 回复中的全部 `Reasoning` 片段（保留正文与 `ToolCall`）。
    StripReasoning { node: NodeId },
    /// 将指定 `ContextInput` 中匹配 `ImageRef` 的图片片段移出（`None`）或替换为短文本占位（`Some`）。
    ReplaceImage {
        node: NodeId,
        image: ImageRef,
        text: Option<ContextText>,
    },
}

impl ContextEdit {
    /// 构造整节点移出编辑。
    pub fn omit_node(node: NodeId) -> Self {
        Self::OmitNode { node }
    }

    /// 经单轮规范化构造替换 `ContextInput` 编辑。
    pub fn replace_input(
        node: NodeId,
        parts: Vec<MessageSemantic<BoundToolCall>>,
    ) -> Result<Self, UserInputError> {
        Ok(Self::ReplaceInput {
            node,
            message: NormalizedUserMessage::from_parts(parts)?,
        })
    }

    /// 核验既有规范化消息并构造替换 `ContextInput` 编辑。
    pub fn replace_input_message(
        node: NodeId,
        message: NativeMessage<BoundToolCall>,
    ) -> Result<Self, InvalidUserMessageError> {
        Ok(Self::ReplaceInput {
            node,
            message: NormalizedUserMessage::try_from_message(message)?,
        })
    }

    /// 构造整步替换 `Step` 编辑。
    pub fn replace_step(node: NodeId, text: impl Into<String>) -> Result<Self, TextBoundError> {
        Ok(Self::ReplaceStep {
            node,
            text: ContextText::new(text)?,
        })
    }

    /// 构造成对移出单次工具调用编辑。
    pub fn omit_invocation(invocation: InvocationId) -> Self {
        Self::OmitInvocation { invocation }
    }

    /// 构造替换单次工具调用结果载荷编辑。
    pub fn replace_tool_result(
        invocation: InvocationId,
        text: impl Into<String>,
    ) -> Result<Self, TextBoundError> {
        Ok(Self::ReplaceToolResult {
            invocation,
            text: ToolResultText::new(text)?,
        })
    }

    /// 构造剥离指定 `Step` 全部 `Reasoning` 片段编辑。
    pub fn strip_reasoning(node: NodeId) -> Self {
        Self::StripReasoning { node }
    }

    /// 构造移出或替换指定 `ContextInput` 中匹配图片的编辑。
    pub fn replace_image(
        node: NodeId,
        image: ImageRef,
        text: Option<String>,
    ) -> Result<Self, TextBoundError> {
        let text = match text {
            Some(raw) => Some(ContextText::new(raw)?),
            None => None,
        };
        Ok(Self::ReplaceImage { node, image, text })
    }
}
