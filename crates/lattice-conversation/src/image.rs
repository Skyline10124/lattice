// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 用户输入图片媒体类型、内容寻址引用与消息位置校验。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{MAX_IMAGE_BYTES, MessageRole, MessageSemantic, NativeMessage, ToolCallState};

/// 识别受支持图片格式所需的文件头魔数最大长度（字节）。
const MAGIC_HEADER_LEN: usize = 12;

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
        let header = &bytes[..bytes.len().min(MAGIC_HEADER_LEN)];
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

/// 由原始图片字节构造 [`ImageRef`] 失败的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum ImageBytesError {
    /// 图片字节数为零或超过单图上限。
    #[error("图片为 {actual} 字节，必须大于 0 且不能超过 3,932,160 字节（3.75 MiB）")]
    InvalidLength {
        /// 图片的原始字节长度。
        actual: u64,
    },
    /// 文件头魔数不属于受支持的图片格式。
    #[error("图片不是受支持的格式，仅支持 PNG、JPEG、WebP、GIF")]
    UnsupportedFormat,
}

/// 校验单张图片的原始字节长度是否落在 `(0, MAX_IMAGE_BYTES]` 内。
pub(crate) fn is_valid_image_length(byte_length: u64) -> bool {
    usize::try_from(byte_length).is_ok_and(|len| len > 0 && len <= MAX_IMAGE_BYTES)
}

impl ImageRef {
    /// 由原始图片字节构造引用：长度必须在 (0, MAX_IMAGE_BYTES] 内且魔数受支持，随后计算 SHA-256。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ImageBytesError> {
        let mut hasher = ImageRefHasher::new();
        hasher.update(bytes)?;
        hasher.finish()
    }
}

#[derive(Clone, Debug)]
enum HeaderState {
    Collecting {
        buf: [u8; MAGIC_HEADER_LEN],
        len: usize,
    },
    Valid(ImageMediaType),
    Invalid,
}

/// 按分块输入流式校验并计算 [`ImageRef`]，不持有整张图片字节。
///
/// 累计长度超过 [`MAX_IMAGE_BYTES`] 时立即返回 [`ImageBytesError::InvalidLength`]；
/// 一旦越限，后续再次调用 [`Self::update`] 或 [`Self::finish`] 仍返回长度错误。
#[derive(Clone, Debug)]
pub struct ImageRefHasher {
    hasher: Sha256,
    header: HeaderState,
    byte_length: u64,
}

impl Default for ImageRefHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl ImageRefHasher {
    /// 构造空的图片分块摘要计算器。
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            header: HeaderState::Collecting {
                buf: [0; MAGIC_HEADER_LEN],
                len: 0,
            },
            byte_length: 0,
        }
    }

    /// 喂入一段原始图片字节；累计长度超过 [`MAX_IMAGE_BYTES`] 时立即拒绝。
    pub fn update(&mut self, chunk: &[u8]) -> Result<(), ImageBytesError> {
        let next = self
            .byte_length
            .saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
        if next > MAX_IMAGE_BYTES as u64 {
            self.byte_length = next;
            return Err(ImageBytesError::InvalidLength { actual: next });
        }
        self.byte_length = next;
        if chunk.is_empty() {
            return Ok(());
        }
        match &mut self.header {
            HeaderState::Collecting { buf, len } => {
                let take = (MAGIC_HEADER_LEN - *len).min(chunk.len());
                buf[*len..*len + take].copy_from_slice(&chunk[..take]);
                *len += take;
                if *len == MAGIC_HEADER_LEN {
                    let header = *buf;
                    match ImageMediaType::sniff(&header) {
                        Some(media_type) => {
                            self.hasher.update(header);
                            if take < chunk.len() {
                                self.hasher.update(&chunk[take..]);
                            }
                            self.header = HeaderState::Valid(media_type);
                        }
                        None => {
                            self.header = HeaderState::Invalid;
                        }
                    }
                }
            }
            HeaderState::Valid(_) => {
                self.hasher.update(chunk);
            }
            HeaderState::Invalid => {}
        }
        Ok(())
    }

    /// 结束输入并核验非空与文件头魔数，产出 [`ImageRef`]。
    pub fn finish(mut self) -> Result<ImageRef, ImageBytesError> {
        if !is_valid_image_length(self.byte_length) {
            return Err(ImageBytesError::InvalidLength {
                actual: self.byte_length,
            });
        }
        let media_type = match self.header {
            HeaderState::Collecting { buf, len } => {
                let header = &buf[..len];
                let media_type =
                    ImageMediaType::sniff(header).ok_or(ImageBytesError::UnsupportedFormat)?;
                self.hasher.update(header);
                media_type
            }
            HeaderState::Valid(media_type) => media_type,
            HeaderState::Invalid => return Err(ImageBytesError::UnsupportedFormat),
        };
        Ok(ImageRef {
            sha256: self.hasher.finalize().into(),
            media_type,
            byte_length: self.byte_length,
        })
    }
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
    #[error("第 {part} 个片段为图片，不能携带回放状态")]
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
        assert_eq!(err.to_string(), "第 2 个片段为图片，不能携带回放状态");
    }

    #[test]
    fn from_bytes_validates_length_magic_and_computes_sha256() {
        let cases: &[(&[u8], ImageMediaType)] = &[
            (b"\x89PNG\r\n\x1a\npayload", ImageMediaType::Png),
            (b"\xff\xd8\xff\xe0jpeg-body", ImageMediaType::Jpeg),
            (b"RIFF\x24\x00\x00\x00WEBPbody", ImageMediaType::Webp),
            (b"GIF87a\x01\x00\x01\x00", ImageMediaType::Gif),
            (b"GIF89a\x01\x00\x01\x00", ImageMediaType::Gif),
        ];
        for &(bytes, expected_type) in cases {
            let image = ImageRef::from_bytes(bytes).expect("受支持的图片字节应构造成功");
            let expected_digest: [u8; 32] = Sha256::digest(bytes).into();
            assert_eq!(image.sha256, expected_digest, "图片摘要必须等于 SHA-256");
            assert_eq!(image.media_type, expected_type, "图片媒体类型不匹配");
            assert_eq!(image.byte_length, bytes.len() as u64, "图片长度不匹配");
        }

        assert_eq!(
            ImageRef::from_bytes(b""),
            Err(ImageBytesError::InvalidLength { actual: 0 }),
            "空字节必须报长度无效"
        );

        let mut max_png = vec![0xAB; MAX_IMAGE_BYTES];
        max_png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let max_ref = ImageRef::from_bytes(&max_png).expect("恰好 MAX_IMAGE_BYTES 的图片应通过");
        assert_eq!(max_ref.byte_length, MAX_IMAGE_BYTES as u64);
        assert_eq!(max_ref.media_type, ImageMediaType::Png);
        assert_eq!(max_ref.sha256, <[u8; 32]>::from(Sha256::digest(&max_png)));

        let mut over_max = vec![0xAB; MAX_IMAGE_BYTES + 1];
        over_max[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            ImageRef::from_bytes(&over_max),
            Err(ImageBytesError::InvalidLength {
                actual: (MAX_IMAGE_BYTES + 1) as u64
            }),
            "超出单图上限一字节必须报长度无效"
        );

        let mut bad_over_max = vec![0x00; MAX_IMAGE_BYTES + 1];
        bad_over_max[..4].copy_from_slice(b"WAVE");
        assert_eq!(
            ImageRef::from_bytes(&bad_over_max),
            Err(ImageBytesError::InvalidLength {
                actual: (MAX_IMAGE_BYTES + 1) as u64
            }),
            "超限输入应先按长度拒绝"
        );

        for unsupported in [
            b"<svg></svg>".as_slice(),
            b"BM\x36\x00\x0c\x00\x00\x00\x00\x00\x36\x00".as_slice(),
            b"RIFF\x24\x00\x00\x00WAVEfmt ".as_slice(),
            b"\x89PNG\r\n\x1a".as_slice(),
        ] {
            assert_eq!(
                ImageRef::from_bytes(unsupported),
                Err(ImageBytesError::UnsupportedFormat),
                "不受支持的魔数必须拒绝"
            );
        }
    }

    #[test]
    fn hasher_matches_from_bytes_across_chunk_boundaries_and_enforces_limits() {
        // 覆盖短于 12 字节、恰好 12 字节、跨 12 字节边界与跨大块的分块切法。
        let mut large_webp = vec![0x5Au8; 25_000];
        large_webp[..12].copy_from_slice(b"RIFF\x24\x00\x00\x00WEBP");

        let payloads: &[&[u8]] = &[
            b"GIF89a",
            b"\xff\xd8\xff",
            b"\x89PNG\r\n\x1a\n1234",
            b"\x89PNG\r\n\x1a\npayload-crossing-header-boundary",
            &large_webp,
        ];
        for &payload in payloads {
            let expected = ImageRef::from_bytes(payload).expect("合法样本应构造成功");
            for chunk_size in [1usize, 3, 5, 12, 13, 4096] {
                let mut hasher = ImageRefHasher::new();
                // 穿插空块不应影响结果。
                hasher.update(b"").expect("空块不应失败");
                for chunk in payload.chunks(chunk_size) {
                    hasher.update(chunk).expect("合规分块不应失败");
                }
                let actual = hasher.finish().expect("合规流结束应返回 ImageRef");
                assert_eq!(
                    actual, expected,
                    "块大小 {chunk_size} 的分块计算结果必须与 from_bytes 一致"
                );
            }
        }

        // 空输入直接结束或只喂空块必须报长度无效。
        assert_eq!(
            ImageRefHasher::new().finish(),
            Err(ImageBytesError::InvalidLength { actual: 0 })
        );

        // 非法魔数跨块输入在结束时拒绝，不产出 ImageRef。
        let mut invalid = ImageRefHasher::new();
        invalid.update(b"RIFF\x24\x00").unwrap();
        invalid.update(b"\x00\x00WAVEfmt ").unwrap();
        assert_eq!(invalid.finish(), Err(ImageBytesError::UnsupportedFormat));

        // 越过 MAX_IMAGE_BYTES 立即报长度无效；即使忽略错误继续调用也不得成功。
        let mut over = ImageRefHasher::new();
        over.update(b"\x89PNG\r\n\x1a\n").unwrap();
        let body = vec![0xCDu8; MAX_IMAGE_BYTES - 8];
        over.update(&body).expect("恰好落到上限时仍应接受");
        assert_eq!(
            over.update(b"x"),
            Err(ImageBytesError::InvalidLength {
                actual: (MAX_IMAGE_BYTES + 1) as u64,
            }),
            "越过上限 1 字节必须立即拒绝"
        );
        assert_eq!(
            over.update(b""),
            Err(ImageBytesError::InvalidLength {
                actual: (MAX_IMAGE_BYTES + 1) as u64,
            }),
            "越限后再喂空块仍必须保持长度错误"
        );
        assert_eq!(
            over.finish(),
            Err(ImageBytesError::InvalidLength {
                actual: (MAX_IMAGE_BYTES + 1) as u64,
            }),
            "越限后调用 finish 不得返回合法 ImageRef"
        );
    }
}
