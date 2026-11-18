// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 受检的单轮用户输入片段与规范化载体。

use std::collections::BTreeMap;
use std::fmt;

use lattice_conversation::{
    BoundToolCall, ImageBytesError, ImageRef, MessageSemantic, NativeMessage, UserInputError,
    normalize_user_message,
};

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
