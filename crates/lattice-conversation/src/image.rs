// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 用户输入图片媒体类型、内容寻址引用与消息位置校验。

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{MessageRole, MessageSemantic, NativeMessage, ToolCallState};

/// 受支持的用户输入图片媒体类型，仅按文件头前 12 字节魔数识别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageMediaType {
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl ImageMediaType {
    /// 读取输入字节的前 12 字节判定图片媒体类型，忽略扩展名与外部声明的媒体类型。
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        let header = &bytes[..bytes.len().min(12)];
        if header.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(Self::Png)
        } else if header.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if header.len() >= 12 && &header[0..4] == b"RIFF" && &header[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else {
            None
        }
    }

    /// 标准 MIME 类型字符串。
    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Webp => "image/webp",
            Self::Gif => "image/gif",
        }
    }

    /// 面向人展示的简短格式扩展名标签。
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Gif => "gif",
        }
    }
}

/// 与存储解耦的图片内容寻址引用。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRef {
    #[serde(with = "crate::hex_digest")]
    pub sha256: [u8; 32],
    pub media_type: ImageMediaType,
    pub byte_length: u64,
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "系统",
        MessageRole::User => "用户",
        MessageRole::Assistant => "助手",
        MessageRole::Tool => "工具",
    }
}

/// 消息中图片片段的位置或回放状态不满足对话合同。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ImagePlacementError {
    /// 非用户消息中出现了图片片段。
    #[error("第 {part} 个片段为图片，{}消息不能包含图片", role_label(*.role))]
    DisallowedRole {
        /// 消息角色。
        role: MessageRole,
        /// 从 1 开始的片段序号。
        part: usize,
    },
    /// 图片片段携带了不透明回放状态。
    #[error("第 {part} 个图片片段不能携带回放状态")]
    UnexpectedReplayState {
        /// 从 1 开始的片段序号。
        part: usize,
    },
}

/// 核验消息中图片片段的放置规则：图片只允许出现在 User 消息中，且 `replay_state` 必须为 `None`。
pub fn validate_image_placement<S: ToolCallState>(
    message: &NativeMessage<S>,
) -> Result<(), ImagePlacementError> {
    for (index, part) in message.parts.iter().enumerate() {
        if matches!(part.semantic, MessageSemantic::Image { .. }) {
            let ordinal = index + 1;
            if message.role != MessageRole::User {
                return Err(ImagePlacementError::DisallowedRole {
                    role: message.role,
                    part: ordinal,
                });
            }
            if part.replay_state.is_some() {
                return Err(ImagePlacementError::UnexpectedReplayState { part: ordinal });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MessagePart, OpaqueProviderState, UnboundToolCall};

    #[test]
    fn sniff_identifies_four_supported_image_formats_and_metadata_strings() {
        let cases: &[(&[u8], ImageMediaType, &str, &str)] = &[
            (
                b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR",
                ImageMediaType::Png,
                "image/png",
                "png",
            ),
            (
                b"\xff\xd8\xff\xe0\x00\x10JFIF",
                ImageMediaType::Jpeg,
                "image/jpeg",
                "jpg",
            ),
            (
                b"RIFF\x24\x00\x00\x00WEBPVP8 ",
                ImageMediaType::Webp,
                "image/webp",
                "webp",
            ),
            (
                b"GIF87a\x01\x00\x01\x00",
                ImageMediaType::Gif,
                "image/gif",
                "gif",
            ),
            (
                b"GIF89a\x01\x00\x01\x00",
                ImageMediaType::Gif,
                "image/gif",
                "gif",
            ),
        ];

        for &(bytes, expected, expected_mime, expected_ext) in cases {
            assert_eq!(
                ImageMediaType::sniff(bytes),
                Some(expected),
                "魔数识别结果不匹配: {bytes:?}"
            );
            assert_eq!(expected.mime_type(), expected_mime, "MIME 类型不匹配");
            assert_eq!(expected.extension(), expected_ext, "扩展名标签不匹配");
        }
    }

    #[test]
    fn sniff_rejects_other_riff_containers_unsupported_formats_and_truncated_headers() {
        let rejected: &[(&str, &[u8])] = &[
            ("空输入", b""),
            ("纯文本", b"hello world! this is plain text"),
            ("WAV 容器", b"RIFF\x24\x00\x00\x00WAVEfmt "),
            ("AVI 容器", b"RIFF\x00\x01\x00\x00AVI LIST"),
            (
                "SVG 文本",
                b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
            ),
            ("BMP 位图", b"BM\x36\x00\x0c\x00\x00\x00\x00\x00\x36\x00"),
            ("PDF 文档", b"%PDF-1.7\n%\xe2\xe3\xcf\xd3"),
            ("PNG 少 1 字节截断头", b"\x89PNG\r\n\x1a"),
            ("JPEG 少 1 字节截断头", b"\xff\xd8"),
            ("WebP 少 1 字节截断头", b"RIFF\x24\x00\x00\x00WEB"),
            ("GIF87a 少 1 字节截断头", b"GIF87"),
            ("GIF89a 少 1 字节截断头", b"GIF89"),
        ];

        for &(label, bytes) in rejected {
            assert_eq!(
                ImageMediaType::sniff(bytes),
                None,
                "{label} 必须被魔数识别拒绝"
            );
        }
    }

    #[test]
    fn image_ref_serde_exact_json_and_strict_rejection() {
        let sha_hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mut sha256 = [0u8; 32];
        for (i, slot) in sha256.iter_mut().enumerate() {
            *slot = u8::from_str_radix(&sha_hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let image = ImageRef {
            sha256,
            media_type: ImageMediaType::Png,
            byte_length: 123,
        };
        let expected_json = r#"{"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","media_type":"png","byte_length":123}"#;

        let encoded = serde_json::to_string(&image).expect("ImageRef 序列化应成功");
        assert_eq!(encoded, expected_json, "ImageRef JSON 精确格式不匹配");
        let decoded: ImageRef = serde_json::from_str(&encoded).expect("ImageRef 反序列化应成功");
        assert_eq!(decoded, image, "ImageRef 往返应相等");

        for (label, expected_tag, variant) in [
            ("png", "\"png\"", ImageMediaType::Png),
            ("jpeg", "\"jpeg\"", ImageMediaType::Jpeg),
            ("webp", "\"webp\"", ImageMediaType::Webp),
            ("gif", "\"gif\"", ImageMediaType::Gif),
        ] {
            let tag_json = serde_json::to_string(&variant).unwrap();
            assert_eq!(tag_json, expected_tag, "{label} serde 标签不匹配");
            let back: ImageMediaType = serde_json::from_str(&tag_json).unwrap();
            assert_eq!(back, variant, "{label} serde 往返不匹配");
        }

        let invalid_cases = [
            (
                "大写十六进制",
                r#"{"sha256":"0123456789ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef","media_type":"png","byte_length":123}"#,
            ),
            (
                "63 位摘要",
                r#"{"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde","media_type":"png","byte_length":123}"#,
            ),
            (
                "65 位摘要",
                r#"{"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef0","media_type":"png","byte_length":123}"#,
            ),
            (
                "非十六进制字符",
                r#"{"sha256":"g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","media_type":"png","byte_length":123}"#,
            ),
            (
                "未知字段",
                r#"{"sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","media_type":"png","byte_length":123,"extra":true}"#,
            ),
        ];

        for (label, raw_json) in invalid_cases {
            assert!(
                serde_json::from_str::<ImageRef>(raw_json).is_err(),
                "{label} 必须被 ImageRef 反序列化拒绝"
            );
        }
    }

    #[test]
    fn validate_image_placement_enforces_user_role_and_empty_replay_state() {
        let image = ImageRef {
            sha256: [0x11; 32],
            media_type: ImageMediaType::Jpeg,
            byte_length: 512,
        };
        let text_part = MessagePart::<UnboundToolCall>::semantic_only(MessageSemantic::Text {
            text: "说明".into(),
        });
        let image_part =
            MessagePart::<UnboundToolCall>::semantic_only(MessageSemantic::Image { image });

        let valid_user = NativeMessage::new(
            MessageRole::User,
            None,
            vec![text_part.clone(), image_part.clone()],
        );
        assert_eq!(
            validate_image_placement(&valid_user),
            Ok(()),
            "合法用户图片消息应通过位置校验"
        );

        for role in [
            MessageRole::Assistant,
            MessageRole::System,
            MessageRole::Tool,
        ] {
            let bad = NativeMessage::new(role, None, vec![text_part.clone(), image_part.clone()]);
            let err = validate_image_placement(&bad).expect_err("非用户图片消息必须被拒绝");
            assert_eq!(
                err,
                ImagePlacementError::DisallowedRole { role, part: 2 },
                "角色错误应返回从 1 开始的片段序号"
            );
            assert!(
                err.to_string().contains("第 2 个片段"),
                "错误消息应包含中文片段序号说明: {err}"
            );
        }

        let user_with_replay = NativeMessage::new(
            MessageRole::User,
            None,
            vec![
                text_part,
                MessagePart {
                    semantic: MessageSemantic::Image { image },
                    replay_state: Some(OpaqueProviderState::new("sig").unwrap()),
                },
            ],
        );
        let err = validate_image_placement(&user_with_replay)
            .expect_err("携带回放状态的用户图片片段必须被拒绝");
        assert_eq!(
            err,
            ImagePlacementError::UnexpectedReplayState { part: 2 },
            "回放状态错误应返回从 1 开始的片段序号"
        );
        assert_eq!(err.to_string(), "第 2 个图片片段不能携带回放状态");
    }
}
