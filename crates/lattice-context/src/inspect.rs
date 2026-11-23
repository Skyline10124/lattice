// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 有效模型上下文的纯客观统计 `EffectiveContext::inspect()`（ADR 0014 第 4 节）。

use std::collections::BTreeMap;

use lattice_conversation::{
    ImageRef, MessageSemantic, NodeId, ResultPayload, StepContent, SuccessContent, canonical_json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::channel::Channel;
use crate::resolve::{EffectiveContext, EntryData, InputState, StepState};

/// 与 `lattice-llm` 投影一致的失败载荷错误消息截断上限（UTF-8 字符边界内最大字节数）。
const BOUNDED_ERROR_CONTENT_BYTES: usize = 4_096;

/// 已被上下文编辑作用过的节点当前最终状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// 经局部改写或替换后仍保留有效内容。
    Modified,
    /// 经局部移出后有效内容为空，在统计条目与投影中自动跳过。
    SkippedEmpty,
    /// 已被 `OmitNode` 整节点移出模型上下文。
    Omitted,
}

/// 曾被至少一次编辑作用过的路径节点及其当前最终状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatedNode {
    pub node: NodeId,
    pub status: NodeStatus,
}

/// 有效模型上下文中单条保留项的客观字节与结构摘要。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EffectiveEntrySummary {
    /// 有效 `ContextInput` 条目摘要。
    Input {
        node: NodeId,
        channel: Channel,
        text_bytes: usize,
        image_count: usize,
        image_bytes: u64,
    },
    /// 有效 `Step` 条目摘要。
    Step {
        node: NodeId,
        text_bytes: usize,
        reasoning_bytes: usize,
        tool_call_count: usize,
        tool_result_bytes: usize,
    },
}

/// 有效模型上下文的客观统计快照（不包含 32 MiB 请求体预算或模型图片模态判断）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextInspection {
    pub entries: Vec<EffectiveEntrySummary>,
    pub operated_nodes: Vec<OperatedNode>,
    pub channel_counts: BTreeMap<Channel, usize>,
    pub total_text_bytes: usize,
    pub images: Vec<ImageRef>,
    pub total_image_bytes: u64,
}

impl EffectiveContext {
    /// 返回当前有效模型上下文的纯客观字节与结构统计。
    pub fn inspect(&self) -> ContextInspection {
        let mut entries = Vec::new();
        let mut operated_nodes = Vec::new();
        let mut channel_counts: BTreeMap<Channel, usize> = BTreeMap::new();
        let mut total_text_bytes: usize = 0;
        let mut images = Vec::new();
        let mut total_image_bytes: u64 = 0;

        for entry in &self.state.entries {
            if let Some(status) = entry.status {
                operated_nodes.push(OperatedNode {
                    node: entry.node,
                    status,
                });
            }

            match &entry.data {
                EntryData::Input { channel, state } => {
                    let InputState::Active { parts } = state else {
                        continue;
                    };
                    if parts.is_empty() {
                        continue;
                    }
                    let mut text_bytes: usize = 0;
                    let mut image_count: usize = 0;
                    let mut image_bytes: u64 = 0;
                    for part in parts {
                        match &part.semantic {
                            MessageSemantic::Text { text } => {
                                text_bytes = text_bytes.saturating_add(text.len());
                            }
                            MessageSemantic::Image { image } => {
                                image_count = image_count.saturating_add(1);
                                image_bytes = image_bytes.saturating_add(image.byte_length);
                                images.push(*image);
                                total_image_bytes =
                                    total_image_bytes.saturating_add(image.byte_length);
                            }
                            MessageSemantic::Reasoning { .. }
                            | MessageSemantic::ToolCall { .. } => {
                                unreachable!("ContextInput 仅包含 Text 与 Image 片段")
                            }
                        }
                    }
                    *channel_counts.entry(channel.clone()).or_insert(0) += 1;
                    total_text_bytes = total_text_bytes.saturating_add(text_bytes);
                    entries.push(EffectiveEntrySummary::Input {
                        node: entry.node,
                        channel: channel.clone(),
                        text_bytes,
                        image_count,
                        image_bytes,
                    });
                }
                EntryData::Step { state } => {
                    let content = match state {
                        StepState::Active { content, .. } | StepState::Replaced(content) => content,
                        StepState::Omitted => continue,
                    };
                    if content.assistant.parts.is_empty() && content.results.is_empty() {
                        continue;
                    }
                    let summary = summarize_step(entry.node, content);
                    if let EffectiveEntrySummary::Step {
                        text_bytes,
                        reasoning_bytes,
                        tool_result_bytes,
                        ..
                    } = &summary
                    {
                        total_text_bytes = total_text_bytes
                            .saturating_add(*text_bytes)
                            .saturating_add(*reasoning_bytes)
                            .saturating_add(*tool_result_bytes);
                    }
                    entries.push(summary);
                }
            }
        }

        ContextInspection {
            entries,
            operated_nodes,
            channel_counts,
            total_text_bytes,
            images,
            total_image_bytes,
        }
    }
}

fn summarize_step(node: NodeId, content: &StepContent) -> EffectiveEntrySummary {
    let mut text_bytes: usize = 0;
    let mut reasoning_bytes: usize = 0;
    for part in &content.assistant.parts {
        match &part.semantic {
            MessageSemantic::Text { text } => {
                text_bytes = text_bytes.saturating_add(text.len());
            }
            MessageSemantic::Reasoning { text } => {
                reasoning_bytes = reasoning_bytes.saturating_add(text.len());
            }
            MessageSemantic::ToolCall { .. } | MessageSemantic::Image { .. } => {}
        }
    }
    let tool_result_bytes = content
        .results
        .iter()
        .map(|result| tool_result_payload_bytes(&result.payload))
        .fold(0usize, usize::saturating_add);

    EffectiveEntrySummary::Step {
        node,
        text_bytes,
        reasoning_bytes,
        tool_call_count: content.results.len(),
        tool_result_bytes,
    }
}

/// 按与 `lattice-llm` 投影一致的规范 JSON 形式计量单条工具结果载荷字节长度。
fn tool_result_payload_bytes(payload: &ResultPayload) -> usize {
    match payload {
        ResultPayload::Succeeded(SuccessContent::Json { byte_length, .. }) => {
            usize::try_from(*byte_length).unwrap_or(usize::MAX)
        }
        ResultPayload::Succeeded(SuccessContent::OutputTooLarge {
            digest,
            byte_length,
        }) => {
            let value = json!({
                "outcome": "succeeded",
                "content_unavailable": "output_too_large",
                "digest": digest.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "byte_length": byte_length,
            });
            canonical_json(&value).len()
        }
        ResultPayload::Failed(error) => {
            let mut end = error.message.len().min(BOUNDED_ERROR_CONTENT_BYTES);
            while !error.message.is_char_boundary(end) {
                end -= 1;
            }
            let value = json!({
                "error": {
                    "code": &error.code,
                    "message": &error.message[..end],
                }
            });
            canonical_json(&value).len()
        }
        ResultPayload::NotExecuted => canonical_json(&json!({"outcome": "not_executed"})).len(),
        ResultPayload::Indeterminate(cause) => {
            canonical_json(&json!({"outcome": "indeterminate", "cause": cause})).len()
        }
    }
}
