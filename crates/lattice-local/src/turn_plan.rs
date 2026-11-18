// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 终端前端提交前的本地单轮上限与 IPC 帧长度预检。

use lattice_conversation::{BoundToolCall, MessageSemantic, normalize_user_message};
use lattice_ipc::{LocalError, LocalErrorKind, PlannedTurnRequest, Result, UserInputPart};

/// 终端前端提交前的本地预检：单轮上限与非空交给 `normalize_user_message`，
/// 帧长度交给 `ipc::encoded_submit_len`；未通过时返回 `BadRequest`。
pub fn check_turn_plan(plan: &PlannedTurnRequest) -> Result<()> {
    let parts = plan
        .parts
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
    normalize_user_message(parts)
        .map_err(|error| LocalError::new(LocalErrorKind::BadRequest, &error.to_string()))?;

    let len = lattice_ipc::ipc::encoded_submit_len(plan);
    if len > lattice_ipc::ipc::MAX_FRAME {
        return Err(LocalError::new(
            LocalErrorKind::BadRequest,
            &format!(
                "提交请求编码后为 {len} 字节，超过本地 IPC 单帧上限 16,777,216 字节（16 MiB），请减少图片或文本"
            ),
        ));
    }
    Ok(())
}
