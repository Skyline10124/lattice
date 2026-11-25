// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use super::{codec::*, local::Selection};
use lattice_conversation::{
    BoundToolCall, CostEstimate, MessagePart, ModelAttemptKey, ModelId, ProviderId, TokenUsage,
    ToolResultOutcome, UsageCompleteness,
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CountedNodeKind {
    Root,
    Input,
    ContextOp,
    Step,
}

// 扁平投影让无关正文（input/op/reply/slots/config_digest）直接跳过，严禁内部标签枚举缓冲整条节点。
#[derive(serde::Deserialize)]
struct CountedNode {
    kind: CountedNodeKind,
    turn_id: Option<TurnId>,
    step_ordinal: Option<u32>,
}

#[derive(serde::Deserialize)]
struct StepAttempts {
    kind: CountedNodeKind,
    turn_id: Option<TurnId>,
    step_ordinal: Option<u32>,
    attempts: Option<Vec<AttemptState>>,
}

#[derive(serde::Deserialize)]
struct AttemptState {
    progress: AttemptStateTag,
}

// 只读标签，跳过 Ended 的诊断；内部标签枚举会缓存整个 progress 对象。
#[derive(serde::Deserialize)]
struct AttemptStateTag {
    progress: AttemptStateKind,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum AttemptStateKind {
    Sent,
    Ended,
}

#[derive(serde::Deserialize)]
struct TurnStartAgent {
    agent_name: AgentName,
}

#[derive(serde::Deserialize)]
struct TurnAgent {
    kind: CountedNodeKind,
    turn_id: Option<TurnId>,
    step_ordinal: Option<u32>,
    turn_start: Option<TurnStartAgent>,
}

#[derive(serde::Deserialize)]
struct UsageTurnStart {
    provider: ProviderId,
    model: ModelId,
}

// 扁平投影跳过正文、上下文操作、槽位与配置摘要，只保留父链、Step 0 模型身份和尝试计量。
#[derive(serde::Deserialize)]
struct UsageNode {
    kind: CountedNodeKind,
    parent: Option<NodeId>,
    turn_id: Option<TurnId>,
    step_ordinal: Option<u32>,
    turn_start: Option<UsageTurnStart>,
    attempts: Option<Vec<UsageAttempt>>,
}

#[derive(serde::Deserialize)]
struct UsageAttempt {
    usage: Option<TokenUsage>,
    usage_kind: UsageCompleteness,
    cost: Option<CostEstimate>,
}

impl UsageAttempt {
    fn metering(&self) -> (Option<TokenUsage>, UsageCompleteness, Option<CostEstimate>) {
        (self.usage, self.usage_kind, self.cost)
    }
}

#[derive(serde::Deserialize)]
struct LogAttempt {
    progress: AttemptProgress,
}

// 扁平投影供 `log()` 使用：跳过 input、op、reply、slots 与 turn_start.config_digest。
#[derive(serde::Deserialize)]
struct LogNode {
    kind: CountedNodeKind,
    parent: Option<NodeId>,
    turn_id: Option<TurnId>,
    step_ordinal: Option<u32>,
    turn_start: Option<TurnStartAgent>,
    progress: Option<StepProgress>,
    attempts: Option<Vec<LogAttempt>>,
}

#[derive(serde::Deserialize)]
struct StepSlots {
    slots: Vec<InvocationSlot>,
}

fn payload_summary(
    payload: &ResultPayload,
) -> (ToolResultOutcome, Option<SlotReason>, MaterialSummary) {
    let (reason, material) = match payload {
        ResultPayload::Succeeded(SuccessContent::Json {
            digest,
            byte_length,
            ..
        }) => (
            None,
            MaterialSummary::Available {
                digest: *digest,
                byte_length: *byte_length,
            },
        ),
        ResultPayload::Succeeded(SuccessContent::OutputTooLarge {
            digest,
            byte_length,
        }) => (
            None,
            MaterialSummary::OutputTooLarge {
                digest: *digest,
                byte_length: *byte_length,
            },
        ),
        ResultPayload::Failed(e) => (
            Some(SlotReason::Failed {
                code: e.code.clone(),
            }),
            MaterialSummary::NoContent,
        ),
        ResultPayload::NotExecuted => (None, MaterialSummary::NoContent),
        ResultPayload::Indeterminate(cause) => (
            Some(SlotReason::Indeterminate(*cause)),
            MaterialSummary::NoContent,
        ),
    };
    (payload.outcome(), reason, material)
}

fn slot_state(slot: &InvocationSlot) -> SlotState {
    match &slot.progress {
        SlotProgress::Registered => SlotState::Registered,
        SlotProgress::Started => SlotState::Started,
        SlotProgress::Terminal { result } => {
            let (outcome, reason, material) = payload_summary(&result.payload);
            SlotState::Terminal {
                outcome,
                reason,
                material,
            }
        }
    }
}

fn slot_detail(slot: &InvocationSlot) -> SlotDetail {
    let result = close::resolve_slot(slot);
    let (outcome, reason, material) = payload_summary(&result.payload);
    SlotDetail {
        invocation_id: slot.binding.invocation_id,
        binding: slot.binding.identity.clone(),
        progress: slot_state(slot),
        outcome,
        reason,
        material,
    }
}

fn attempt(
    index: usize,
    a: &AttemptRecord,
    address: lattice_conversation::ModelMessageAddress,
) -> AttemptDetail {
    let progress = match &a.progress {
        AttemptProgress::Sent => AttemptProgressDetail::Sent,
        AttemptProgress::Ended { end, diagnostic } => AttemptProgressDetail::Ended {
            end: end.clone(),
            diagnostic: diagnostic.provider_error.clone(),
        },
    };
    let (end, diagnostic) = close::resolve_attempt(a);
    let end = match end {
        AttemptEnd::Completed => AttemptEndDetail::Completed { step: address },
        AttemptEnd::Stopped { cause } => AttemptEndDetail::Stopped {
            cause,
            diagnostic: diagnostic.provider_error,
        },
    };
    AttemptDetail {
        attempt: index as u32,
        origin: a.origin.clone(),
        usage: a.usage,
        usage_kind: a.usage_kind,
        cost: a.cost,
        progress,
        end,
        retry_delay_ms: a.retry_delay_ms,
    }
}

fn detail(session_id: SessionId, node: NodeId, step: &Node) -> Result<StepDetail> {
    let Node::Step {
        turn_id,
        step_ordinal,
        progress,
        attempts,
        slots,
        updated_at,
        ..
    } = step
    else {
        return Err(corrupt("nodes", &node.0.to_string(), "expected step node"));
    };
    Ok(StepDetail {
        node,
        key: StepKey {
            turn_id: *turn_id,
            ordinal: *step_ordinal,
        },
        progress: progress.clone(),
        end: close::resolve_step(progress, slots)?,
        attempts: attempts
            .iter()
            .enumerate()
            .map(|(index, a)| {
                attempt(
                    index,
                    a,
                    lattice_conversation::ModelMessageAddress {
                        session_id,
                        turn_id: *turn_id,
                        model_step_ordinal: *step_ordinal,
                    },
                )
            })
            .collect(),
        slots: slots.iter().map(slot_detail).collect(),
        updated_at: *updated_at,
    })
}

fn positive(limit: usize) -> Result<()> {
    if limit == 0 {
        Err(invalid("limit", "page budget must be positive"))
    } else {
        Ok(())
    }
}

fn positive_turns(turns: u32) -> Result<()> {
    if turns == 0 {
        Err(invalid("turns", "turn count must be positive"))
    } else {
        Ok(())
    }
}

fn step_entry(session: SessionId, node: NodeId, content: Node) -> Result<ConversationEntry> {
    let Node::Step {
        step_ordinal,
        turn_start,
        progress,
        attempts,
        slots,
        ..
    } = &content
    else {
        return Err(corrupt("nodes", &node.0.to_string(), "expected step node"));
    };
    let agent_name = if *step_ordinal == 0 {
        let meta = turn_start.as_ref().ok_or_else(|| {
            corrupt(
                "nodes",
                &format!("{session}/{}", node.0),
                "missing turn start metadata",
            )
        })?;
        Some(meta.agent_name.clone())
    } else {
        None
    };
    let summary = if matches!(
        progress,
        StepProgress::Closed {
            end: StepEnd::Continue
        }
    ) {
        None
    } else {
        Some(close::summary(progress, attempts, slots)?)
    };
    let step = detail(session, node, &content)?;
    let reply = content.step_content();
    let retries = content.retry_notices()?;
    Ok(ConversationEntry::Step {
        node,
        step,
        agent_name,
        reply,
        retries,
        summary,
    })
}

fn is_unit_start(node: &Node, parent: &Node) -> bool {
    !matches!(
        node,
        Node::Root
            | Node::Step {
                step_ordinal: 1..,
                ..
            }
    ) && matches!(parent, Node::Root | Node::Step { .. })
}

fn entries(session: SessionId, path: Vec<(NodeId, Node)>) -> Result<Vec<ConversationEntry>> {
    let mut entries = Vec::new();
    for (node, content) in path {
        match content {
            Node::Input {
                accepted_at, input, ..
            } => {
                entries.push(ConversationEntry::Input {
                    node,
                    accepted_at,
                    input,
                });
            }
            Node::ContextOp {
                accepted_at, op, ..
            } => {
                entries.push(ConversationEntry::ContextOp {
                    node,
                    accepted_at,
                    op,
                });
            }
            Node::Step { .. } => {
                entries.push(step_entry(session, node, content)?);
            }
            Node::Root => {}
        }
    }
    Ok(entries)
}

pub(super) fn resolve_path_with_loader(
    session: SessionId,
    position: NodeId,
    next_pending_id: Option<NodeId>,
    pending: &[ContextInput],
    mut load_node: impl FnMut(NodeId) -> Result<Option<Node>>,
) -> Result<EffectiveContext> {
    let mut cursor = position;
    let mut chain = Vec::new();
    while cursor != NodeId(0) {
        let node = load_node(cursor)?.ok_or_else(|| {
            corrupt(
                "nodes",
                &format!("{session}/{}", cursor.0),
                "missing path node",
            )
        })?;
        let Some(parent) = node.parent() else {
            return Err(corrupt(
                "nodes",
                &format!("{session}/{}", cursor.0),
                "nonzero root",
            ));
        };
        if parent >= cursor {
            return Err(corrupt(
                "nodes",
                &format!("{session}/{}", cursor.0),
                "invalid path parent",
            ));
        }
        chain.push((cursor, node));
        cursor = parent;
    }
    if load_node(NodeId(0))? != Some(Node::Root) {
        return Err(corrupt(
            "nodes",
            &format!("{session}/0"),
            "missing root node",
        ));
    }
    chain.reverse();

    let mut records = Vec::with_capacity(chain.len() + pending.len());
    for (id, node) in &chain {
        match node {
            Node::Root => {
                return Err(corrupt(
                    "nodes",
                    &format!("{session}/{}", id.0),
                    "nonzero root",
                ));
            }
            Node::Input { input, .. } => {
                records.push(PathRecord::Input { id: *id, input });
            }
            Node::ContextOp { op, .. } => {
                records.push(PathRecord::Op { id: *id, op });
            }
            Node::Step { .. } => {
                if let Some(content) = node.step_content() {
                    records.push(PathRecord::Step { id: *id, content });
                }
            }
        }
    }
    if !pending.is_empty() {
        let mut next = next_pending_id.expect("存在 pending 输入时必须提供起始节点序号");
        for input in pending {
            records.push(PathRecord::Input { id: next, input });
            next = next_sequence(next.0, "node").map(NodeId)?;
        }
    }
    lattice_context::resolve(&records).map_err(|err| {
        corrupt(
            "nodes",
            &format!("{session}/{}", position.0),
            &format!("路径上下文归约失败：{err}"),
        )
    })
}

impl Records {
    pub(super) fn current_agent(&self, s: SessionId) -> Result<Option<AgentName>> {
        if !self.sessions.contains_key(&s) {
            return Err(missing("session", s));
        }
        self.session_settings
            .get(&s)
            .map(|settings| Some(settings.profile.agent_name.clone()))
            .ok_or_else(|| {
                corrupt(
                    "session_settings",
                    &s.to_string(),
                    "session settings missing",
                )
            })
    }

    pub(super) fn turn_detail(&self, s: SessionId, t: TurnId) -> Result<TurnDetail> {
        let start_ref = self
            .turn_index
            .get(&t)
            .filter(|r| r.session_id == s)
            .ok_or_else(|| missing("turn", t))?;
        let Some(Node::Step {
            turn_id,
            step_ordinal: 0,
            turn_start: Some(meta),
            ..
        }) = self.nodes.get(&(s, start_ref.node))
        else {
            return Err(corrupt(
                "nodes",
                &format!("{s}/{}", start_ref.node.0),
                "missing turn start step node",
            ));
        };
        let raw_steps = self.turn_steps(s, t)?;
        let (_, last_node) = raw_steps
            .last()
            .ok_or_else(|| corrupt("nodes", &t.to_string(), "turn has no steps"))?;
        let Node::Step {
            progress,
            attempts: last_attempts,
            slots: last_slots,
            updated_at: ended_at,
            ..
        } = last_node
        else {
            return Err(corrupt("nodes", &t.to_string(), "turn tip is not a step"));
        };
        let summary = close::summary(progress, last_attempts, last_slots)?;
        let usage = close::usage(raw_steps.iter().flat_map(|(_, node)| match node {
            Node::Step { attempts, .. } => attempts.as_slice(),
            _ => &[],
        }));
        let steps = raw_steps
            .iter()
            .map(|(node_id, node)| detail(s, *node_id, node))
            .collect::<Result<Vec<_>>>()?;
        Ok(TurnDetail {
            turn_id: *turn_id,
            session_id: s,
            start_node: start_ref.node,
            input_nodes: meta.input_nodes.clone(),
            agent_name: meta.agent_name.clone(),
            accepted_at: meta.accepted_at,
            ended_at: *ended_at,
            summary,
            usage: Box::new(usage),
            steps,
        })
    }
}

fn validate_usage_node(session_id: SessionId, id: NodeId, node: &UsageNode) -> Result<NodeId> {
    let err = || {
        corrupt(
            "nodes",
            &format!("{session_id}/{}", id.0),
            "invalid usage node header",
        )
    };
    let parent = node.parent.ok_or_else(err)?;
    if parent >= id {
        return Err(err());
    }
    match node.kind {
        CountedNodeKind::Root => return Err(err()),
        CountedNodeKind::Input | CountedNodeKind::ContextOp => {
            if node.turn_id.is_some()
                || node.step_ordinal.is_some()
                || node.turn_start.is_some()
                || node.attempts.is_some()
            {
                return Err(err());
            }
        }
        CountedNodeKind::Step => {
            let ordinal = node.step_ordinal.ok_or_else(err)?;
            if node.turn_id.is_none()
                || node.attempts.is_none()
                || (ordinal == 0) != node.turn_start.is_some()
            {
                return Err(err());
            }
        }
    }
    Ok(parent)
}

impl SessionRead {
    /// 同一读事务固定当前位置，汇总当前路径与会话全部节点中的尝试。
    pub fn usage(&self, session_id: SessionId) -> Result<SessionUsage> {
        let session = self
            .find_session(session_id)?
            .ok_or_else(|| missing("session", session_id))?;
        let mut head = session.current;
        let mut path = Vec::new();
        while head != NodeId(0) {
            let node: UsageNode = self.projected_node(session_id, head)?;
            let parent = validate_usage_node(session_id, head, &node)?;
            path.push((head, node));
            head = parent;
        }
        path.reverse();
        let path_total = UsageSummary::from_attempts(
            path.iter()
                .filter(|(_, node)| matches!(node.kind, CountedNodeKind::Step))
                .flat_map(|(_, node)| node.attempts.iter().flatten().map(UsageAttempt::metering)),
        );
        let table = self.tx.open_table(NODES).map_err(table_error)?;
        let mut session_attempts = Vec::new();
        for entry in table.range(
            node_key(session_id, NodeId(1)).as_slice()
                ..=node_key(session_id, NodeId(u64::MAX)).as_slice(),
        )? {
            let (key, value) = entry?;
            let (_, id) = decode_node_key(key.value())?;
            let key = format!("{session_id}/{}", id.0);
            let bytes = attach_path(&self.path, payload("nodes", &key, value.value()))?;
            let node: UsageNode = serde_json::from_slice(bytes)
                .map_err(|_| corrupt("nodes", &key, "invalid record encoding"))?;
            validate_usage_node(session_id, id, &node)?;
            if matches!(node.kind, CountedNodeKind::Step) {
                session_attempts.extend(
                    node.attempts
                        .ok_or_else(|| corrupt("nodes", &key, "missing step attempts"))?,
                );
            }
        }
        let session_total =
            UsageSummary::from_attempts(session_attempts.iter().map(UsageAttempt::metering));
        let mut nodes = path.into_iter().peekable();
        let mut turns = Vec::new();
        while let Some((node_id, node)) = nodes.next() {
            match node.kind {
                CountedNodeKind::Input | CountedNodeKind::ContextOp => continue,
                CountedNodeKind::Root => {
                    return Err(corrupt(
                        "nodes",
                        &format!("{session_id}/{}", node_id.0),
                        "nonzero root",
                    ));
                }
                CountedNodeKind::Step => {
                    let invalid_turn = || {
                        corrupt(
                            "nodes",
                            &format!("{session_id}/{}", node_id.0),
                            "invalid usage path turn",
                        )
                    };
                    if node.step_ordinal != Some(0) {
                        return Err(invalid_turn());
                    }
                    let turn_id = node.turn_id.ok_or_else(invalid_turn)?;
                    let start = node.turn_start.ok_or_else(invalid_turn)?;
                    let mut attempts = node.attempts.ok_or_else(invalid_turn)?;
                    let mut expected_ordinal = 1u32;
                    while nodes.peek().is_some_and(|(_, next)| {
                        matches!(next.kind, CountedNodeKind::Step)
                            && next.turn_id == Some(turn_id)
                            && next.step_ordinal != Some(0)
                    }) {
                        let (_, step) = nodes.next().expect("peeked step");
                        if step.step_ordinal != Some(expected_ordinal) {
                            return Err(invalid_turn());
                        }
                        expected_ordinal =
                            expected_ordinal.checked_add(1).ok_or_else(invalid_turn)?;
                        attempts.extend(step.attempts.ok_or_else(invalid_turn)?);
                    }
                    turns.push(TurnUsageRow {
                        turn_id,
                        start_node: node_id,
                        provider: start.provider,
                        model: start.model,
                        usage: UsageSummary::from_attempts(
                            attempts.iter().map(UsageAttempt::metering),
                        ),
                    });
                }
            }
        }
        Ok(SessionUsage {
            session_id,
            position: session.current,
            turns,
            path_total,
            session_total,
        })
    }

    fn local(&self, session: SessionId, selection: Selection) -> Result<Records> {
        attach_path(&self.path, local::read(&self.tx, session, selection))
    }

    fn decode<T: serde::de::DeserializeOwned + serde::Serialize>(
        &self,
        table: &str,
        key: &str,
        bytes: &[u8],
    ) -> Result<T> {
        attach_path(&self.path, decode(table, key, bytes))
    }

    fn raw_turn_node(&self, id: TurnId) -> Result<Option<NodeRef>> {
        self.tx
            .open_table(TURN_INDEX)
            .map_err(table_error)?
            .get(id.to_string().as_str())?
            .map(|v| self.decode("turn_index", &id.to_string(), v.value()))
            .transpose()
    }

    /// 调用方给出的 Session 可能不存在，由调用方报 NotFound。
    pub fn find_session(&self, id: SessionId) -> Result<Option<SessionRecord>> {
        let table = self.tx.open_table(SESSIONS).map_err(table_error)?;
        let Some(value) = table.get(id.to_string().as_str())? else {
            return Ok(None);
        };
        let s: SessionRecord = self.decode("sessions", &id.to_string(), value.value())?;
        if s.session_id != id {
            return Err(corrupt("sessions", &id.to_string(), "key/value mismatch"));
        }
        Ok(Some(s))
    }

    pub fn session(&self, id: SessionId) -> Result<SessionRecord> {
        self.local(id, Selection::Session)?
            .sessions
            .remove(&id)
            .ok_or_else(|| missing("session", id))
    }

    /// 按 TurnId 索引找回该轮 `Step 0` 节点位置，并校验所属 Turn。
    pub fn turn_node(&self, id: TurnId) -> Result<Option<NodeRef>> {
        let Some(node_ref) = self.raw_turn_node(id)? else {
            return Ok(None);
        };
        Ok(self
            .local(node_ref.session_id, Selection::Turn(id))?
            .turn_index
            .remove(&id))
    }

    /// 内部组合层读取冻结配置使用；不要直接序列化到公开 IPC。
    pub fn turn_config(&self, session: SessionId, turn: TurnId) -> Result<FrozenTurnConfig> {
        self.local(session, Selection::Turn(turn))?
            .turn_config(session, turn)
    }

    /// 读取指定轮次随 `Step 0.turn_start.input_nodes` 绑定的全部上下文输入。
    pub fn turn_inputs(&self, session: SessionId, turn: TurnId) -> Result<Vec<ContextInput>> {
        self.local(session, Selection::Turn(turn))?
            .turn_inputs(session, turn)
    }

    pub fn turn_input(
        &self,
        session: SessionId,
        turn: TurnId,
    ) -> Result<Vec<MessagePart<BoundToolCall>>> {
        self.local(session, Selection::Turn(turn))?
            .turn_input(session, turn)
    }

    /// 借出一张图片的原始字节并调用一次 `consume`；字节只在回调内有效。
    pub fn with_blob<T>(&self, digest: &[u8; 32], consume: impl FnOnce(&[u8]) -> T) -> Result<T> {
        let key = blob_key(digest);
        let table = self.tx.open_table(BLOBS).map_err(table_error)?;
        let guard = table
            .get(key.as_str())?
            .ok_or_else(|| corrupt("blobs", &key, "引用的图片字节不存在"))?;
        let bytes = attach_path(&self.path, payload("blobs", &key, guard.value()))?;
        Ok(consume(bytes))
    }

    /// 仅供测试统计 `blobs` 表中的图片条目总数，核验内容寻址去重与准入前拒绝未写库。
    #[cfg(any(test, feature = "test-utils"))]
    pub fn blob_count(&self) -> Result<usize> {
        use redb::ReadableTableMetadata;
        let table = self.tx.open_table(BLOBS).map_err(table_error)?;
        Ok(table.len()? as usize)
    }

    pub fn stored_config(&self, digest: &[u8; 32]) -> Result<StoredTurnConfig> {
        let key = config_key(digest);
        let stored: StoredTurnConfig = self.decode(
            "configs",
            &key,
            self.tx
                .open_table(CONFIGS)
                .map_err(table_error)?
                .get(key.as_str())?
                .ok_or_else(|| missing("config", key.clone()))?
                .value(),
        )?;
        validate::stored_config(digest, &stored)
            .map_err(|_| corrupt("configs", &key, "invalid stored config"))?;
        Ok(stored)
    }

    /// 读取会话的当前设置；完整设置只供后台组成，不直接发到 IPC。
    pub fn session_settings(&self, session: SessionId) -> Result<SessionSettings> {
        self.find_session(session)?
            .ok_or_else(|| missing("session", session))?;
        let key = session.to_string();
        let table = self.tx.open_table(SESSION_SETTINGS).map_err(table_error)?;
        let value = table
            .get(key.as_str())?
            .ok_or_else(|| corrupt("session_settings", &key, "session settings missing"))?;
        let settings: SessionSettings =
            attach_path(&self.path, decode_session_settings(&key, value.value()))?;
        settings
            .validate()
            .map_err(|_| corrupt("session_settings", &key, "invalid session settings"))?;
        Ok(settings)
    }

    pub fn current_agent(&self, session: SessionId) -> Result<Option<AgentName>> {
        self.local(session, Selection::Session)?
            .current_agent(session)
    }

    pub fn node(&self, node: NodeRef) -> Result<Node> {
        self.local(node.session_id, Selection::Node(node.node))?
            .nodes
            .remove(&(node.session_id, node.node))
            .ok_or_else(|| missing("node", node.node.0))
    }

    /// 按身份键前缀做一次有序范围读取，最多返回 `limit` 个；只读键，不读记录值，也不扫描无关会话。
    pub fn session_ids_with_prefix(&self, prefix: &str, limit: usize) -> Result<Vec<SessionId>> {
        positive(limit)?;
        let table = self.tx.open_table(SESSIONS).map_err(table_error)?;
        let mut items = Vec::new();
        for entry in table.range::<&str>(prefix..)? {
            let (key, _) = entry?;
            if !key.value().starts_with(prefix) || items.len() == limit {
                break;
            }
            items.push(
                SessionId::parse(key.value())
                    .map_err(|_| corrupt("sessions", "key", "invalid id"))?,
            );
        }
        Ok(items)
    }

    /// 只读扫描所有会话，返回只有根节点且 `updated_at < cutoff` 的草稿候选。
    pub fn expired_drafts(&self, cutoff: Timestamp) -> Result<Vec<SessionId>> {
        let sessions = self.tx.open_table(SESSIONS).map_err(table_error)?;
        let nodes = self.tx.open_table(NODES).map_err(table_error)?;
        let mut drafts = Vec::new();
        for entry in sessions.range::<&str>(..)? {
            let (key, value) = entry?;
            let id = SessionId::parse(key.value())
                .map_err(|_| corrupt("sessions", "key", "invalid id"))?;
            let session: SessionRecord = self.decode("sessions", key.value(), value.value())?;
            if session.session_id != id {
                return Err(corrupt("sessions", key.value(), "key/value mismatch"));
            }
            if session.updated_at >= cutoff {
                continue;
            }
            let tip = nodes
                .range(
                    node_key(id, NodeId(0)).as_slice()..=node_key(id, NodeId(u64::MAX)).as_slice(),
                )?
                .next_back()
                .transpose()?
                .map(|(k, _)| attach_path(&self.path, decode_node_key(k.value())))
                .transpose()?
                .map(|(_, node)| node)
                .ok_or_else(|| corrupt("nodes", &format!("{id}/0"), "missing root node"))?;
            if tip == NodeId(0) {
                drafts.push(id);
            }
        }
        Ok(drafts)
    }

    pub fn list(
        &self,
        after: Option<SessionId>,
        limit: usize,
    ) -> Result<Page<SessionListRow, SessionId>> {
        positive(limit)?;
        let table = self.tx.open_table(SESSIONS).map_err(table_error)?;
        let after = after.map(|id| id.to_string());
        let mut sessions = table.range::<&str>((
            after
                .as_deref()
                .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded),
            std::ops::Bound::Unbounded,
        ))?;
        let nodes = self.tx.open_table(NODES).map_err(table_error)?;
        let mut items = Vec::new();
        for entry in sessions.by_ref().take(limit) {
            let (key, _) = entry?;
            let id = SessionId::parse(key.value())
                .map_err(|_| corrupt("sessions", "key", "invalid id"))?;
            let r = self.local(id, Selection::Session)?;
            let unclosed_turn = r.unclosed_turn(id);
            let s = r.sessions.get(&id).ok_or_else(|| missing("session", id))?;
            let node_count = nodes
                .range(
                    node_key(id, NodeId(0)).as_slice()..=node_key(id, NodeId(u64::MAX)).as_slice(),
                )?
                .try_fold(0u64, |n, entry| -> Result<u64> {
                    entry?;
                    next_sequence(n, "node count")
                })?;
            items.push(SessionListRow {
                session_id: id,
                project_path: s.project_path.clone(),
                current: s.current,
                node_count,
                unclosed_turn,
                updated_at: s.updated_at,
            });
        }
        let next = if sessions.next().transpose()?.is_some() {
            items.last().map(|s| s.session_id)
        } else {
            None
        };
        Ok(Page { items, next })
    }

    pub fn log(&self, id: SessionId) -> Result<SessionLog> {
        let session = self
            .find_session(id)?
            .ok_or_else(|| missing("session", id))?;
        let current_agent = Some(self.session_settings(id)?.profile.agent_name);
        let table = self.tx.open_table(NODES).map_err(table_error)?;
        let mut scanned: Vec<(NodeId, LogNode)> = Vec::new();
        for entry in table
            .range(node_key(id, NodeId(0)).as_slice()..=node_key(id, NodeId(u64::MAX)).as_slice())?
        {
            let (key, value) = entry?;
            let (_, n) = decode_node_key(key.value())?;
            let key_str = format!("{id}/{}", n.0);
            let bytes = attach_path(&self.path, payload("nodes", &key_str, value.value()))?;
            let node: LogNode = serde_json::from_slice(bytes)
                .map_err(|_| corrupt("nodes", &key_str, "invalid record encoding"))?;
            let err = || corrupt("nodes", &key_str, "invalid log node header");
            match node.kind {
                CountedNodeKind::Root => {
                    if n != NodeId(0)
                        || node.parent.is_some()
                        || node.turn_id.is_some()
                        || node.step_ordinal.is_some()
                        || node.turn_start.is_some()
                        || node.progress.is_some()
                        || node.attempts.is_some()
                    {
                        return Err(err());
                    }
                }
                CountedNodeKind::Input | CountedNodeKind::ContextOp => {
                    if n == NodeId(0)
                        || node.parent.is_none_or(|p| p >= n)
                        || node.turn_id.is_some()
                        || node.step_ordinal.is_some()
                        || node.turn_start.is_some()
                        || node.progress.is_some()
                        || node.attempts.is_some()
                    {
                        return Err(err());
                    }
                }
                CountedNodeKind::Step => {
                    let ordinal = node.step_ordinal.ok_or_else(err)?;
                    if n == NodeId(0)
                        || node.parent.is_none_or(|p| p >= n)
                        || node.turn_id.is_none()
                        || (ordinal == 0) != node.turn_start.is_some()
                        || node.progress.is_none()
                        || node.attempts.is_none()
                    {
                        return Err(err());
                    }
                }
            }
            scanned.push((n, node));
        }
        if scanned
            .first()
            .is_none_or(|(n, node)| *n != NodeId(0) || !matches!(node.kind, CountedNodeKind::Root))
            || !scanned.iter().any(|(n, _)| *n == session.current)
        {
            return Err(corrupt(
                "sessions",
                &id.to_string(),
                "session root or current node missing",
            ));
        }
        let unclosed_turn = scanned.last().and_then(|(_, tip)| match tip.kind {
            CountedNodeKind::Step if tip.progress.as_ref().is_some_and(|p| !p.is_closed()) => {
                tip.turn_id
            }
            _ => None,
        });

        let mut nodes = Vec::with_capacity(scanned.len());
        for (idx, (n, node)) in scanned.iter().enumerate() {
            let kind = match node.kind {
                CountedNodeKind::Root => NodeKind::Root,
                CountedNodeKind::Input => NodeKind::Input,
                CountedNodeKind::ContextOp => NodeKind::ContextOp,
                CountedNodeKind::Step => NodeKind::Step {
                    ordinal: node.step_ordinal.expect("已校验 Step 的 step_ordinal"),
                },
            };
            let turn_start = if matches!(node.kind, CountedNodeKind::Step)
                && node.step_ordinal == Some(0)
            {
                let turn_id = node.turn_id.expect("已校验 Step 0 的 turn_id");
                let agent_name = node
                    .turn_start
                    .as_ref()
                    .expect("已校验 Step 0 的 turn_start")
                    .agent_name
                    .clone();
                let mut last_idx = idx;
                let mut expected_ord = 1u32;
                while let Some((_, next)) = scanned.get(last_idx + 1) {
                    if matches!(next.kind, CountedNodeKind::Step) && next.turn_id == Some(turn_id) {
                        if next.step_ordinal != Some(expected_ord) {
                            return Err(corrupt(
                                "nodes",
                                &format!("{id}/{}", n.0),
                                "invalid step ordinal sequence in turn",
                            ));
                        }
                        last_idx += 1;
                        expected_ord = expected_ord.checked_add(1).ok_or_else(|| {
                            corrupt("nodes", &format!("{id}/{}", n.0), "step ordinal overflow")
                        })?;
                    } else {
                        break;
                    }
                }
                let (last_node_id, last_step) = &scanned[last_idx];
                let progress = last_step
                    .progress
                    .as_ref()
                    .expect("已校验 Step 的 progress");
                let attempts = last_step
                    .attempts
                    .as_ref()
                    .expect("已校验 Step 的 attempts");
                let slots = if matches!(progress, StepProgress::Executing) {
                    let projected: StepSlots = self.projected_node(id, *last_node_id)?;
                    projected.slots
                } else {
                    Vec::new()
                };
                let summary = close::summary_from_progress(
                    progress,
                    attempts.last().map(|a| &a.progress),
                    &slots,
                )?;
                Some(TurnLogSummary {
                    turn_id,
                    summary,
                    agent_name,
                })
            } else {
                None
            };
            nodes.push(LogRow {
                node: NodeHeader {
                    id: *n,
                    parent: node.parent,
                    turn_id: node.turn_id,
                    kind,
                },
                current: *n == session.current,
                turn_start,
            });
        }
        Ok(SessionLog {
            session,
            current_agent,
            unclosed_turn,
            nodes,
        })
    }

    pub fn turn_detail(&self, session: SessionId, turn: TurnId) -> Result<TurnDetail> {
        self.local(session, Selection::Turn(turn))?
            .turn_detail(session, turn)
    }

    /// 路径二：沿 `Root -> position` 归约有效模型上下文（全程不读取 `blobs` 表）。
    pub fn effective_context(
        &self,
        session: SessionId,
        position: NodeId,
    ) -> Result<EffectiveContext> {
        self.effective_context_with_pending(session, position, &[])
    }

    /// 路径二：沿 `Root -> position` 并在末尾追加待提交 `pending` 输入后归约有效模型上下文（全程不读取 `blobs` 表）。
    pub fn effective_context_with_pending(
        &self,
        session: SessionId,
        position: NodeId,
        pending: &[ContextInput],
    ) -> Result<EffectiveContext> {
        self.find_session(session)?
            .ok_or_else(|| missing("session", session))?;
        let next_pending_id = if pending.is_empty() {
            None
        } else {
            let table = self.tx.open_table(NODES).map_err(table_error)?;
            let tip = table
                .range(
                    node_key(session, NodeId(0)).as_slice()
                        ..=node_key(session, NodeId(u64::MAX)).as_slice(),
                )?
                .next_back()
                .transpose()?
                .map(|(k, _)| attach_path(&self.path, decode_node_key(k.value())))
                .transpose()?
                .map(|(_, node_id)| node_id)
                .ok_or_else(|| corrupt("nodes", &format!("{session}/0"), "missing root node"))?;
            Some(next_sequence(tip.0, "node").map(NodeId)?)
        };
        if self.find_node(session, position)?.is_none() {
            return Err(missing("node", position.0));
        }
        resolve_path_with_loader(session, position, next_pending_id, pending, |node_id| {
            self.find_node(session, node_id)
        })
    }

    /// 路径二：检视指定位置（`None` 时取会话当前位置）的有效模型上下文客观统计（全程不读取 `blobs` 表）。
    pub fn inspect_context(
        &self,
        session: SessionId,
        position: Option<NodeId>,
    ) -> Result<(NodeId, ContextInspection)> {
        let record = self
            .find_session(session)?
            .ok_or_else(|| missing("session", session))?;
        let pos = position.unwrap_or(record.current);
        if self.find_node(session, pos)?.is_none() {
            return Err(missing("node", pos.0));
        }
        let effective = resolve_path_with_loader(session, pos, None, &[], |node_id| {
            self.find_node(session, node_id)
        })?;
        Ok((pos, effective.inspect()))
    }

    /// 路径一：同一读事务内取得会话头与当前路径最近的若干轮（含末尾未触发上下文块与未关闭 Step）。
    pub fn view(&self, session: SessionId, turns: u32) -> Result<SessionView> {
        positive_turns(turns)?;
        let record = self
            .find_session(session)?
            .ok_or_else(|| missing("session", session))?;
        let (path, has_more) = self.conversation_window(session, record.current, turns)?;
        let settings = self.session_settings(session)?;
        let current_agent = Some(settings.profile.agent_name.clone());
        Ok(SessionView {
            session: record,
            current_agent,
            settings,
            entries: entries(session, path)?,
            has_more,
        })
    }

    /// 路径一：`before` 必须是本会话的轮次单元起点；读取只取其固定父链上的更早轮次单元与是否存在更早记录。
    pub fn history(
        &self,
        session: SessionId,
        before: NodeId,
        turns: u32,
    ) -> Result<(Vec<ConversationEntry>, bool)> {
        positive_turns(turns)?;
        self.find_session(session)?
            .ok_or_else(|| missing("session", session))?;
        let Some(node) = self.find_node(session, before)? else {
            return Err(invalid("before", "分页起点节点不在当前会话中"));
        };
        let Some(parent_id) = node.parent() else {
            return Err(invalid("before", "根节点不能作为分页起点"));
        };
        let parent = self.path_node(session, parent_id)?;
        if !is_unit_start(&node, &parent) {
            return Err(invalid("before", "before 必须是轮次单元起点节点"));
        }
        let (path, has_more) = self.conversation_window(session, parent_id, turns)?;
        Ok((entries(session, path)?, has_more))
    }

    /// 读取节点的最新完整条目，供观察者在节点变化后整条替换。
    pub fn entry(&self, node: NodeRef) -> Result<ConversationEntry> {
        self.find_session(node.session_id)?
            .ok_or_else(|| missing("session", node.session_id))?;
        let content = self
            .find_node(node.session_id, node.node)?
            .ok_or_else(|| missing("node", node.node.0))?;
        match content {
            Node::Root => Err(invalid("node", "root has no conversation entry")),
            Node::Input {
                accepted_at, input, ..
            } => Ok(ConversationEntry::Input {
                node: node.node,
                accepted_at,
                input,
            }),
            Node::ContextOp {
                accepted_at, op, ..
            } => Ok(ConversationEntry::ContextOp {
                node: node.node,
                accepted_at,
                op,
            }),
            Node::Step {
                turn_id,
                step_ordinal,
                ..
            } => {
                let start = self.turn_start_node(node.session_id, turn_id)?;
                if step_ordinal == 0 {
                    if start != node.node {
                        return Err(corrupt(
                            "nodes",
                            &format!("{}/{}", node.session_id, node.node.0),
                            "step 0 does not match turn index",
                        ));
                    }
                } else {
                    if start.0.checked_add(u64::from(step_ordinal)) != Some(node.node.0) {
                        return Err(corrupt(
                            "nodes",
                            &format!("{}/{}", node.session_id, node.node.0),
                            "step does not match turn start offset",
                        ));
                    }
                    let start_header: CountedNode = self.projected_node(node.session_id, start)?;
                    if !matches!(start_header.kind, CountedNodeKind::Step)
                        || start_header.turn_id != Some(turn_id)
                        || start_header.step_ordinal != Some(0)
                    {
                        return Err(corrupt(
                            "nodes",
                            &format!("{}/{}", node.session_id, start.0),
                            "expected turn start step",
                        ));
                    }
                }
                step_entry(node.session_id, node.node, content)
            }
        }
    }

    fn find_node(&self, session: SessionId, node: NodeId) -> Result<Option<Node>> {
        self.tx
            .open_table(NODES)
            .map_err(table_error)?
            .get(node_key(session, node).as_slice())?
            .map(|value| self.decode("nodes", &format!("{}/{}", session, node.0), value.value()))
            .transpose()
    }

    /// 只解码指定路径节点；缺失表示已有路径损坏，不装载整轮配置。
    pub fn path_node(&self, session: SessionId, node: NodeId) -> Result<Node> {
        self.find_node(session, node)?.ok_or_else(|| {
            corrupt(
                "nodes",
                &format!("{}/{}", session, node.0),
                "missing path node",
            )
        })
    }

    /// 经 TurnId 索引定位本会话的 `Step 0`，索引缺失或归属不符时报损坏。
    pub fn turn_start_node(&self, session: SessionId, turn: TurnId) -> Result<NodeId> {
        self.raw_turn_node(turn)?
            .filter(|node| node.session_id == session)
            .map(|node| node.node)
            .ok_or_else(|| {
                corrupt(
                    "turn_index",
                    &turn.to_string(),
                    "missing turn start step node",
                )
            })
    }

    /// 只读指定 Step 的尝试标签，供生成中文字的推送守卫使用。
    pub fn attempt_is_sent(
        &self,
        session: SessionId,
        node: NodeId,
        key: ModelAttemptKey,
    ) -> Result<bool> {
        let step: StepAttempts = self.projected_node(session, node)?;
        let err = || {
            corrupt(
                "nodes",
                &format!("{session}/{}", node.0),
                "expected step node",
            )
        };
        if !matches!(step.kind, CountedNodeKind::Step) {
            return Err(err());
        }
        let turn_id = step.turn_id.ok_or_else(err)?;
        let step_ordinal = step.step_ordinal.ok_or_else(err)?;
        let attempts = step.attempts.ok_or_else(err)?;
        Ok(turn_id == key.turn_id
            && step_ordinal == key.model_step_ordinal
            && attempts
                .get(key.attempt_ordinal as usize)
                .is_some_and(|attempt| matches!(attempt.progress.progress, AttemptStateKind::Sent)))
    }

    /// 经 TurnId 索引只读取本轮 `Step 0` 上的 agent，不装载输入、正文与冻结配置。
    pub fn turn_agent(&self, session: SessionId, turn: TurnId) -> Result<AgentName> {
        let node = self.turn_start_node(session, turn)?;
        let step: TurnAgent = self.projected_node(session, node)?;
        if !matches!(step.kind, CountedNodeKind::Step)
            || step.turn_id != Some(turn)
            || step.step_ordinal != Some(0)
        {
            return Err(corrupt(
                "nodes",
                &format!("{session}/{}", node.0),
                "expected turn start step",
            ));
        }
        let start = step.turn_start.ok_or_else(|| {
            corrupt(
                "nodes",
                &format!("{session}/{}", node.0),
                "missing turn start metadata",
            )
        })?;
        Ok(start.agent_name)
    }

    fn projected_node<T: serde::de::DeserializeOwned>(
        &self,
        session: SessionId,
        node: NodeId,
    ) -> Result<T> {
        let key = format!("{}/{}", session, node.0);
        let table = self.tx.open_table(NODES).map_err(table_error)?;
        let value = table
            .get(node_key(session, node).as_slice())?
            .ok_or_else(|| corrupt("nodes", &key, "missing path node"))?;
        let bytes = attach_path(&self.path, payload("nodes", &key, value.value()))?;
        serde_json::from_slice(bytes).map_err(|_| corrupt("nodes", &key, "invalid record encoding"))
    }

    fn conversation_window(
        &self,
        session: SessionId,
        mut head: NodeId,
        mut turns: u32,
    ) -> Result<(Vec<(NodeId, Node)>, bool)> {
        let mut path = Vec::new();

        // 阶段 1：收集末尾尚未触发推理的连续 Input/ContextOp 尾块（不消耗 turns 预算）。
        let mut first_step: Option<(NodeId, Node)> = None;
        if turns > 0 {
            while head != NodeId(0) {
                let node = self.path_node(session, head)?;
                let Some(parent) = node.parent() else {
                    return Err(corrupt(
                        "nodes",
                        &format!("{session}/{}", head.0),
                        "nonzero root",
                    ));
                };
                if parent >= head {
                    return Err(corrupt(
                        "nodes",
                        &format!("{session}/{}", head.0),
                        "invalid path parent",
                    ));
                }
                if matches!(node, Node::Input { .. } | Node::ContextOp { .. }) {
                    path.push((head, node));
                    head = parent;
                } else {
                    first_step = Some((head, node));
                    break;
                }
            }
        }

        // 阶段 2：向回收集至多 turns 个「Step 0..=last_step + 紧邻 Step 0 之前的连续 Input/ContextOp 块」。
        while turns > 0 && head != NodeId(0) {
            // 沿本轮 Step 向回走到 Step 0。
            loop {
                let (node_id, node) = match first_step.take() {
                    Some(peeked) => peeked,
                    None => (head, self.path_node(session, head)?),
                };
                let Node::Step {
                    parent,
                    step_ordinal,
                    ..
                } = &node
                else {
                    return Err(corrupt(
                        "nodes",
                        &format!("{session}/{}", node_id.0),
                        "expected step node in turn unit",
                    ));
                };
                let parent = *parent;
                let is_step_zero = *step_ordinal == 0;
                if parent >= node_id {
                    return Err(corrupt(
                        "nodes",
                        &format!("{session}/{}", node_id.0),
                        "invalid path parent",
                    ));
                }
                path.push((node_id, node));
                head = parent;
                if is_step_zero {
                    turns -= 1;
                    break;
                }
                if head == NodeId(0) {
                    return Err(corrupt(
                        "nodes",
                        &format!("{session}/{}", node_id.0),
                        "step has no step 0 ancestor",
                    ));
                }
            }
            // 用扁平头探查 Step 0 紧邻前置的连续 Input/ContextOp，避免在 turns == 0 时误解更早 Step 的正文。
            while head != NodeId(0) {
                let header: CountedNode = self.projected_node(session, head)?;
                match header.kind {
                    CountedNodeKind::Input | CountedNodeKind::ContextOp => {
                        let node = self.path_node(session, head)?;
                        let Some(parent) = node.parent() else {
                            return Err(corrupt(
                                "nodes",
                                &format!("{session}/{}", head.0),
                                "nonzero root",
                            ));
                        };
                        if parent >= head {
                            return Err(corrupt(
                                "nodes",
                                &format!("{session}/{}", head.0),
                                "invalid path parent",
                            ));
                        }
                        path.push((head, node));
                        head = parent;
                    }
                    CountedNodeKind::Step => break,
                    CountedNodeKind::Root => {
                        return Err(corrupt(
                            "nodes",
                            &format!("{session}/{}", head.0),
                            "nonzero root",
                        ));
                    }
                }
            }
        }

        let has_more = head != NodeId(0);
        path.reverse();
        Ok((path, has_more))
    }
}
