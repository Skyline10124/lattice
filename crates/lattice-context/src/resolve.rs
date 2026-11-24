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
use crate::project::entry_is_projected;
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
    projected_count: usize,
}

/// 沿根到当前位置单遍扫描归约得到的有效模型上下文。
#[derive(Clone, Debug, PartialEq)]
pub struct EffectiveContext {
    pub(crate) state: ResolveState,
}

/// 供 [`ContextCursor::rollback`] 使用的不透明单步回滚凭证。
#[derive(Debug)]
pub struct ContextUndo {
    kind: UndoKind,
}

#[derive(Debug)]
enum UndoKind {
    Input {
        id: NodeId,
    },
    Step {
        id: NodeId,
        invocations: Vec<InvocationId>,
    },
    Op {
        id: NodeId,
        snapshots: Vec<(usize, PathEntry)>,
    },
}

/// 支持增量推进与零前缀克隆回滚的模型上下文树遍历游标。
///
/// 推进、校验与编辑规则与 [`resolve`] 严格同源，供会话树单遍 DFS 校验按分支进退复用共享前缀状态。
#[derive(Clone, Debug, Default)]
pub struct ContextCursor {
    state: ResolveState,
}

impl ContextCursor {
    /// 创建位于根节点处的空游标。
    pub fn new() -> Self {
        Self::default()
    }

    /// 以 O(1) 判断当前游标路径投影后是否存在至少一条有效模型条目；
    /// 语义严格等价于 `!self.clone().into_effective().project().is_empty()`。
    pub fn has_projected_items(&self) -> bool {
        self.state.projected_count > 0
    }

    /// 沿路径推进单条记录并返回可逆回滚凭证；失败时保证游标状态零污染。
    pub fn advance(&mut self, record: PathRecord<'_>) -> Result<ContextUndo, ContextError> {
        match record {
            PathRecord::Input { id, input } => self.advance_input(id, input),
            PathRecord::Step { id, content } => self.advance_step(id, content),
            PathRecord::Op { id, op } => self.advance_op(id, op),
        }
    }

    /// 按与 [`Self::advance`] 严格的 LIFO 顺序撤销单步推进，恢复到父节点状态。
    pub fn rollback(&mut self, undo: ContextUndo) {
        match undo.kind {
            UndoKind::Input { id } => {
                let popped = self
                    .state
                    .entries
                    .pop()
                    .expect("rollback 顺序必须与 advance 严格 LIFO 对应");
                assert_eq!(popped.node, id, "rollback 弹出的 Input 节点与凭证不匹配");
                if entry_is_projected(&popped) {
                    decrement_projected_count(&mut self.state.projected_count);
                }
                let removed_idx = self.state.entry_index.remove(&id);
                assert_eq!(removed_idx, Some(self.state.entries.len()));
                let removed_kind = self.state.node_kinds.remove(&id);
                assert_eq!(removed_kind, Some(PathNodeKind::Input));
            }
            UndoKind::Step { id, invocations } => {
                let popped = self
                    .state
                    .entries
                    .pop()
                    .expect("rollback 顺序必须与 advance 严格 LIFO 对应");
                assert_eq!(popped.node, id, "rollback 弹出的 Step 节点与凭证不匹配");
                if entry_is_projected(&popped) {
                    decrement_projected_count(&mut self.state.projected_count);
                }
                let removed_idx = self.state.entry_index.remove(&id);
                assert_eq!(removed_idx, Some(self.state.entries.len()));
                let removed_kind = self.state.node_kinds.remove(&id);
                assert_eq!(removed_kind, Some(PathNodeKind::Step));
                for invocation in invocations {
                    let removed_owner = self.state.invocation_owner.remove(&invocation);
                    assert_eq!(removed_owner, Some(id));
                }
            }
            UndoKind::Op { id, snapshots } => {
                let removed_kind = self.state.node_kinds.remove(&id);
                assert_eq!(
                    removed_kind,
                    Some(PathNodeKind::Op),
                    "rollback 的 Op 节点与凭证不匹配"
                );
                restore_entry_snapshots(&mut self.state, snapshots);
            }
        }
    }

    /// 消费当前游标并产出只读的 [`EffectiveContext`]。
    pub fn into_effective(self) -> EffectiveContext {
        EffectiveContext { state: self.state }
    }

    fn advance_ref(&mut self, record: &PathRecord<'_>) -> Result<ContextUndo, ContextError> {
        match record {
            PathRecord::Input { id, input } => self.advance_input(*id, input),
            PathRecord::Step { id, content } => self.advance_step(*id, content.clone()),
            PathRecord::Op { id, op } => self.advance_op(*id, op),
        }
    }

    fn advance_input(
        &mut self,
        id: NodeId,
        input: &ContextInput,
    ) -> Result<ContextUndo, ContextError> {
        ensure_unique_node(&self.state, id)?;
        let entry = PathEntry {
            node: id,
            data: EntryData::Input {
                channel: input.channel().clone(),
                state: InputState::Active {
                    parts: input.message().parts.clone(),
                },
            },
            status: None,
        };
        let index = self.state.entries.len();
        if entry_is_projected(&entry) {
            self.state.projected_count += 1;
        }
        self.state.entries.push(entry);
        self.state.entry_index.insert(id, index);
        self.state.node_kinds.insert(id, PathNodeKind::Input);
        Ok(ContextUndo {
            kind: UndoKind::Input { id },
        })
    }

    fn advance_step(
        &mut self,
        id: NodeId,
        content: StepContent,
    ) -> Result<ContextUndo, ContextError> {
        ensure_unique_node(&self.state, id)?;
        let invocations = validate_step_entrance(&self.state, id, &content)?;
        let entry = PathEntry {
            node: id,
            data: EntryData::Step {
                state: StepState::Active {
                    content,
                    omitted_invocations: BTreeSet::new(),
                },
            },
            status: None,
        };
        let index = self.state.entries.len();
        if entry_is_projected(&entry) {
            self.state.projected_count += 1;
        }
        self.state.entries.push(entry);
        self.state.entry_index.insert(id, index);
        self.state.node_kinds.insert(id, PathNodeKind::Step);
        for &invocation in &invocations {
            self.state.invocation_owner.insert(invocation, id);
        }
        Ok(ContextUndo {
            kind: UndoKind::Step { id, invocations },
        })
    }

    fn advance_op(&mut self, id: NodeId, op: &ContextOp) -> Result<ContextUndo, ContextError> {
        ensure_unique_node(&self.state, id)?;
        // 在登记本 Op 自身前执行应用，保证编辑只能引用本 Op 之前出现的路径节点；
        // 若多编辑批中途失败，apply_op_reversible 逆序恢复全部快照，确保零污染。
        let snapshots = apply_op_reversible(&mut self.state, op)?;
        self.state.node_kinds.insert(id, PathNodeKind::Op);
        Ok(ContextUndo {
            kind: UndoKind::Op { id, snapshots },
        })
    }
}

/// 对沿根到当前位置收集的顺序路径记录执行单遍扫描归约。
pub fn resolve(path: &[PathRecord<'_>]) -> Result<EffectiveContext, ContextError> {
    let mut cursor = ContextCursor::new();
    for record in path {
        cursor.advance_ref(record)?;
    }
    Ok(cursor.into_effective())
}

impl EffectiveContext {
    /// 只读校验待落树的 [`ContextOp`] 是否对当前有效上下文合法；与 [`resolve`] 共用同一应用规则且无副作用。
    pub fn validate_op(&self, op: &ContextOp) -> Result<(), ContextError> {
        let mut preview = self.state.clone();
        for edit in op.edits() {
            apply_edit(&mut preview, edit, None)?;
        }
        Ok(())
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

fn apply_op_reversible(
    state: &mut ResolveState,
    op: &ContextOp,
) -> Result<Vec<(usize, PathEntry)>, ContextError> {
    let mut snapshots = Vec::with_capacity(op.edits().len());
    for edit in op.edits() {
        if let Err(err) = apply_edit(state, edit, Some(&mut snapshots)) {
            restore_entry_snapshots(state, snapshots);
            return Err(err);
        }
    }
    Ok(snapshots)
}

fn restore_entry_snapshots(state: &mut ResolveState, snapshots: Vec<(usize, PathEntry)>) {
    for (idx, old_entry) in snapshots.into_iter().rev() {
        let before = entry_is_projected(&state.entries[idx]);
        let after = entry_is_projected(&old_entry);
        state.entries[idx] = old_entry;
        adjust_projected_count(&mut state.projected_count, before, after);
    }
}

fn decrement_projected_count(projected_count: &mut usize) {
    *projected_count = projected_count
        .checked_sub(1)
        .expect("projected_count 下溢：有效投影条目计数不一致");
}

fn adjust_projected_count(projected_count: &mut usize, before: bool, after: bool) {
    match (before, after) {
        (false, true) => *projected_count += 1,
        (true, false) => decrement_projected_count(projected_count),
        (false, false) | (true, true) => {}
    }
}

fn mutate_entry(
    state: &mut ResolveState,
    idx: usize,
    undo: Option<&mut Vec<(usize, PathEntry)>>,
    mutate: impl FnOnce(&mut PathEntry),
) {
    if let Some(snapshots) = undo {
        snapshots.push((idx, state.entries[idx].clone()));
    }
    let before = entry_is_projected(&state.entries[idx]);
    mutate(&mut state.entries[idx]);
    let after = entry_is_projected(&state.entries[idx]);
    adjust_projected_count(&mut state.projected_count, before, after);
}

fn apply_edit(
    state: &mut ResolveState,
    edit: &ContextEdit,
    undo: Option<&mut Vec<(usize, PathEntry)>>,
) -> Result<(), ContextError> {
    match edit {
        ContextEdit::OmitNode { node } => {
            let kind = lookup_node_kind(state, *node)?;
            if kind == PathNodeKind::Op {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: PathNodeKind::Op,
                });
            }
            let idx = entry_index_of(state, *node);
            match &state.entries[idx].data {
                EntryData::Input {
                    state: InputState::Omitted,
                    ..
                }
                | EntryData::Step {
                    state: StepState::Omitted,
                } => return Err(ContextError::NodeAlreadyOmitted { node: *node }),
                EntryData::Input { .. } | EntryData::Step { .. } => {}
            }
            mutate_entry(state, idx, undo, |entry| {
                match &mut entry.data {
                    EntryData::Input { state, .. } => *state = InputState::Omitted,
                    EntryData::Step { state } => *state = StepState::Omitted,
                }
                entry.status = Some(NodeStatus::Omitted);
            });
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
            let idx = entry_index_of(state, *node);
            let EntryData::Input {
                state: input_state, ..
            } = &state.entries[idx].data
            else {
                unreachable!("已确认节点类别为 Input");
            };
            if matches!(input_state, InputState::Omitted) {
                return Err(ContextError::NodeAlreadyOmitted { node: *node });
            }
            mutate_entry(state, idx, undo, |entry| {
                let EntryData::Input { state, .. } = &mut entry.data else {
                    unreachable!("已确认节点类别为 Input");
                };
                *state = InputState::Active {
                    parts: message.as_message().parts.clone(),
                };
                entry.status = Some(NodeStatus::Modified);
            });
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
            let idx = entry_index_of(state, *node);
            let EntryData::Input {
                state: input_state, ..
            } = &state.entries[idx].data
            else {
                unreachable!("已确认节点类别为 Input");
            };
            let InputState::Active { parts } = input_state else {
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
            mutate_entry(state, idx, undo, |entry| {
                let EntryData::Input {
                    state: InputState::Active { parts },
                    ..
                } = &mut entry.data
                else {
                    unreachable!("已确认目标为有效 Input");
                };
                *parts = updated;
                entry.status = Some(if parts.is_empty() {
                    NodeStatus::SkippedEmpty
                } else {
                    NodeStatus::Modified
                });
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
            let idx = entry_index_of(state, *node);
            let EntryData::Step { state: step_state } = &state.entries[idx].data else {
                unreachable!("已确认节点类别为 Step");
            };
            if matches!(step_state, StepState::Omitted) {
                return Err(ContextError::NodeAlreadyOmitted { node: *node });
            }
            mutate_entry(state, idx, undo, |entry| {
                let EntryData::Step { state } = &mut entry.data else {
                    unreachable!("已确认节点类别为 Step");
                };
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
            });
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
            let idx = entry_index_of(state, *node);
            check_active_step(&state.entries[idx], *node)?;
            mutate_entry(state, idx, undo, |entry| {
                let (content, _) = active_step_mut_unchecked(entry);
                let status = sanitize_step_after_assistant_edit(content);
                entry.status = Some(status);
            });
            Ok(())
        }
        ContextEdit::OmitInvocation { invocation } => {
            let owner = lookup_invocation_owner(state, *invocation)?;
            let idx = entry_index_of(state, owner);
            let (_, omitted_invocations) = check_active_step(&state.entries[idx], owner)?;
            if omitted_invocations.contains(invocation) {
                return Err(ContextError::InvocationAlreadyOmitted {
                    invocation: *invocation,
                });
            }
            mutate_entry(state, idx, undo, |entry| {
                let (content, omitted_invocations) = active_step_mut_unchecked(entry);
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
            });
            Ok(())
        }
        ContextEdit::ReplaceToolResult { invocation, text } => {
            let owner = lookup_invocation_owner(state, *invocation)?;
            let idx = entry_index_of(state, owner);
            let (content, omitted_invocations) = check_active_step(&state.entries[idx], owner)?;
            if omitted_invocations.contains(invocation) {
                return Err(ContextError::InvocationAlreadyOmitted {
                    invocation: *invocation,
                });
            }

            let result = content
                .results
                .iter()
                .find(|item| item.invocation_id == *invocation)
                .expect("有效未移出的 InvocationId 必存在对应 StoredToolResult");
            if matches!(
                result.payload,
                ResultPayload::NotExecuted | ResultPayload::Indeterminate(_)
            ) {
                return Err(ContextError::ToolResultNotReplaceable {
                    invocation: *invocation,
                    outcome: result.payload.outcome(),
                });
            }

            mutate_entry(state, idx, undo, |entry| {
                let (content, _) = active_step_mut_unchecked(entry);
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
                        unreachable!("进入写回前已校验工具结果结局可替换")
                    }
                }
                // 仅修改客户端回填的工具结果，不改动助手回复；保留 `assistant` 的
                // `provider_origin`、`Reasoning` 与逐片段 `replay_state`（含当前轮工具调用签名）。
                entry.status = Some(NodeStatus::Modified);
            });
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

fn entry_index_of(state: &ResolveState, node: NodeId) -> usize {
    *state
        .entry_index
        .get(&node)
        .expect("已核验的 Input/Step 节点必存在索引")
}

fn check_active_step(
    entry: &PathEntry,
    node: NodeId,
) -> Result<(&StepContent, &BTreeSet<InvocationId>), ContextError> {
    let EntryData::Step { state } = &entry.data else {
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

fn active_step_mut_unchecked(
    entry: &mut PathEntry,
) -> (&mut StepContent, &mut BTreeSet<InvocationId>) {
    let EntryData::Step {
        state: StepState::Active {
            content,
            omitted_invocations,
        },
    } = &mut entry.data
    else {
        unreachable!("已核验目标节点为 Active Step");
    };
    (content, omitted_invocations)
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
