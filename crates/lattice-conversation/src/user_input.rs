// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 单轮用户输入消息的上限核验与保序规范化。

use thiserror::Error;

use crate::{
    BoundToolCall, MAX_IMAGE_BYTES, MAX_USER_IMAGE_TOTAL_BYTES, MAX_USER_IMAGES_PER_TURN,
    MAX_USER_TEXT_BYTES, MessagePart, MessageRole, MessageSemantic, NativeMessage,
};

/// 单轮用户输入校验或规范化失败原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum UserInputError {
    /// 规范化后既无非空文本也无图片。
    #[error("用户输入不能为空")]
    Empty,
    /// 包含文本与图片之外的消息片段。
    #[error("第 {part} 个用户输入片段类型不受支持，仅允许文本和图片")]
    UnsupportedPart {
        /// 从 1 开始的片段序号。
        part: usize,
    },
    /// 全部文本片段 UTF-8 字节总和超过上限。
    #[error("单轮文本为 {actual} 字节（按 UTF-8 计），不能超过 2,097,152 字节（2 MiB）")]
    TextTooLarge {
        /// 实际 UTF-8 字节总和。
        actual: usize,
    },
    /// 单张图片字节数为零或超过单图上限。
    #[error("第 {image} 张图片为 {actual} 字节，必须大于 0 且不能超过 3,932,160 字节（3.75 MiB）")]
    InvalidImageLength {
        /// 从 1 开始的图片序号。
        image: usize,
        /// 该图片的原始字节长度。
        actual: u64,
    },
    /// 单轮图片张数超过上限。
    #[error("单轮图片为 {actual} 张，不能超过 8 张")]
    TooManyImages {
        /// 实际图片张数。
        actual: usize,
    },
    /// 单轮原始图片字节总和超过上限。
    #[error("单轮图片总计为 {actual} 字节，不能超过 10,485,760 字节（10 MiB）")]
    ImageTotalTooLarge {
        /// 实际图片字节总和。
        actual: u64,
    },
}

/// 按单轮上限核验用户输入片段并执行保序规范化。
pub fn normalize_user_message(
    parts: Vec<MessageSemantic<BoundToolCall>>,
) -> Result<NativeMessage<BoundToolCall>, UserInputError> {
    // 1. 只接受 Text 与 Image。
    for (index, part) in parts.iter().enumerate() {
        if !matches!(
            part,
            MessageSemantic::Text { .. } | MessageSemantic::Image { .. }
        ) {
            return Err(UserInputError::UnsupportedPart { part: index + 1 });
        }
    }

    // 2. 累计全部 Text 的 UTF-8 字节。
    let mut text_bytes: usize = 0;
    for part in &parts {
        if let MessageSemantic::Text { text } = part {
            text_bytes = text_bytes
                .checked_add(text.len())
                .ok_or(UserInputError::TextTooLarge { actual: usize::MAX })?;
        }
    }
    if text_bytes > MAX_USER_TEXT_BYTES {
        return Err(UserInputError::TextTooLarge { actual: text_bytes });
    }

    // 3. 逐图核对 0 < byte_length <= MAX_IMAGE_BYTES。
    let mut image_count: usize = 0;
    let mut image_total_bytes: u64 = 0;
    for part in &parts {
        if let MessageSemantic::Image { image } = part {
            image_count = image_count
                .checked_add(1)
                .ok_or(UserInputError::TooManyImages { actual: usize::MAX })?;
            let valid_len = usize::try_from(image.byte_length)
                .is_ok_and(|len| len > 0 && len <= MAX_IMAGE_BYTES);
            if !valid_len {
                return Err(UserInputError::InvalidImageLength {
                    image: image_count,
                    actual: image.byte_length,
                });
            }
            image_total_bytes = image_total_bytes
                .checked_add(image.byte_length)
                .ok_or(UserInputError::ImageTotalTooLarge { actual: u64::MAX })?;
        }
    }

    // 4. 按出现次数核对图片张数上限。
    if image_count > MAX_USER_IMAGES_PER_TURN {
        return Err(UserInputError::TooManyImages {
            actual: image_count,
        });
    }

    // 5. 核对单轮图片总字节上限。
    let max_total_bytes =
        u64::try_from(MAX_USER_IMAGE_TOTAL_BYTES).expect("MAX_USER_IMAGE_TOTAL_BYTES 可转换为 u64");
    if image_total_bytes > max_total_bytes {
        return Err(UserInputError::ImageTotalTooLarge {
            actual: image_total_bytes,
        });
    }

    // 6 & 7. 删除空 Text（不裁剪空白），合并删除后相邻的 Text，保留 Image 顺序与重复。
    let mut normalized: Vec<MessagePart<BoundToolCall>> = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            MessageSemantic::Text { text } => {
                if text.is_empty() {
                    continue;
                }
                if let Some(MessagePart {
                    semantic: MessageSemantic::Text { text: previous },
                    ..
                }) = normalized.last_mut()
                {
                    previous.push_str(&text);
                } else {
                    normalized.push(MessagePart::semantic_only(MessageSemantic::Text { text }));
                }
            }
            MessageSemantic::Image { image } => {
                normalized.push(MessagePart::semantic_only(MessageSemantic::Image { image }));
            }
            MessageSemantic::Reasoning { .. } | MessageSemantic::ToolCall { .. } => {
                unreachable!("步骤 1 已拒绝非文本和非图片片段")
            }
        }
    }

    // 8. 规范化后结果不能为空。
    if normalized.is_empty() {
        return Err(UserInputError::Empty);
    }

    Ok(NativeMessage::new(MessageRole::User, None, normalized))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ImageMediaType, ImageRef, InvocationId, ModelMessageAddress, SessionId, ToolCallAddress,
        TurnId, UnboundToolCall,
    };

    fn img(tag: u8, byte_length: u64) -> ImageRef {
        ImageRef {
            sha256: [tag; 32],
            media_type: ImageMediaType::Png,
            byte_length,
        }
    }

    fn text(s: &str) -> MessageSemantic<BoundToolCall> {
        MessageSemantic::Text {
            text: s.to_string(),
        }
    }

    fn image(tag: u8, byte_length: u64) -> MessageSemantic<BoundToolCall> {
        MessageSemantic::Image {
            image: img(tag, byte_length),
        }
    }

    fn sample_bound_call() -> BoundToolCall {
        UnboundToolCall {
            provider_call_id: Some("call_1".into()),
            name: "read_file".into(),
            arguments: serde_json::json!({"path": "a.txt"}),
        }
        .bind(
            InvocationId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            ToolCallAddress {
                message: ModelMessageAddress {
                    session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
                    turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
                    model_step_ordinal: 0,
                },
                part_ordinal: 0,
            },
        )
    }

    #[test]
    fn normalize_preserves_interleaved_order_merges_adjacent_text_and_keeps_duplicates() {
        // 纯文本（含空白保留，不裁剪）
        let text_only = normalize_user_message(vec![text("  你好  ")]).expect("纯文本应通过");
        assert_eq!(
            text_only,
            NativeMessage::new(
                MessageRole::User,
                None,
                vec![MessagePart::semantic_only(text("  你好  "))],
            )
        );

        // 纯图片
        let image_only = normalize_user_message(vec![image(1, 512)]).expect("纯图片应通过");
        assert_eq!(
            image_only,
            NativeMessage::new(
                MessageRole::User,
                None,
                vec![MessagePart::semantic_only(image(1, 512))],
            )
        );

        // [Text, Image, Text] 交错保序，不跨 Image 合并
        let interleaved = normalize_user_message(vec![text("图前"), image(1, 100), text("图后")])
            .expect("图文交错应通过");
        assert_eq!(
            interleaved,
            NativeMessage::new(
                MessageRole::User,
                None,
                vec![
                    MessagePart::semantic_only(text("图前")),
                    MessagePart::semantic_only(image(1, 100)),
                    MessagePart::semantic_only(text("图后")),
                ],
            )
        );

        // 删除空文本后合并相邻文本，保留重复图片与顺序
        let merged = normalize_user_message(vec![
            text(""),
            text("甲"),
            text(""),
            text("乙"),
            image(2, 256),
            text(""),
            image(2, 256),
            text("丙"),
            text("丁"),
            text(""),
        ])
        .expect("合并与重复图片应通过");
        assert_eq!(
            merged,
            NativeMessage::new(
                MessageRole::User,
                None,
                vec![
                    MessagePart::semantic_only(text("甲乙")),
                    MessagePart::semantic_only(image(2, 256)),
                    MessagePart::semantic_only(image(2, 256)),
                    MessagePart::semantic_only(text("丙丁")),
                ],
            )
        );
    }

    #[test]
    fn normalize_rejects_empty_and_unsupported_parts() {
        assert_eq!(normalize_user_message(vec![]), Err(UserInputError::Empty));
        assert_eq!(
            normalize_user_message(vec![text(""), text("")]),
            Err(UserInputError::Empty)
        );

        let err_reasoning = normalize_user_message(vec![
            text("ok"),
            MessageSemantic::Reasoning {
                text: "think".into(),
            },
        ])
        .expect_err("Reasoning 片段必须拒绝");
        assert_eq!(err_reasoning, UserInputError::UnsupportedPart { part: 2 });

        let err_tool = normalize_user_message(vec![MessageSemantic::ToolCall {
            call: sample_bound_call(),
        }])
        .expect_err("ToolCall 片段必须拒绝");
        assert_eq!(err_tool, UserInputError::UnsupportedPart { part: 1 });
    }

    #[test]
    fn normalize_enforces_exact_boundaries_for_all_four_limits() {
        // 1. MAX_USER_TEXT_BYTES 恰好通过，超一字节失败
        let half = MAX_USER_TEXT_BYTES / 2;
        let exact_text = normalize_user_message(vec![
            MessageSemantic::Text {
                text: "a".repeat(half),
            },
            MessageSemantic::Text {
                text: "b".repeat(MAX_USER_TEXT_BYTES - half),
            },
        ])
        .expect("恰好 2 MiB 文本应通过");
        assert_eq!(
            exact_text.parts.len(),
            1,
            "相邻最大文本片段应合并为单个片段"
        );

        let over_text = normalize_user_message(vec![
            MessageSemantic::Text {
                text: "a".repeat(MAX_USER_TEXT_BYTES),
            },
            text("z"),
        ])
        .expect_err("超出 1 字节的文本必须拒绝");
        assert_eq!(
            over_text,
            UserInputError::TextTooLarge {
                actual: MAX_USER_TEXT_BYTES + 1
            }
        );
        assert_eq!(
            over_text.to_string(),
            "单轮文本为 2097153 字节（按 UTF-8 计），不能超过 2,097,152 字节（2 MiB）"
        );

        // 2. MAX_IMAGE_BYTES 恰好通过，0 字节与超一字节失败
        assert!(
            normalize_user_message(vec![image(1, MAX_IMAGE_BYTES as u64)]).is_ok(),
            "恰好 3.75 MiB 的单张图片应通过"
        );
        assert_eq!(
            normalize_user_message(vec![image(1, 100), image(2, 0)]),
            Err(UserInputError::InvalidImageLength {
                image: 2,
                actual: 0
            })
        );
        assert_eq!(
            normalize_user_message(vec![image(1, MAX_IMAGE_BYTES as u64 + 1)]),
            Err(UserInputError::InvalidImageLength {
                image: 1,
                actual: MAX_IMAGE_BYTES as u64 + 1
            })
        );

        // 3. MAX_USER_IMAGES_PER_TURN（含重复摘要按出现次数计数）恰好 8 张通过，9 张失败
        let eight_same: Vec<_> = (0..MAX_USER_IMAGES_PER_TURN)
            .map(|_| image(7, 1024))
            .collect();
        let ok_eight = normalize_user_message(eight_same).expect("8 张重复图片应通过");
        assert_eq!(ok_eight.parts.len(), 8);

        let nine_same: Vec<_> = (0..=MAX_USER_IMAGES_PER_TURN)
            .map(|_| image(7, 1024))
            .collect();
        assert_eq!(
            normalize_user_message(nine_same),
            Err(UserInputError::TooManyImages {
                actual: MAX_USER_IMAGES_PER_TURN + 1
            })
        );

        // 4. MAX_USER_IMAGE_TOTAL_BYTES（10 MiB = 3_932_160 * 2 + 2_621_440）恰好通过，超一字节失败
        let exact_total = vec![
            image(1, 3_932_160),
            image(2, 3_932_160),
            image(3, 2_621_440),
        ];
        assert!(
            normalize_user_message(exact_total).is_ok(),
            "图片总字节恰好 10 MiB 应通过"
        );

        let over_total = vec![
            image(1, 3_932_160),
            image(2, 3_932_160),
            image(3, 2_621_441),
        ];
        assert_eq!(
            normalize_user_message(over_total),
            Err(UserInputError::ImageTotalTooLarge {
                actual: MAX_USER_IMAGE_TOTAL_BYTES as u64 + 1
            })
        );
    }
}
