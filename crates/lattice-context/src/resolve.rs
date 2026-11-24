// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 沿根到当前位置的单遍扫描归约 `resolve()` 与只读操作校验 `validate_op()`（ADR 0014 第 4 节）。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::mem;

use lattice_conversation::{
    BoundToolCall, ImageRef, InvocationId, MessagePart, MessageRole, MessageSemantic,
    NativeMessage, NodeId, ReplayOrigin, ResultPayload, StepContent, StoredToolResult,
    SuccessContent, ToolError, ToolResultOutcome, canonical_json, tool_content_digest,
    validate_image_placement,
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

type SavedAssistant = (Option<ReplayOrigin>, Vec<MessagePart<BoundToolCall>>);

#[derive(Debug)]
struct EditUndo {
    idx: usize,
    old_status: Option<NodeStatus>,
    detail: EditUndoDetail,
}

#[derive(Debug)]
enum EditUndoDetail {
    InputState(InputState),
    StepState(StepState),
    ToolResultPayload {
        result_idx: usize,
        old_payload: ResultPayload,
    },
    StripReasoning {
        old_provider_origin: Option<ReplayOrigin>,
        old_parts: Vec<MessagePart<BoundToolCall>>,
    },
    OmitInvocation {
        invocation: InvocationId,
        old_assistant: Option<SavedAssistant>,
        result_idx: usize,
        removed_result: StoredToolResult,
    },
}

/// 单次 `ContextOp` 粒度的首次保存去重跟踪器：
/// 确保同一 `ContextOp` 对同一 `Input`、同一 `Step` 助手回复或同一工具结果连续施加多次编辑时，
/// 至多保存一次初始状态，后续编辑原地移动零重复克隆。
#[derive(Default)]
struct OpUndoTracker {
    saved_inputs: BTreeSet<usize>,
    saved_assistants: BTreeSet<usize>,
    saved_tool_results: BTreeSet<(usize, InvocationId)>,
}

struct OpUndoRecorder<'a> {
    tracker: &'a mut OpUndoTracker,
    undos: &'a mut Vec<EditUndo>,
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
        undos: Vec<EditUndo>,
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
            PathRecord::Input { id, input } => {
                self.push_input(id, input)?;
                Ok(ContextUndo {
                    kind: UndoKind::Input { id },
                })
            }
            PathRecord::Step { id, content } => {
                let invocations = self.push_step(id, content)?;
                Ok(ContextUndo {
                    kind: UndoKind::Step { id, invocations },
                })
            }
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
            UndoKind::Op { id, undos } => {
                let removed_kind = self.state.node_kinds.remove(&id);
                assert_eq!(
                    removed_kind,
                    Some(PathNodeKind::Op),
                    "rollback 的 Op 节点与凭证不匹配"
                );
                restore_edit_undos(&mut self.state, undos);
            }
        }
    }

    /// 消费当前游标并产出只读的 [`EffectiveContext`]。
    pub fn into_effective(self) -> EffectiveContext {
        EffectiveContext { state: self.state }
    }

    fn push_input(&mut self, id: NodeId, input: &ContextInput) -> Result<(), ContextError> {
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
        Ok(())
    }

    fn push_step(
        &mut self,
        id: NodeId,
        content: StepContent,
    ) -> Result<Vec<InvocationId>, ContextError> {
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
        Ok(invocations)
    }

    fn advance_op(&mut self, id: NodeId, op: &ContextOp) -> Result<ContextUndo, ContextError> {
        ensure_unique_node(&self.state, id)?;
        // 在登记本 Op 自身前执行应用，保证编辑只能引用本 Op 之前出现的路径节点；
        // 若多编辑批中途失败，apply_op_reversible 逆序恢复全部字段级回滚项，确保零污染。
        let undos = apply_op_reversible(&mut self.state, op)?;
        self.state.node_kinds.insert(id, PathNodeKind::Op);
        Ok(ContextUndo {
            kind: UndoKind::Op { id, undos },
        })
    }
}

/// 对沿根到当前位置收集的顺序路径记录执行单遍扫描归约。
pub fn resolve(path: &[PathRecord<'_>]) -> Result<EffectiveContext, ContextError> {
    let mut cursor = ContextCursor::new();
    for record in path {
        match record {
            PathRecord::Input { id, input } => cursor.push_input(*id, input)?,
            PathRecord::Step { id, content } => {
                cursor.push_step(*id, content.clone())?;
            }
            PathRecord::Op { id, op } => {
                ensure_unique_node(&cursor.state, *id)?;
                // 普通归约遇错即丢弃局部状态，无需记录回滚项，直接复用同一 apply_edit 原地演进。
                for edit in op.edits() {
                    apply_edit(&mut cursor.state, edit, None)?;
                }
                cursor.state.node_kinds.insert(*id, PathNodeKind::Op);
            }
        }
    }
    Ok(cursor.into_effective())
}

impl EffectiveContext {
    /// 只读校验待落树的 [`ContextOp`] 是否对当前有效上下文合法；与 [`resolve`] 共用同一应用规则且无副作用。
    pub fn validate_op(&self, op: &ContextOp) -> Result<(), ContextError> {
        let mut overlay = ValidateOverlay::new(&self.state);
        for edit in op.edits() {
            apply_edit(&mut overlay, edit, None)?;
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

/// 供 `apply_edit` 统一访问路径条目的内部抽象：
/// - `ResolveState` 直接原地修改并同步维护 `projected_count`；
/// - `ValidateOverlay` 仅在编辑校验通过且首次写入某条目时对该单条目按需 CoW 克隆，
///   避免 `validate_op` 克隆整会话状态或重复克隆同一目标 Step。
trait EditTarget {
    fn node_kind(&self, node: NodeId) -> Option<PathNodeKind>;
    fn invocation_owner(&self, invocation: InvocationId) -> Option<NodeId>;
    fn entry_index(&self, node: NodeId) -> usize;
    fn entry(&self, idx: usize) -> &PathEntry;
    fn mutate_entry<R>(&mut self, idx: usize, mutate: impl FnOnce(&mut PathEntry) -> R) -> R;
}

impl EditTarget for ResolveState {
    fn node_kind(&self, node: NodeId) -> Option<PathNodeKind> {
        self.node_kinds.get(&node).copied()
    }

    fn invocation_owner(&self, invocation: InvocationId) -> Option<NodeId> {
        self.invocation_owner.get(&invocation).copied()
    }

    fn entry_index(&self, node: NodeId) -> usize {
        *self
            .entry_index
            .get(&node)
            .expect("已核验的 Input/Step 节点必存在索引")
    }

    fn entry(&self, idx: usize) -> &PathEntry {
        &self.entries[idx]
    }

    fn mutate_entry<R>(&mut self, idx: usize, mutate: impl FnOnce(&mut PathEntry) -> R) -> R {
        let before = entry_is_projected(&self.entries[idx]);
        let ret = mutate(&mut self.entries[idx]);
        let after = entry_is_projected(&self.entries[idx]);
        adjust_projected_count(&mut self.projected_count, before, after);
        ret
    }
}

struct ValidateOverlay<'a> {
    base: &'a ResolveState,
    overrides: BTreeMap<usize, PathEntry>,
}

impl<'a> ValidateOverlay<'a> {
    fn new(base: &'a ResolveState) -> Self {
        Self {
            base,
            overrides: BTreeMap::new(),
        }
    }
}

impl EditTarget for ValidateOverlay<'_> {
    fn node_kind(&self, node: NodeId) -> Option<PathNodeKind> {
        self.base.node_kind(node)
    }

    fn invocation_owner(&self, invocation: InvocationId) -> Option<NodeId> {
        self.base.invocation_owner(invocation)
    }

    fn entry_index(&self, node: NodeId) -> usize {
        self.base.entry_index(node)
    }

    fn entry(&self, idx: usize) -> &PathEntry {
        self.overrides.get(&idx).unwrap_or(&self.base.entries[idx])
    }

    fn mutate_entry<R>(&mut self, idx: usize, mutate: impl FnOnce(&mut PathEntry) -> R) -> R {
        let entry = self
            .overrides
            .entry(idx)
            .or_insert_with(|| self.base.entries[idx].clone());
        mutate(entry)
    }
}

fn apply_op_reversible(
    state: &mut ResolveState,
    op: &ContextOp,
) -> Result<Vec<EditUndo>, ContextError> {
    let mut tracker = OpUndoTracker::default();
    let mut undos = Vec::with_capacity(op.edits().len());
    for edit in op.edits() {
        if let Err(err) = apply_edit(
            state,
            edit,
            Some(OpUndoRecorder {
                tracker: &mut tracker,
                undos: &mut undos,
            }),
        ) {
            restore_edit_undos(state, undos);
            return Err(err);
        }
    }
    Ok(undos)
}

fn restore_edit_undos(state: &mut ResolveState, undos: Vec<EditUndo>) {
    for undo in undos.into_iter().rev() {
        state.mutate_entry(undo.idx, |entry| {
            entry.status = undo.old_status;
            match undo.detail {
                EditUndoDetail::InputState(old) => {
                    let EntryData::Input { state, .. } = &mut entry.data else {
                        unreachable!("回滚 InputState 目标必须为 Input 条目");
                    };
                    *state = old;
                }
                EditUndoDetail::StepState(old) => {
                    let EntryData::Step { state } = &mut entry.data else {
                        unreachable!("回滚 StepState 目标必须为 Step 条目");
                    };
                    *state = old;
                }
                EditUndoDetail::ToolResultPayload {
                    result_idx,
                    old_payload,
                } => {
                    let (content, _) = active_step_mut_unchecked(entry);
                    content.results[result_idx].payload = old_payload;
                }
                EditUndoDetail::StripReasoning {
                    old_provider_origin,
                    old_parts,
                } => {
                    let (content, _) = active_step_mut_unchecked(entry);
                    content.assistant.provider_origin = old_provider_origin;
                    content.assistant.parts = old_parts;
                }
                EditUndoDetail::OmitInvocation {
                    invocation,
                    old_assistant,
                    result_idx,
                    removed_result,
                } => {
                    let (content, omitted_invocations) = active_step_mut_unchecked(entry);
                    let removed = omitted_invocations.remove(&invocation);
                    assert!(
                        removed,
                        "回滚 OmitInvocation 时目标调用必须处于已移出集合中"
                    );
                    content.results.insert(result_idx, removed_result);
                    if let Some((old_provider_origin, old_parts)) = old_assistant {
                        content.assistant.provider_origin = old_provider_origin;
                        content.assistant.parts = old_parts;
                    }
                }
            }
        });
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

fn apply_edit<T: EditTarget>(
    target: &mut T,
    edit: &ContextEdit,
    mut recorder: Option<OpUndoRecorder<'_>>,
) -> Result<(), ContextError> {
    let recorded = match edit {
        ContextEdit::OmitNode { node } => {
            let kind = lookup_node_kind(target, *node)?;
            if kind == PathNodeKind::Op {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: PathNodeKind::Op,
                });
            }
            let idx = target.entry_index(*node);
            match &target.entry(idx).data {
                EntryData::Input {
                    state: InputState::Omitted,
                    ..
                }
                | EntryData::Step {
                    state: StepState::Omitted,
                } => return Err(ContextError::NodeAlreadyOmitted { node: *node }),
                EntryData::Input { .. } | EntryData::Step { .. } => {}
            }
            let record_step = recorder.is_some();
            let save_input = kind == PathNodeKind::Input
                && recorder
                    .as_mut()
                    .is_some_and(|r| r.tracker.saved_inputs.insert(idx));
            target.mutate_entry(idx, |entry| {
                let old_status = entry.status.replace(NodeStatus::Omitted);
                let detail = match &mut entry.data {
                    EntryData::Input { state, .. } => {
                        let old = mem::replace(state, InputState::Omitted);
                        save_input.then_some(EditUndoDetail::InputState(old))
                    }
                    EntryData::Step { state } => {
                        let old = mem::replace(state, StepState::Omitted);
                        record_step.then_some(EditUndoDetail::StepState(old))
                    }
                };
                detail.map(|detail| EditUndo {
                    idx,
                    old_status,
                    detail,
                })
            })
        }
        ContextEdit::ReplaceInput { node, message } => {
            let kind = lookup_node_kind(target, *node)?;
            if kind != PathNodeKind::Input {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: kind,
                });
            }
            let idx = target.entry_index(*node);
            let EntryData::Input {
                state: input_state, ..
            } = &target.entry(idx).data
            else {
                unreachable!("已确认节点类别为 Input");
            };
            if matches!(input_state, InputState::Omitted) {
                return Err(ContextError::NodeAlreadyOmitted { node: *node });
            }
            let save_input = recorder
                .as_mut()
                .is_some_and(|r| r.tracker.saved_inputs.insert(idx));
            target.mutate_entry(idx, |entry| {
                let old_status = entry.status.replace(NodeStatus::Modified);
                let EntryData::Input { state, .. } = &mut entry.data else {
                    unreachable!("已确认节点类别为 Input");
                };
                let old = mem::replace(
                    state,
                    InputState::Active {
                        parts: message.as_message().parts.clone(),
                    },
                );
                save_input.then_some(EditUndo {
                    idx,
                    old_status,
                    detail: EditUndoDetail::InputState(old),
                })
            })
        }
        ContextEdit::ReplaceImage { node, image, text } => {
            let kind = lookup_node_kind(target, *node)?;
            if kind != PathNodeKind::Input {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: kind,
                });
            }
            let idx = target.entry_index(*node);
            {
                let EntryData::Input {
                    state: input_state, ..
                } = &target.entry(idx).data
                else {
                    unreachable!("已确认节点类别为 Input");
                };
                let InputState::Active { parts } = input_state else {
                    return Err(ContextError::NodeAlreadyOmitted { node: *node });
                };
                // 进入写回前仅做零分配存在性检查，未命中时不拷贝任何片段文本。
                if !parts.iter().any(|part| {
                    matches!(
                        &part.semantic,
                        MessageSemantic::Image { image: candidate } if candidate == image
                    )
                }) {
                    return Err(ContextError::ImageNotFound {
                        node: *node,
                        image: *image,
                    });
                }
            }
            let save_input = recorder
                .as_mut()
                .is_some_and(|r| r.tracker.saved_inputs.insert(idx));
            target.mutate_entry(idx, |entry| {
                let EntryData::Input {
                    state: InputState::Active { parts },
                    ..
                } = &mut entry.data
                else {
                    unreachable!("已确认目标为有效 Input");
                };
                let saved_old_parts = if save_input {
                    let old_parts = mem::take(parts);
                    let mut updated: Vec<MessagePart<BoundToolCall>> =
                        Vec::with_capacity(old_parts.len());
                    for part in &old_parts {
                        match &part.semantic {
                            MessageSemantic::Image { image: candidate } if candidate == image => {
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
                            MessageSemantic::Reasoning { .. }
                            | MessageSemantic::ToolCall { .. } => {
                                unreachable!("ContextInput 仅包含 Text 与 Image 片段")
                            }
                        }
                    }
                    *parts = updated;
                    Some(old_parts)
                } else {
                    let mut updated: Vec<MessagePart<BoundToolCall>> =
                        Vec::with_capacity(parts.len());
                    for part in parts.drain(..) {
                        match part.semantic {
                            MessageSemantic::Image { image: candidate } if &candidate == image => {
                                if let Some(replacement) = text {
                                    push_merged_user_text(&mut updated, replacement.as_str());
                                }
                            }
                            MessageSemantic::Text { text: existing } => {
                                push_owned_merged_user_text(&mut updated, existing);
                            }
                            MessageSemantic::Image { image: other } => {
                                updated.push(MessagePart::semantic_only(MessageSemantic::Image {
                                    image: other,
                                }));
                            }
                            MessageSemantic::Reasoning { .. }
                            | MessageSemantic::ToolCall { .. } => {
                                unreachable!("ContextInput 仅包含 Text 与 Image 片段")
                            }
                        }
                    }
                    *parts = updated;
                    None
                };
                let new_status = if parts.is_empty() {
                    NodeStatus::SkippedEmpty
                } else {
                    NodeStatus::Modified
                };
                let old_status = entry.status.replace(new_status);
                saved_old_parts.map(|old_parts| EditUndo {
                    idx,
                    old_status,
                    detail: EditUndoDetail::InputState(InputState::Active { parts: old_parts }),
                })
            })
        }
        ContextEdit::ReplaceStep { node, text } => {
            let kind = lookup_node_kind(target, *node)?;
            if kind != PathNodeKind::Step {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: kind,
                });
            }
            let idx = target.entry_index(*node);
            let EntryData::Step { state: step_state } = &target.entry(idx).data else {
                unreachable!("已确认节点类别为 Step");
            };
            if matches!(step_state, StepState::Omitted) {
                return Err(ContextError::NodeAlreadyOmitted { node: *node });
            }
            let record_step = recorder.is_some();
            target.mutate_entry(idx, |entry| {
                let old_status = entry.status.replace(NodeStatus::Modified);
                let EntryData::Step { state } = &mut entry.data else {
                    unreachable!("已确认节点类别为 Step");
                };
                let old = mem::replace(
                    state,
                    StepState::Replaced(StepContent {
                        assistant: NativeMessage::new(
                            MessageRole::Assistant,
                            None,
                            vec![MessagePart::semantic_only(MessageSemantic::Text {
                                text: text.as_str().to_owned(),
                            })],
                        ),
                        results: Vec::new(),
                    }),
                );
                record_step.then_some(EditUndo {
                    idx,
                    old_status,
                    detail: EditUndoDetail::StepState(old),
                })
            })
        }
        ContextEdit::StripReasoning { node } => {
            let kind = lookup_node_kind(target, *node)?;
            if kind != PathNodeKind::Step {
                return Err(ContextError::NodeKindMismatch {
                    node: *node,
                    actual: kind,
                });
            }
            let idx = target.entry_index(*node);
            check_active_step(target.entry(idx), *node)?;
            let save_assistant = recorder
                .as_mut()
                .is_some_and(|r| r.tracker.saved_assistants.insert(idx));
            target.mutate_entry(idx, |entry| {
                let (content, _) = active_step_mut_unchecked(entry);
                let (status, saved) =
                    sanitize_assistant_parts_for_edit(content, None, save_assistant);
                let old_status = entry.status.replace(status);
                saved.map(|(old_provider_origin, old_parts)| EditUndo {
                    idx,
                    old_status,
                    detail: EditUndoDetail::StripReasoning {
                        old_provider_origin,
                        old_parts,
                    },
                })
            })
        }
        ContextEdit::OmitInvocation { invocation } => {
            let owner = lookup_invocation_owner(target, *invocation)?;
            let idx = target.entry_index(owner);
            let result_idx = {
                let (content, omitted_invocations) = check_active_step(target.entry(idx), owner)?;
                if omitted_invocations.contains(invocation) {
                    return Err(ContextError::InvocationAlreadyOmitted {
                        invocation: *invocation,
                    });
                }
                content
                    .results
                    .iter()
                    .position(|result| result.invocation_id == *invocation)
                    .expect("有效未移出的 InvocationId 必存在对应 StoredToolResult")
            };
            let record_undo = recorder.is_some();
            let save_assistant = recorder
                .as_mut()
                .is_some_and(|r| r.tracker.saved_assistants.insert(idx));
            target.mutate_entry(idx, |entry| {
                let (content, omitted_invocations) = active_step_mut_unchecked(entry);
                omitted_invocations.insert(*invocation);
                let removed_result = content.results.remove(result_idx);
                let (status, old_assistant) =
                    sanitize_assistant_parts_for_edit(content, Some(*invocation), save_assistant);
                let old_status = entry.status.replace(status);
                record_undo.then_some(EditUndo {
                    idx,
                    old_status,
                    detail: EditUndoDetail::OmitInvocation {
                        invocation: *invocation,
                        old_assistant,
                        result_idx,
                        removed_result,
                    },
                })
            })
        }
        ContextEdit::ReplaceToolResult { invocation, text } => {
            let owner = lookup_invocation_owner(target, *invocation)?;
            let idx = target.entry_index(owner);
            let (result_idx, new_payload) = {
                let (content, omitted_invocations) = check_active_step(target.entry(idx), owner)?;
                if omitted_invocations.contains(invocation) {
                    return Err(ContextError::InvocationAlreadyOmitted {
                        invocation: *invocation,
                    });
                }
                let (result_idx, result) = content
                    .results
                    .iter()
                    .enumerate()
                    .find(|(_, item)| item.invocation_id == *invocation)
                    .expect("有效未移出的 InvocationId 必存在对应 StoredToolResult");
                let new_payload = match &result.payload {
                    ResultPayload::Succeeded(_) => {
                        let value = serde_json::Value::String(text.as_str().to_owned());
                        let bytes = canonical_json(&value);
                        let digest = tool_content_digest(&bytes);
                        let byte_length = bytes.len() as u64;
                        ResultPayload::Succeeded(SuccessContent::Json {
                            value,
                            digest,
                            byte_length,
                        })
                    }
                    ResultPayload::Failed(err) => ResultPayload::Failed(ToolError {
                        code: err.code.clone(),
                        message: text.as_str().to_owned(),
                    }),
                    ResultPayload::NotExecuted | ResultPayload::Indeterminate(_) => {
                        return Err(ContextError::ToolResultNotReplaceable {
                            invocation: *invocation,
                            outcome: result.payload.outcome(),
                        });
                    }
                };
                (result_idx, new_payload)
            };

            let save_tool_result = recorder
                .as_mut()
                .is_some_and(|r| r.tracker.saved_tool_results.insert((idx, *invocation)));
            // 仅移动替换目标工具结果载荷，不触碰也不克隆助手回复与其他工具结果；
            // 保留 `assistant` 的 `provider_origin`、`Reasoning` 与逐片段 `replay_state`。
            target.mutate_entry(idx, |entry| {
                let old_status = entry.status.replace(NodeStatus::Modified);
                let (content, _) = active_step_mut_unchecked(entry);
                let old_payload =
                    mem::replace(&mut content.results[result_idx].payload, new_payload);
                save_tool_result.then_some(EditUndo {
                    idx,
                    old_status,
                    detail: EditUndoDetail::ToolResultPayload {
                        result_idx,
                        old_payload,
                    },
                })
            })
        }
    };

    if let (Some(recorder), Some(item)) = (recorder, recorded) {
        recorder.undos.push(item);
    }
    Ok(())
}

fn lookup_node_kind<T: EditTarget>(target: &T, node: NodeId) -> Result<PathNodeKind, ContextError> {
    target
        .node_kind(node)
        .ok_or(ContextError::UnknownNode { node })
}

fn lookup_invocation_owner<T: EditTarget>(
    target: &T,
    invocation: InvocationId,
) -> Result<NodeId, ContextError> {
    target
        .invocation_owner(invocation)
        .ok_or(ContextError::UnknownInvocation { invocation })
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

fn push_owned_merged_user_text(out: &mut Vec<MessagePart<BoundToolCall>>, text: String) {
    if text.is_empty() {
        return;
    }
    if let Some(MessagePart {
        semantic: MessageSemantic::Text { text: prev },
        ..
    }) = out.last_mut()
    {
        prev.push_str(&text);
    } else {
        out.push(MessagePart::semantic_only(MessageSemantic::Text { text }));
    }
}

fn push_cleaned_assistant_semantic(
    cleaned: &mut Vec<MessagePart<BoundToolCall>>,
    semantic: MessageSemantic<BoundToolCall>,
) {
    match semantic {
        MessageSemantic::Text { text } => {
            if text.is_empty() {
                return;
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
        MessageSemantic::Reasoning { .. } => {}
        other => {
            cleaned.push(MessagePart::semantic_only(other));
        }
    }
}

/// 仅在助手回复片段发生局部删改（`StripReasoning` / `OmitInvocation`）时执行的清洗路径：
/// 清除 `provider_origin` 与逐片段 `replay_state`、按需剔除目标工具调用、剥离无来源 `Reasoning`
/// 与空 `Text` 片段、合并相邻 `Text` 并判定最终状态；
/// `save_assistant == false` 时原地 `drain` 零克隆移动，`save_assistant == true` 时移出原片段供回滚恢复。
fn sanitize_assistant_parts_for_edit(
    content: &mut StepContent,
    omit_invocation: Option<InvocationId>,
    save_assistant: bool,
) -> (NodeStatus, Option<SavedAssistant>) {
    let old_origin = content.assistant.provider_origin.take();
    let mut cleaned: Vec<MessagePart<BoundToolCall>> =
        Vec::with_capacity(content.assistant.parts.len());

    let saved = if save_assistant {
        let old_parts = mem::take(&mut content.assistant.parts);
        for part in &old_parts {
            if let Some(target_inv) = omit_invocation
                && matches!(
                    &part.semantic,
                    MessageSemantic::ToolCall { call } if call.invocation_id == target_inv
                )
            {
                continue;
            }
            match &part.semantic {
                MessageSemantic::Reasoning { .. } => continue,
                MessageSemantic::Text { text } if text.is_empty() => continue,
                other => push_cleaned_assistant_semantic(&mut cleaned, other.clone()),
            }
        }
        Some((old_origin, old_parts))
    } else {
        for part in content.assistant.parts.drain(..) {
            if let Some(target_inv) = omit_invocation
                && matches!(
                    &part.semantic,
                    MessageSemantic::ToolCall { call } if call.invocation_id == target_inv
                )
            {
                continue;
            }
            push_cleaned_assistant_semantic(&mut cleaned, part.semantic);
        }
        None
    };

    content.assistant.parts = cleaned;
    let status = if content.assistant.parts.is_empty() && content.results.is_empty() {
        NodeStatus::SkippedEmpty
    } else {
        NodeStatus::Modified
    };
    (status, saved)
}
