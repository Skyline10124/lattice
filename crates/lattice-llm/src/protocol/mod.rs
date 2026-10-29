// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 按协议组织的流解析与消息投影，以及协议标识与各协议支持的可选行为。

pub use lattice_conversation::Protocol;

pub(crate) mod anthropic_messages;
pub(crate) mod chat_completions;
mod common;
mod features;
pub(crate) mod generate_content;
pub mod projection;
mod usage;

pub(crate) use features::{
    Cache, ChatCompletionsFeatures, EffortParam, Features, MessagesFeatures, ReasoningReplay,
    ThinkingBudget, ThinkingToggle,
};
