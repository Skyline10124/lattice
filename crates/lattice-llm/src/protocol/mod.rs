// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 协议标识与各协议支持的可选行为。

use serde::Deserialize;

mod features;
pub(crate) use features::{
    Cache, ChatCompletionsFeatures, EffortParam, Features, MessagesFeatures, ReasoningReplay,
    ThinkingBudget, ThinkingToggle,
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Protocol {
    ChatCompletions,
    Responses,
    #[serde(rename = "anthropic-messages")]
    Messages,
    GenerateContent,
    Interactions,
}

impl Protocol {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat-completions",
            Self::Responses => "responses",
            Self::Messages => "anthropic-messages",
            Self::GenerateContent => "generate-content",
            Self::Interactions => "interactions",
        }
    }
}
