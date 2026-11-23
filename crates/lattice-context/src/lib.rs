// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 纯同步、零 I/O 的模型上下文领域层（ADR 0014）。
//!
//! 本 crate 负责：
//! - 开放受检来源标识 [`Channel`]；
//! - 规范化多模态上下文输入 [`ContextInput`] 与强类型批量编辑操作 [`ContextOp`] / [`ContextEdit`]；
//! - 沿根到当前位置的单遍扫描归约 [`resolve`]、只读校验 [`EffectiveContext::validate_op`]、
//!   客观统计 [`EffectiveContext::inspect`] 与 provider 中立模型投影 [`EffectiveContext::project`]。

#![forbid(unsafe_code)]

mod channel;
mod inspect;
mod project;
mod resolve;
mod types;

pub use channel::{Channel, ChannelError, MAX_CHANNEL_BYTES};
pub use inspect::{ContextInspection, EffectiveEntrySummary, NodeStatus, OperatedNode};
pub use resolve::{ContextError, EffectiveContext, PathNodeKind, PathRecord, resolve};
pub use types::{
    ContextEdit, ContextInput, ContextOp, ContextText, EmptyContextOpError,
    InvalidUserMessageError, NormalizedUserMessage, TextBoundError, ToolResultText,
};
