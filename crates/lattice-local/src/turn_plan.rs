// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 终端前端提交前的本地上限、图片引用一致性与 IPC 帧长度预检。

use std::collections::{BTreeMap, BTreeSet};

use lattice_context::ContextEdit;
use lattice_conversation::{
    BoundToolCall, ImageRef, MAX_IMAGE_BYTES, MAX_USER_IMAGE_TOTAL_BYTES, MAX_USER_IMAGES_PER_TURN,
    MAX_USER_TEXT_BYTES, MessageSemantic, UserInputError, normalize_user_message,
};
use lattice_ipc::{
    LocalError, LocalErrorKind, PlannedContextOpRequest, PlannedFeedRequest, PlannedTurnRequest,
    Result, UserInputPart,
};

fn normalize_planned_parts(
    parts: &[UserInputPart<ImageRef>],
) -> Result<lattice_conversation::NativeMessage<BoundToolCall>> {
    let semantic_parts = parts
        .iter()
        .map(|part| match part {
            UserInputPart::Text { text } => {
                MessageSemantic::<BoundToolCall>::Text { text: text.clone() }
            }
            UserInputPart::Image { image } => {
                MessageSemantic::<BoundToolCall>::Image { image: *image }
            }
        })
        .collect();
    normalize_user_message(semantic_parts)
        .map_err(|error| LocalError::new(LocalErrorKind::BadRequest, &error.to_string()))
}

fn check_frame_limit(action: &str, len: usize) -> Result<()> {
    if len > lattice_ipc::ipc::MAX_FRAME {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "{action}请求编码后为 {len} 字节，超过本地 IPC 单帧上限 16,777,216 字节（16 MiB），请减少图片或文本"
            ),
        ));
    }
    Ok(())
}

/// 终端前端 `submit` 提交前的本地预检：
/// 核验输入列表非空、每条输入经 `normalize_user_message` 合规、跨批次累计文本与图片总量满足单轮上限，
/// 以及 `ipc::encoded_submit_len` 不超过 `MAX_FRAME`（16 MiB）；未通过时返回 `BadRequest`。
pub fn check_turn_plan(plan: &PlannedTurnRequest) -> Result<()> {
    if plan.inputs.is_empty() {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &UserInputError::Empty.to_string(),
        ));
    }

    let max_image_bytes = u64::try_from(MAX_IMAGE_BYTES).expect("MAX_IMAGE_BYTES 可转换为 u64");
    let max_total_bytes =
        u64::try_from(MAX_USER_IMAGE_TOTAL_BYTES).expect("MAX_USER_IMAGE_TOTAL_BYTES 可转换为 u64");
    let mut text_bytes: usize = 0;
    let mut image_count: usize = 0;
    let mut image_total_bytes: u64 = 0;

    for item in &plan.inputs {
        let normalized = normalize_planned_parts(&item.parts)?;
        for part in &normalized.parts {
            match &part.semantic {
                MessageSemantic::Text { text } => {
                    text_bytes = text_bytes.checked_add(text.len()).ok_or_else(|| {
                        LocalError::new(
                            LocalErrorKind::BadRequest,
                            &UserInputError::TextTooLarge { actual: usize::MAX }.to_string(),
                        )
                    })?;
                }
                MessageSemantic::Image { image } => {
                    image_count = image_count.checked_add(1).ok_or_else(|| {
                        LocalError::new(
                            LocalErrorKind::BadRequest,
                            &UserInputError::TooManyImages { actual: usize::MAX }.to_string(),
                        )
                    })?;
                    if image.byte_length == 0 || image.byte_length > max_image_bytes {
                        return Err(LocalError::new(
                            LocalErrorKind::BadRequest,
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
                            LocalError::new(
                                LocalErrorKind::BadRequest,
                                &UserInputError::ImageTotalTooLarge { actual: u64::MAX }
                                    .to_string(),
                            )
                        })?;
                }
                MessageSemantic::Reasoning { .. } | MessageSemantic::ToolCall { .. } => {
                    unreachable!("规范化用户输入仅包含 Text 与 Image 片段")
                }
            }
        }
    }

    if text_bytes > MAX_USER_TEXT_BYTES {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &UserInputError::TextTooLarge { actual: text_bytes }.to_string(),
        ));
    }
    if image_count > MAX_USER_IMAGES_PER_TURN {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &UserInputError::TooManyImages {
                actual: image_count,
            }
            .to_string(),
        ));
    }
    if image_total_bytes > max_total_bytes {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &UserInputError::ImageTotalTooLarge {
                actual: image_total_bytes,
            }
            .to_string(),
        ));
    }

    check_frame_limit("提交", lattice_ipc::ipc::encoded_submit_len(plan))
}

/// 终端前端 `feed` 投喂前的本地预检：核验单条输入规范化与 `MAX_FRAME`（16 MiB）上限。
pub fn check_feed_plan(plan: &PlannedFeedRequest) -> Result<()> {
    let _ = normalize_planned_parts(&plan.parts)?;
    check_frame_limit("投喂", lattice_ipc::ipc::encoded_feed_len(plan))
}

/// 终端前端 `context_op` 发送前的本地预检：
/// 核验附带图片无重复摘要、每张附带图片均被 `ReplaceInput` 引用且尺寸合规，以及编码总长不超过 `MAX_FRAME`（16 MiB）。
pub fn check_context_op_plan(plan: &PlannedContextOpRequest) -> Result<()> {
    let max_image_bytes = u64::try_from(MAX_IMAGE_BYTES).expect("MAX_IMAGE_BYTES 可转换为 u64");
    let mut referenced: BTreeMap<[u8; 32], ImageRef> = BTreeMap::new();
    for edit in plan.op.edits() {
        if let ContextEdit::ReplaceInput { message, .. } = edit {
            for part in &message.as_message().parts {
                if let MessageSemantic::Image { image } = &part.semantic {
                    referenced.insert(image.sha256, *image);
                }
            }
        }
    }

    let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
    for (index, item) in plan.images.iter().enumerate() {
        let image = item.image;
        if image.byte_length == 0 || image.byte_length > max_image_bytes {
            return Err(LocalError::new(
                LocalErrorKind::BadRequest,
                &UserInputError::InvalidImageLength {
                    image: index + 1,
                    actual: image.byte_length,
                }
                .to_string(),
            ));
        }
        if !seen.insert(image.sha256) {
            return Err(LocalError::new(
                LocalErrorKind::BadRequest,
                "上下文操作附带了重复摘要的图片",
            ));
        }
        if referenced.get(&image.sha256) != Some(&image) {
            return Err(LocalError::new(
                LocalErrorKind::BadRequest,
                "上下文操作附带了未被任何 ReplaceInput 引用的图片",
            ));
        }
    }

    check_frame_limit("上下文操作", lattice_ipc::ipc::encoded_context_op_len(plan))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lattice_context::{Channel, ContextEdit, ContextOp};
    use lattice_conversation::{
        ImageMediaType, ImageRef, MAX_IMAGE_BYTES, MAX_USER_IMAGE_TOTAL_BYTES,
        MAX_USER_IMAGES_PER_TURN, MAX_USER_TEXT_BYTES, MessageSemantic,
    };
    use lattice_ipc::{
        ContextImage, LocalErrorKind, NodeId, PlannedContextOpRequest, PlannedFeedRequest,
        PlannedTurnRequest, TurnInputItem, UserInputPart,
    };

    use super::*;

    fn fake_image(seed: u8, byte_length: u64) -> ImageRef {
        ImageRef {
            sha256: [seed; 32],
            media_type: ImageMediaType::Png,
            byte_length,
        }
    }

    fn channel(name: &str) -> Channel {
        Channel::parse(name).expect("测试通道名应合法")
    }

    #[test]
    fn check_turn_plan_enforces_batch_limits_and_frame_cap() {
        let sid = crate::new_session_id();
        let tid = crate::new_turn_id();

        let empty = PlannedTurnRequest {
            turn_id: tid,
            session: sid,
            workspace: PathBuf::from("/tmp/work"),
            trigger_channel: channel("cli"),
            inputs: Vec::new(),
            expected_settings: None,
            expected_position: None,
        };
        assert_eq!(
            check_turn_plan(&empty).unwrap_err().kind,
            LocalErrorKind::BadRequest,
            "空 inputs 列表必须被拒绝"
        );

        let over_text = PlannedTurnRequest {
            turn_id: tid,
            session: sid,
            workspace: PathBuf::from("/tmp/work"),
            trigger_channel: channel("cli"),
            inputs: vec![
                TurnInputItem {
                    channel: channel("cli"),
                    parts: vec![UserInputPart::Text {
                        text: "a".repeat(MAX_USER_TEXT_BYTES),
                    }],
                },
                TurnInputItem {
                    channel: channel("tui"),
                    parts: vec![UserInputPart::Text { text: "b".into() }],
                },
            ],
            expected_settings: None,
            expected_position: None,
        };
        assert_eq!(
            check_turn_plan(&over_text).unwrap_err().kind,
            LocalErrorKind::BadRequest,
            "跨条目累计文本超过 2 MiB 必须被拒绝"
        );

        let over_count = PlannedTurnRequest {
            turn_id: tid,
            session: sid,
            workspace: PathBuf::from("/tmp/work"),
            trigger_channel: channel("cli"),
            inputs: vec![
                TurnInputItem {
                    channel: channel("cli"),
                    parts: (0..5)
                        .map(|idx| UserInputPart::Image {
                            image: fake_image(idx, 1024),
                        })
                        .collect(),
                },
                TurnInputItem {
                    channel: channel("tui"),
                    parts: (5..(MAX_USER_IMAGES_PER_TURN as u8 + 1))
                        .map(|idx| UserInputPart::Image {
                            image: fake_image(idx, 1024),
                        })
                        .collect(),
                },
            ],
            expected_settings: None,
            expected_position: None,
        };
        assert_eq!(
            check_turn_plan(&over_count).unwrap_err().kind,
            LocalErrorKind::BadRequest,
            "跨条目累计图片张数超过上限必须被拒绝"
        );

        let over_total_img = PlannedTurnRequest {
            turn_id: tid,
            session: sid,
            workspace: PathBuf::from("/tmp/work"),
            trigger_channel: channel("cli"),
            inputs: vec![
                TurnInputItem {
                    channel: channel("cli"),
                    parts: vec![
                        UserInputPart::Image {
                            image: fake_image(1, MAX_IMAGE_BYTES as u64),
                        },
                        UserInputPart::Image {
                            image: fake_image(2, MAX_IMAGE_BYTES as u64),
                        },
                    ],
                },
                TurnInputItem {
                    channel: channel("tui"),
                    parts: vec![
                        UserInputPart::Image {
                            image: fake_image(3, MAX_IMAGE_BYTES as u64),
                        },
                        UserInputPart::Image {
                            image: fake_image(4, MAX_IMAGE_BYTES as u64),
                        },
                    ],
                },
            ],
            expected_settings: None,
            expected_position: None,
        };
        assert_eq!(
            check_turn_plan(&over_total_img).unwrap_err().kind,
            LocalErrorKind::BadRequest,
            "跨条目累计图片总字节数超过 10 MiB 必须被拒绝"
        );

        let quarter_total = (MAX_USER_IMAGE_TOTAL_BYTES / 4) as u64;
        let exact_ok = PlannedTurnRequest {
            turn_id: tid,
            session: sid,
            workspace: PathBuf::from("/tmp/work"),
            trigger_channel: channel("cli"),
            inputs: vec![
                TurnInputItem {
                    channel: channel("cli"),
                    parts: vec![
                        UserInputPart::Image {
                            image: fake_image(1, quarter_total),
                        },
                        UserInputPart::Image {
                            image: fake_image(2, quarter_total),
                        },
                    ],
                },
                TurnInputItem {
                    channel: channel("tui"),
                    parts: vec![
                        UserInputPart::Text {
                            text: "合计 10 MiB 合法图片".into(),
                        },
                        UserInputPart::Image {
                            image: fake_image(3, quarter_total),
                        },
                        UserInputPart::Image {
                            image: fake_image(4, quarter_total),
                        },
                    ],
                },
            ],
            expected_settings: None,
            expected_position: Some(NodeId(1)),
        };
        assert!(
            check_turn_plan(&exact_ok).is_ok(),
            "跨条目累计在上限与 16 MiB 帧长内的计划必须通过"
        );
    }

    #[test]
    fn check_feed_plan_and_context_op_plan_validate_images_and_frame_cap() {
        let sid = crate::new_session_id();
        let img1 = fake_image(1, 2048);
        let img2 = fake_image(2, 4096);
        let unreferenced = fake_image(9, 1024);

        let ok_feed = PlannedFeedRequest {
            session: sid,
            workspace: PathBuf::from("/tmp/work"),
            channel: channel("cli"),
            parts: vec![
                UserInputPart::Text {
                    text: "投喂内容".into(),
                },
                UserInputPart::Image { image: img1 },
            ],
            expected_position: None,
        };
        assert!(check_feed_plan(&ok_feed).is_ok(), "合法 feed 计划应通过");

        let empty_feed = PlannedFeedRequest {
            parts: Vec::new(),
            ..ok_feed
        };
        assert_eq!(
            check_feed_plan(&empty_feed).unwrap_err().kind,
            LocalErrorKind::BadRequest,
            "空 feed 输入必须被拒绝"
        );

        let op = ContextOp::new(
            channel("tui"),
            vec![
                ContextEdit::replace_input(
                    NodeId(1),
                    vec![
                        MessageSemantic::Text {
                            text: "替换输入".into(),
                        },
                        MessageSemantic::Image { image: img1 },
                        MessageSemantic::Image { image: img2 },
                    ],
                )
                .expect("ReplaceInput 应合法"),
            ],
        )
        .expect("ContextOp 应合法");

        let ok_op = PlannedContextOpRequest {
            session: sid,
            workspace: PathBuf::from("/tmp/work"),
            op: op.clone(),
            images: vec![ContextImage { image: img1 }],
            expected_position: Some(NodeId(2)),
        };
        assert!(
            check_context_op_plan(&ok_op).is_ok(),
            "仅附带部分被引用的新图片应允许（其余图片可已落库）"
        );

        let dup_op = PlannedContextOpRequest {
            images: vec![ContextImage { image: img1 }, ContextImage { image: img1 }],
            ..ok_op.clone()
        };
        assert_eq!(
            check_context_op_plan(&dup_op).unwrap_err().kind,
            LocalErrorKind::BadRequest,
            "重复摘要的附带图片必须被拒绝"
        );

        let unref_op = PlannedContextOpRequest {
            images: vec![ContextImage {
                image: unreferenced,
            }],
            ..ok_op.clone()
        };
        assert_eq!(
            check_context_op_plan(&unref_op).unwrap_err().kind,
            LocalErrorKind::BadRequest,
            "未被任何 ReplaceInput 引用的附带图片必须被拒绝"
        );

        // 5 张 3 MiB 图片经 base64 编码约 20 MiB，超过 16 MiB 单帧上限。
        let big_imgs: Vec<ImageRef> = (1..=5)
            .map(|seed| fake_image(seed, MAX_IMAGE_BYTES as u64))
            .collect();
        let big_op = ContextOp::new(
            channel("cli"),
            big_imgs
                .iter()
                .enumerate()
                .map(|(i, image)| {
                    ContextEdit::replace_input(
                        NodeId(i as u64 + 1),
                        vec![MessageSemantic::Image { image: *image }],
                    )
                    .expect("ReplaceInput 应合法")
                })
                .collect(),
        )
        .expect("ContextOp 应合法");
        let over_frame_op = PlannedContextOpRequest {
            session: sid,
            workspace: PathBuf::from("/tmp/work"),
            op: big_op,
            images: big_imgs
                .into_iter()
                .map(|image| ContextImage { image })
                .collect(),
            expected_position: None,
        };
        assert_eq!(
            check_context_op_plan(&over_frame_op).unwrap_err().kind,
            LocalErrorKind::BadRequest,
            "编码总长超过 16 MiB 的 context_op 请求必须被拒绝"
        );
    }
}
