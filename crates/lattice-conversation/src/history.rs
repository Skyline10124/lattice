// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral 的结构化对话历史项，不拥有持久化、锁或 I/O。

use serde::{Deserialize, Serialize};

use crate::message::NativeMessage;
use crate::tool_call::BoundToolCall;

/// 生产历史只含系统/用户消息及完整配对的 Step，不承接旧 Tool 字符串。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "content",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ConversationItem {
    Message(NativeMessage<BoundToolCall>),
    Step(crate::StepContent),
}
