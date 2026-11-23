// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 有效模型上下文到 provider 中立历史序列的确定性投影 `EffectiveContext::project()`（ADR 0014 第 4 节）。

use lattice_conversation::{
    BoundToolCall, ConversationItem, MessagePart, MessageRole, MessageSemantic, NativeMessage,
};

use crate::channel::Channel;
use crate::resolve::{EffectiveContext, EntryData, InputState, StepState};

const CLOSE_TAG: &str = "</lattice-context>";

impl EffectiveContext {
    /// 将有效模型上下文投影为 provider 中立的对话历史序列（不拼接 `system_prompt`）。
    pub fn project(&self) -> Vec<ConversationItem> {
        let mut items = Vec::with_capacity(self.state.entries.len());

        for entry in &self.state.entries {
            match &entry.data {
                EntryData::Input { channel, state } => {
                    let InputState::Active { parts } = state else {
                        continue;
                    };
                    if parts.is_empty() {
                        continue;
                    }
                    items.push(ConversationItem::Message(project_input(channel, parts)));
                }
                EntryData::Step { state } => {
                    let content = match state {
                        StepState::Active { content, .. } | StepState::Replaced(content) => content,
                        StepState::Omitted => continue,
                    };
                    if content.assistant.parts.is_empty() && content.results.is_empty() {
                        continue;
                    }
                    items.push(ConversationItem::Step(content.clone()));
                }
            }
        }

        items
    }
}

fn project_input(
    channel: &Channel,
    parts: &[MessagePart<BoundToolCall>],
) -> NativeMessage<BoundToolCall> {
    let open = format!(
        "<lattice-context channel=\"{}\">",
        escape_channel_attr(channel.as_str())
    );

    if parts.len() == 1 {
        let wrapped = match &parts[0].semantic {
            MessageSemantic::Text { text } => vec![text_part(format!(
                "{open}\n{}\n{CLOSE_TAG}",
                escape_body_text(text)
            ))],
            MessageSemantic::Image { image } => vec![
                text_part(open),
                MessagePart::semantic_only(MessageSemantic::Image { image: *image }),
                text_part(CLOSE_TAG.to_owned()),
            ],
            MessageSemantic::Reasoning { .. } | MessageSemantic::ToolCall { .. } => {
                unreachable!("ContextInput 仅包含 Text 与 Image 片段")
            }
        };
        return NativeMessage::new(MessageRole::User, None, wrapped);
    }

    let last_index = parts.len() - 1;
    let mut wrapped = Vec::with_capacity(parts.len() + 2);
    for (index, part) in parts.iter().enumerate() {
        let is_first = index == 0;
        let is_last = index == last_index;
        match &part.semantic {
            MessageSemantic::Text { text } => {
                let escaped = escape_body_text(text);
                if is_first {
                    wrapped.push(text_part(format!("{open}\n{escaped}")));
                } else if is_last {
                    wrapped.push(text_part(format!("{escaped}\n{CLOSE_TAG}")));
                } else {
                    wrapped.push(text_part(escaped));
                }
            }
            MessageSemantic::Image { image } => {
                if is_first {
                    wrapped.push(text_part(open.clone()));
                }
                wrapped.push(MessagePart::semantic_only(MessageSemantic::Image {
                    image: *image,
                }));
                if is_last {
                    wrapped.push(text_part(CLOSE_TAG.to_owned()));
                }
            }
            MessageSemantic::Reasoning { .. } | MessageSemantic::ToolCall { .. } => {
                unreachable!("ContextInput 仅包含 Text 与 Image 片段")
            }
        }
    }

    NativeMessage::new(MessageRole::User, None, wrapped)
}

fn text_part(text: String) -> MessagePart<BoundToolCall> {
    MessagePart::semantic_only(MessageSemantic::Text { text })
}

fn escape_channel_attr(channel: &str) -> String {
    let mut out = String::with_capacity(channel.len());
    for ch in channel.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_body_text(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            if line.trim_start_matches('\\') == CLOSE_TAG {
                format!("\\{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
