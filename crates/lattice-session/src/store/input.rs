// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 受检的单轮用户输入片段与规范化载体。

use std::collections::BTreeMap;
use std::fmt;

use lattice_context::{Channel, ContextEdit, ContextInput};
use lattice_conversation::{
    BoundToolCall, ImageBytesError, ImageRef, MAX_IMAGE_BYTES, MAX_USER_IMAGE_TOTAL_BYTES,
    MAX_USER_IMAGES_PER_TURN, MAX_USER_TEXT_BYTES, MessageSemantic, NativeMessage, NodeId,
    UserInputError, normalize_user_message,
};

use super::{Result as StoreResult, invalid};

/// 接受边界的原始输入片段。
#[derive(Clone, PartialEq, Eq)]
pub enum TurnInputPart {
    Text { text: String },
    Image { bytes: Vec<u8> },
}

impl fmt::Debug for TurnInputPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { text } => f
                .debug_struct("Text")
                .field("byte_length", &text.len())
                .finish(),
            Self::Image { bytes } => f
                .debug_struct("Image")
                .field("byte_length", &bytes.len())
                .finish(),
        }
    }
}

/// 已规范化、图片字节与引用一一对应的单轮用户输入。
#[derive(Clone, PartialEq)]
pub struct TurnInput {
    message: NativeMessage<BoundToolCall>,
    blobs: BTreeMap<[u8; 32], Vec<u8>>,
}

impl TurnInput {
    /// 按片段顺序校验图片字节并执行保序规范化；同一摘要的图片字节只保留一份。
    pub fn new(parts: Vec<TurnInputPart>) -> Result<Self, UserInputError> {
        let mut semantic_parts = Vec::with_capacity(parts.len());
        let mut blobs = BTreeMap::new();
        let mut image_ordinal = 0usize;
        for part in parts {
            match part {
                TurnInputPart::Text { text } => {
                    semantic_parts.push(MessageSemantic::Text { text });
                }
                TurnInputPart::Image { bytes } => {
                    image_ordinal += 1;
                    let image = ImageRef::from_bytes(&bytes).map_err(|err| match err {
                        ImageBytesError::InvalidLength { actual } => {
                            UserInputError::InvalidImageLength {
                                image: image_ordinal,
                                actual,
                            }
                        }
                        ImageBytesError::UnsupportedFormat => {
                            UserInputError::UnsupportedImageFormat {
                                image: image_ordinal,
                            }
                        }
                    })?;
                    blobs.entry(image.sha256).or_insert(bytes);
                    semantic_parts.push(MessageSemantic::Image { image });
                }
            }
        }
        let message = normalize_user_message(semantic_parts)?;
        Ok(Self { message, blobs })
    }

    /// 只含一个文本片段的输入。
    pub fn text(text: String) -> Result<Self, UserInputError> {
        Self::new(vec![TurnInputPart::Text { text }])
    }

    /// 规范化后的用户消息。
    pub fn message(&self) -> &NativeMessage<BoundToolCall> {
        &self.message
    }

    /// 按值拆出规范化消息与去重后的图片字节映射，供接受事务写入。
    pub(super) fn into_parts(self) -> (NativeMessage<BoundToolCall>, BTreeMap<[u8; 32], Vec<u8>>) {
        (self.message, self.blobs)
    }

    /// 绑定来源通道并拆出 [`ContextInput`] 与去重后的图片字节映射。
    pub fn into_context_input(
        self,
        channel: Channel,
    ) -> (ContextInput, BTreeMap<[u8; 32], Vec<u8>>) {
        let (message, blobs) = self.into_parts();
        let input = ContextInput::from_message(channel, message)
            .expect("TurnInput 构造时已通过单轮用户输入规范化校验");
        (input, blobs)
    }

    /// 构造替换指定 `Input` 节点的 [`ContextEdit::ReplaceInput`] 及随附的去重图片字节映射。
    pub fn into_replace_input(self, node: NodeId) -> (ContextEdit, BTreeMap<[u8; 32], Vec<u8>>) {
        let (message, blobs) = self.into_parts();
        let edit = ContextEdit::replace_input_message(node, message)
            .expect("TurnInput 构造时已通过单轮用户输入规范化校验");
        (edit, blobs)
    }
}

/// 跨整批 `1..=N` 条输入累计核验单轮用户输入上限，复用 [`UserInputError`] 错误文本。
pub fn validate_turn_input_batch(inputs: &[(Channel, TurnInput)]) -> StoreResult<()> {
    if inputs.is_empty() {
        return Err(invalid("inputs", &UserInputError::Empty.to_string()));
    }

    let mut text_bytes: usize = 0;
    for (_, item) in inputs {
        for part in &item.message.parts {
            if let MessageSemantic::Text { text } = &part.semantic {
                text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| {
                    invalid(
                        "inputs",
                        &UserInputError::TextTooLarge { actual: usize::MAX }.to_string(),
                    )
                })?;
            }
        }
    }
    if text_bytes > MAX_USER_TEXT_BYTES {
        return Err(invalid(
            "inputs",
            &UserInputError::TextTooLarge { actual: text_bytes }.to_string(),
        ));
    }

    let max_image_bytes = u64::try_from(MAX_IMAGE_BYTES).expect("MAX_IMAGE_BYTES 可转换为 u64");
    let max_total_bytes =
        u64::try_from(MAX_USER_IMAGE_TOTAL_BYTES).expect("MAX_USER_IMAGE_TOTAL_BYTES 可转换为 u64");
    let mut image_count: usize = 0;
    let mut image_total_bytes: u64 = 0;
    for (_, item) in inputs {
        for part in &item.message.parts {
            if let MessageSemantic::Image { image } = &part.semantic {
                image_count = image_count.checked_add(1).ok_or_else(|| {
                    invalid(
                        "inputs",
                        &UserInputError::TooManyImages { actual: usize::MAX }.to_string(),
                    )
                })?;
                if image.byte_length == 0 || image.byte_length > max_image_bytes {
                    return Err(invalid(
                        "inputs",
                        &UserInputError::InvalidImageLength {
                            image: image_count,
                            actual: image.byte_length,
                        }
                        .to_string(),
                    ));
                }
                image_total_bytes = image_total_bytes
                    .checked_add(image.byte_length)
                    .ok_or_else(|| {
                        invalid(
                            "inputs",
                            &UserInputError::ImageTotalTooLarge { actual: u64::MAX }.to_string(),
                        )
                    })?;
            }
        }
    }
    if image_count > MAX_USER_IMAGES_PER_TURN {
        return Err(invalid(
            "inputs",
            &UserInputError::TooManyImages {
                actual: image_count,
            }
            .to_string(),
        ));
    }
    if image_total_bytes > max_total_bytes {
        return Err(invalid(
            "inputs",
            &UserInputError::ImageTotalTooLarge {
                actual: image_total_bytes,
            }
            .to_string(),
        ));
    }
    Ok(())
}

impl fmt::Debug for TurnInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct DebugPart<'a>(&'a MessageSemantic<BoundToolCall>);
        impl fmt::Debug for DebugPart<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self.0 {
                    MessageSemantic::Text { text } => f
                        .debug_struct("Text")
                        .field("byte_length", &text.len())
                        .finish(),
                    MessageSemantic::Image { image } => {
                        f.debug_struct("Image").field("image", image).finish()
                    }
                    MessageSemantic::Reasoning { text } => f
                        .debug_struct("Reasoning")
                        .field("byte_length", &text.len())
                        .finish(),
                    MessageSemantic::ToolCall { call } => f
                        .debug_struct("ToolCall")
                        .field("name", &call.name)
                        .finish(),
                }
            }
        }
        let parts: Vec<_> = self
            .message
            .parts
            .iter()
            .map(|part| DebugPart(&part.semantic))
            .collect();
        f.debug_struct("TurnInput")
            .field("parts", &parts)
            .field("blob_count", &self.blobs.len())
            .finish()
    }
}
