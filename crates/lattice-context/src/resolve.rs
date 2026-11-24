// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 沿根到当前位置的单遍扫描归约 `resolve()` 与只读操作校验 `validate_op()`（ADR 0014 第 4 节）。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use lattice_conversation::{
    BoundToolCall, ImageRef, InvocationId, MessagePart, MessageRole, MessageSemantic,
    NativeMessage, NodeId, ResultPayload, StepContent, SuccessContent, ToolResultOutcome,
    canonical_json, tool_content_digest, validate_image_placement,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::channel::Channel;
use crate::inspect::NodeStatus;
use crate::types::{ContextEdit, ContextInput, ContextOp};

/// 沿根到当前位置收集的单条路径记录。
#[derive(Clone, Debug, PartialEq)]
pub enum PathRecord<'a> {
    /// 一次上下文投喂节点。
    Input { id: NodeId, input: &'a ContextInput },
    /// 一条已有完整回复的 `Step` 节点。
    Step { id: NodeId, content: StepContent },
    /// 一次批量上下文操作节点。
    Op { id: NodeId, op: &'a ContextOp },
}

/// 路径节点类别，用于诊断目标类型错配。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathNodeKind {
    Input,
    Step,
    Op,
}

impl fmt::Display for PathNodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Input => "Input",
            Self::Step => "Step",
            Self::Op => "Op",
        };
        f.write_str(label)
    }
}

/// 上下文路径归约或 `ContextOp` 校验失败原因。
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum ContextError {
    /// 路径中出现重复的节点序号。
    #[error("路径中出现重复的节点 NodeId({})", .node.0)]
    DuplicateNodeId { node: NodeId },
    /// `Step` 记录不满足助手消息与调用/结果配对不变量。
    #[error("节点 NodeId({}) 的 Step 内容无效：{reason}", .node.0)]
    InvalidStep { node: NodeId, reason: String },
    /// 路径中出现重复的工具调用身份。
    #[error("路径中出现重复的工具调用 InvocationId({invocation})")]
    DuplicateInvocationId { invocation: InvocationId },
    /// 编辑引用的目标节点不在当前路径前置记录中。
    #[error("当前路径前置记录中不存在节点 NodeId({})", .node.0)]
    UnknownNode { node: NodeId },
    /// 编辑引用的目标工具调用不在当前路径前置 `Step` 中。
    #[error("当前路径前置记录中不存在工具调用 InvocationId({invocation})")]
    UnknownInvocation { invocation: InvocationId },
    /// 编辑目标节点类别与操作变体不匹配。
    #[error("节点 NodeId({}) 类型为 {actual}，不支持该上下文编辑操作", .node.0)]
    NodeKindMismatch { node: NodeId, actual: PathNodeKind },
    /// 目标节点已被 `OmitNode` 移出，不能再施加编辑。
    #[error("节点 NodeId({}) 已被移出模型上下文，不能再施加编辑", .node.0)]
    NodeAlreadyOmitted { node: NodeId },
    /// 目标 `Step` 已被 `ReplaceStep` 整步替换，不能再施加局部编辑。
    #[error("Step 节点 NodeId({}) 已被整步替换，不能再施加局部编辑", .node.0)]
    StepAlreadyReplaced { node: NodeId },
    /// 目标工具调用已被先前 `OmitInvocation` 成对移出。
    #[error("工具调用 InvocationId({invocation}) 已被移出")]
    InvocationAlreadyOmitted { invocation: InvocationId },
    /// 目标工具调用的结局无可替换的正文载荷（仅 `Succeeded` 与 `Failed` 允许替换）。
    #[error(
        "工具调用 InvocationId({invocation}) 的结局为 {}，无可替换的正文载荷",
        .outcome.as_str()
    )]
    ToolResultNotReplaceable {
        invocation: InvocationId,
        outcome: ToolResultOutcome,
    },
    /// 目标 `Input` 当前有效片段中不含指定的图片引用。
    #[error("Input 节点 NodeId({}) 当前有效片段中不包含目标图片", .node.0)]
    ImageNotFound { node: NodeId, image: ImageRef },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum InputState {
    Active {
        parts: Vec<MessagePart<BoundToolCall>>,
    },
    Omitted,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StepState {
    Active {
        content: StepContent,
        omitted_invocations: BTreeSet<InvocationId>,
    },
    Replaced(StepContent),
    Omitted,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EntryData {
    Input { channel: Channel, state: InputState },
    Step { state: StepState },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PathEntry {
    pub(crate) node: NodeId,
    pub(crate) data: EntryData,
    pub(crate) status: Option<NodeStatus>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ResolveState {
    pub(crate) entries: Vec<PathEntry>,
    entry_index: BTreeMap<NodeId, usize>,
    node_kinds: BTreeMap<NodeId, PathNodeKind>,
    invocation_owner: BTreeMap<InvocationId, NodeId>,
}

/// 沿根到当前位置单遍扫描归约得到的有效模型上下文。
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveContext {
    pub(crate) state: ResolveState,
}

/// 对沿根到当前位置收集的顺序路径记录执行单遍扫描归约。
pub fn resolve(path: &[PathRecord<'_>]) -> Result<EffectiveContext, ContextError> {
    let mut state = ResolveState::default();

    for record in path {
        match record {
            PathRecord::Input { id, input } => {
                ensure_unique_node(&state, *id)?;
                let index = state.entries.len();
                state.entries.push(PathEntry {
                    node: *id,
                    data: EntryData::Input {
                        channel: input.channel().clone(),
                        state: InputState::Active {
                            parts: input.message().parts.clone(),
                        },
                    },
                    status: None,
                });
                state.entry_index.insert(*id, index);
                state.node_kinds.insert(*id, PathNodeKind::Input);
            }
            PathRecord::Step { id, content } => {
                ensure_unique_node(&state, *id)?;
                let invocations = validate_step_entrance(&state, *id, content)?;
                let index = state.entries.len();
                state.entries.push(PathEntry {
                    node: *id,
                    data: EntryData::Step {
                        state: StepState::Active {
                            content: content.clone(),
                            omitted_invocations: BTreeSet::new(),
                        },
                    },
                    status: None,
                });
                state.entry_index.insert(*id, index);
                state.node_kinds.insert(*id, PathNodeKind::Step);
                for invocation in invocations {
                    state.invocation_owner.insert(invocation, *id);
                }
            }
            PathRecord::Op { id, op } => {
                ensure_unique_node(&state, *id)?;
                // 在登记本 Op 自身前执行应用，保证编辑只能引用本 Op 之前出现的路径节点。
                apply_op(&mut state, op)?;
                state.node_kinds.insert(*id, PathNodeKind::Op);
            }
        }
    }

    Ok(EffectiveContext { state })
}

impl EffectiveContext {
    /// 只读校验待落树的 [`ContextOp`] 是否对当前有效上下文合法；与 [`resolve`] 共用同一应用规则且无副作用。
    pub fn validate_op(&self, op: &ContextOp) -> Result<(), ContextError> {
        let mut preview = self.state.clone();
        apply_op(&mut preview, op)
    }
}

fn ensure_unique_node(state: &ResolveState, id: NodeId) -> Result<(), ContextError> {
    if state.node_kinds.contains_key(&id) {
        return Err(ContextError::DuplicateNodeId { node: id });
    }
    Ok(())
}

fn validate_step_entrance(
    state: &ResolveState,
    node: NodeId,
    content: &StepContent,
) -> Result<Vec<InvocationId>, ContextError> {
    if content.assistant.role != MessageRole::Assistant {
        return Err(ContextError::InvalidStep {
            node,
            reason: "助手消息角色必须为 assistant".to_owned(),
        });
    }
    if let Err(err) = validate_image_placement(&content.assistant) {
        return Err(ContextError::InvalidStep {
            node,
            reason: format!("助手消息图片位置无效：{err}"),
        });
    }

    let calls: Vec<_> = content.assistant.tool_calls().collect();
    if calls.len() != content.results.len() {
        return Err(ContextError::InvalidStep {
            node,
            reason: format!(
                "工具调用数量（{}）与结果数量（{}）不一致",
                calls.len(),
                content.results.len()
            ),
        });
    }

    let mut seen_in_step = BTreeSet::new();
    let mut invocations = Vec::with_capacity(calls.len());
    for (index, ((_, call), result)) in calls.into_iter().zip(&content.results).enumerate() {
        if call.invocation_id != result.invocation_id || call.name != result.name {
            return Err(ContextError::InvalidStep {
                node,
                reason: format!("第 {} 个工具调用与结果身份或工具名不匹配", index + 1),
            });
        }
        if state.invocation_owner.contains_key(&call.invocation_id)
            || !seen_in_step.insert(call.invocation_id)
        {
            return Err(ContextError::DuplicateInvocationId {
                invocation: call.invocation_id,
            });
        }
        invocations.push(call.invocation_id);
    }

    Ok(invocations)
}

fn apply_op(state: &mut ResolveState, op: &ContextOp) -> Result<(), ContextError> {
    for edit in op.edits() {
        apply_edit(state, edit)?;
    }
    Ok(())
}

fn apply_edit(state: &mut ResolveState, edit: &ContextEdit) -> Result<(), ContextError> {
    match edit {
        ContextEdit::OmitNode { node } => {
            let kind = lookup_node_kind(state, *node)?;
            if kind == PathNodeKind::Op {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: PathNodeKind::Op,
                });
            }
            let entry = entry_mut(state, *node);
            match &mut entry.data {
                EntryData::Input { state, .. } => {
                    if matches!(state, InputState::Omitted) {
                        return Err(ContextError::NodeAlreadyOmitted { node: *node });
                    }
                    *state = InputState::Omitted;
                }
                EntryData::Step { state } => {
                    if matches!(state, StepState::Omitted) {
                        return Err(ContextError::NodeAlreadyOmitted { node: *node });
                    }
                    *state = StepState::Omitted;
                }
            }
            entry.status = Some(NodeStatus::Omitted);
            Ok(())
        }
        ContextEdit::ReplaceInput { node, message } => {
            let kind = lookup_node_kind(state, *node)?;
            if kind != PathNodeKind::Input {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: kind,
                });
            }
            let entry = entry_mut(state, *node);
            let EntryData::Input { state, .. } = &mut entry.data else {
                unreachable!("已确认节点类别为 Input");
            };
            if matches!(state, InputState::Omitted) {
                return Err(ContextError::NodeAlreadyOmitted { node: *node });
            }
            *state = InputState::Active {
                parts: message.as_message().parts.clone(),
            };
            entry.status = Some(NodeStatus::Modified);
            Ok(())
        }
        ContextEdit::ReplaceImage { node, image, text } => {
            let kind = lookup_node_kind(state, *node)?;
            if kind != PathNodeKind::Input {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: kind,
                });
            }
            let entry = entry_mut(state, *node);
            let EntryData::Input { state, .. } = &mut entry.data else {
                unreachable!("已确认节点类别为 Input");
            };
            let InputState::Active { parts } = state else {
                return Err(ContextError::NodeAlreadyOmitted { node: *node });
            };

            let mut replaced_any = false;
            let mut updated: Vec<MessagePart<BoundToolCall>> = Vec::with_capacity(parts.len());
            for part in parts.iter() {
                match &part.semantic {
                    MessageSemantic::Image { image: candidate } if candidate == image => {
                        replaced_any = true;
                        if let Some(replacement) = text {
                            push_merged_user_text(&mut updated, replacement.as_str());
                        }
                    }
                    MessageSemantic::Text { text: existing } => {
                        push_merged_user_text(&mut updated, existing);
                    }
                    MessageSemantic::Image { image: other } => {
                        updated.push(MessagePart::semantic_only(MessageSemantic::Image {
                            image: *other,
                        }));
                    }
                    MessageSemantic::Reasoning { .. } | MessageSemantic::ToolCall { .. } => {
                        unreachable!("ContextInput 仅包含 Text 与 Image 片段")
                    }
                }
            }
            if !replaced_any {
                return Err(ContextError::ImageNotFound {
                    node: *node,
                    image: *image,
                });
            }
            *parts = updated;
            entry.status = Some(if parts.is_empty() {
                NodeStatus::SkippedEmpty
            } else {
                NodeStatus::Modified
            });
            Ok(())
        }
        ContextEdit::ReplaceStep { node, text } => {
            let kind = lookup_node_kind(state, *node)?;
            if kind != PathNodeKind::Step {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: kind,
                });
            }
            let entry = entry_mut(state, *node);
            let EntryData::Step { state } = &mut entry.data else {
                unreachable!("已确认节点类别为 Step");
            };
            if matches!(state, StepState::Omitted) {
                return Err(ContextError::NodeAlreadyOmitted { node: *node });
            }
            *state = StepState::Replaced(StepContent {
                assistant: NativeMessage::new(
                    MessageRole::Assistant,
                    None,
                    vec![MessagePart::semantic_only(MessageSemantic::Text {
                        text: text.as_str().to_owned(),
                    })],
                ),
                results: Vec::new(),
            });
            entry.status = Some(NodeStatus::Modified);
            Ok(())
        }
        ContextEdit::StripReasoning { node } => {
            let kind = lookup_node_kind(state, *node)?;
            if kind != PathNodeKind::Step {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: kind,
                });
            }
            let entry = entry_mut(state, *node);
            let (content, _) = active_step_mut(entry, *node)?;
            let status = sanitize_step_after_assistant_edit(content);
            entry.status = Some(status);
            Ok(())
        }
        ContextEdit::OmitInvocation { invocation } => {
            let owner = lookup_invocation_owner(state, *invocation)?;
            let entry = entry_mut(state, owner);
            let (content, omitted_invocations) = active_step_mut(entry, owner)?;
            if omitted_invocations.contains(invocation) {
                return Err(ContextError::InvocationAlreadyOmitted {
                    invocation: *invocation,
                });
            }
            omitted_invocations.insert(*invocation);
            content.assistant.parts.retain(|part| {
                !matches!(
                    &part.semantic,
                    MessageSemantic::ToolCall { call } if call.invocation_id == *invocation
                )
            });
            content
                .results
                .retain(|result| result.invocation_id != *invocation);
            let status = sanitize_step_after_assistant_edit(content);
            entry.status = Some(status);
            Ok(())
        }
        ContextEdit::ReplaceToolResult { invocation, text } => {
            let owner = lookup_invocation_owner(state, *invocation)?;
            let entry = entry_mut(state, owner);
            let (content, omitted_invocations) = active_step_mut(entry, owner)?;
            if omitted_invocations.contains(invocation) {
                return Err(ContextError::InvocationAlreadyOmitted {
                    invocation: *invocation,
                });
            }

            let result = content
                .results
                .iter_mut()
                .find(|item| item.invocation_id == *invocation)
                .expect("有效未移出的 InvocationId 必存在对应 StoredToolResult");
            match &mut result.payload {
                ResultPayload::Succeeded(_) => {
                    let value = serde_json::Value::String(text.as_str().to_owned());
                    let bytes = canonical_json(&value);
                    let digest = tool_content_digest(&bytes);
                    let byte_length = bytes.len() as u64;
                    result.payload = ResultPayload::Succeeded(SuccessContent::Json {
                        value,
                        digest,
                        byte_length,
                    });
                }
                ResultPayload::Failed(err) => {
                    err.message = text.as_str().to_owned();
                }
                ResultPayload::NotExecuted | ResultPayload::Indeterminate(_) => {
                    return Err(ContextError::ToolResultNotReplaceable {
                        invocation: *invocation,
                        outcome: result.payload.outcome(),
                    });
                }
            }
            // 仅修改客户端回填的工具结果，不改动助手回复；保留 `assistant` 的
            // `provider_origin`、`Reasoning` 与逐片段 `replay_state`（含当前轮工具调用签名）。
            entry.status = Some(NodeStatus::Modified);
            Ok(())
        }
    }
}

fn lookup_node_kind(state: &ResolveState, node: NodeId) -> Result<PathNodeKind, ContextError> {
    state
        .node_kinds
        .get(&node)
        .copied()
        .ok_or(ContextError::UnknownNode { node })
}

fn lookup_invocation_owner(
    state: &ResolveState,
    invocation: InvocationId,
) -> Result<NodeId, ContextError> {
    state
        .invocation_owner
        .get(&invocation)
        .copied()
        .ok_or(ContextError::UnknownInvocation { invocation })
}

fn entry_mut(state: &mut ResolveState, node: NodeId) -> &mut PathEntry {
    let index = *state
        .entry_index
        .get(&node)
        .expect("已核验的 Input/Step 节点必存在索引");
    &mut state.entries[index]
}

fn active_step_mut(
    entry: &mut PathEntry,
    node: NodeId,
) -> Result<(&mut StepContent, &mut BTreeSet<InvocationId>), ContextError> {
    let EntryData::Step { state } = &mut entry.data else {
        unreachable!("调用前已确认目标节点类别为 Step");
    };
    match state {
        StepState::Active {
            content,
            omitted_invocations,
        } => Ok((content, omitted_invocations)),
        StepState::Replaced(_) => Err(ContextError::StepAlreadyReplaced { node }),
        StepState::Omitted => Err(ContextError::NodeAlreadyOmitted { node }),
    }
}

fn push_merged_user_text(out: &mut Vec<MessagePart<BoundToolCall>>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(MessagePart {
        semantic: MessageSemantic::Text { text: prev },
        ..
    }) = out.last_mut()
    {
        prev.push_str(text);
    } else {
        out.push(MessagePart::semantic_only(MessageSemantic::Text {
            text: text.to_owned(),
        }));
    }
}

/// 仅在助手回复片段发生局部删改（`StripReasoning` / `OmitInvocation`）时执行的清洗路径：
/// 清除 `provider_origin` 与逐片段 `replay_state`、剥离无来源 `Reasoning` 与空 `Text` 片段、
/// 合并相邻 `Text` 并判定最终状态。
fn sanitize_step_after_assistant_edit(content: &mut StepContent) -> NodeStatus {
    content.assistant.provider_origin = None;

    let mut cleaned: Vec<MessagePart<BoundToolCall>> =
        Vec::with_capacity(content.assistant.parts.len());
    for mut part in content.assistant.parts.drain(..) {
        part.replay_state = None;
        match part.semantic {
            MessageSemantic::Text { text } => {
                if text.is_empty() {
                    continue;
                }
                if let Some(MessagePart {
                    semantic: MessageSemantic::Text { text: prev },
                    ..
                }) = cleaned.last_mut()
                {
                    prev.push_str(&text);
                } else {
                    cleaned.push(MessagePart::semantic_only(MessageSemantic::Text { text }));
                }
            }
            // `provider_origin` 已清除，下游跨来源投影会丢弃全部 `Reasoning`，
            // 此处同步剥离以避免仅剩推理的 Step 产出空助手消息或虚报 `reasoning_bytes`。
            MessageSemantic::Reasoning { .. } => continue,
            other => {
                cleaned.push(MessagePart::semantic_only(other));
            }
        }
    }
    content.assistant.parts = cleaned;

    if content.assistant.parts.is_empty() && content.results.is_empty() {
        NodeStatus::SkippedEmpty
    } else {
        NodeStatus::Modified
    }
}
