// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use super::{codec::*, local::Selection};
use lattice_conversation::{
    CostEstimate, ModelAttemptKey, ModelId, ProviderId, TokenUsage, ToolResultOutcome,
    UsageCompleteness,
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CountedNodeKind {
    UserMessage,
    Step,
}

// 根节点不参与计数；扁平投影让无关字段直接跳过，避免内部标签枚举缓存整份正文。
#[derive(serde::Deserialize)]
struct CountedNode {
    kind: CountedNodeKind,
    parent: NodeId,
    turn_id: TurnId,
}

#[derive(serde::Deserialize)]
struct StepAttempts {
    kind: CountedNodeKind,
    turn_id: TurnId,
    step_ordinal: u32,
    attempts: Vec<AttemptState>,
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
struct TurnAgent {
    kind: CountedNodeKind,
    turn_id: TurnId,
    agent_name: AgentName,
}

// 扁平投影跳过正文与执行材料，只保留父链、轮的模型身份和尝试计量。
#[derive(serde::Deserialize)]
struct UsageNode {
    kind: CountedNodeKind,
    parent: NodeId,
    turn_id: TurnId,
    provider: Option<ProviderId>,
    model: Option<ModelId>,
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
        progress,
        attempts,
        slots,
        ..
    } = &content
    else {
        return Err(corrupt("nodes", &node.0.to_string(), "expected step node"));
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
    let retries = content.retry_notices()?;
    Ok(ConversationEntry::Step {
        node,
        content,
        step,
        retries,
        summary,
    })
}

fn entries(
    session: SessionId,
    path: Vec<(NodeId, Node)>,
    mut ordinal: u64,
) -> Result<Vec<ConversationEntry>> {
    let mut has_user = false;
    let mut entries = Vec::new();
    for (node, content) in path {
        match &content {
            Node::UserMessage { .. } => {
                ordinal += 1;
                has_user = true;
                entries.push(ConversationEntry::UserMessage {
                    node,
                    content,
                    ordinal,
                });
            }
            Node::Step { .. } => {
                if !has_user {
                    return Err(corrupt(
                        "nodes",
                        &node.0.to_string(),
                        "step has no user ancestor",
                    ));
                }
                entries.push(step_entry(session, node, content)?);
            }
            Node::Root => {}
        }
    }
    Ok(entries)
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
        let user_ref = self
            .turn_index
            .get(&t)
            .filter(|r| r.session_id == s)
            .ok_or_else(|| missing("turn", t))?;
        let Some(Node::UserMessage {
            turn_id,
            accepted_at,
            agent_name,
            ..
        }) = self.nodes.get(&(s, user_ref.node))
        else {
            return Err(corrupt(
                "nodes",
                &format!("{s}/{}", user_ref.node.0),
                "missing turn user node",
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
            user_node: user_ref.node,
            agent_name: agent_name.clone(),
            accepted_at: *accepted_at,
            ended_at: *ended_at,
            summary,
            usage: Box::new(usage),
            steps,
        })
    }
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
            if node.parent >= head {
                return Err(corrupt(
                    "nodes",
                    &format!("{session_id}/{}", head.0),
                    "invalid path parent",
                ));
            }
            head = node.parent;
            path.push(node);
        }
        path.reverse();
        let path_total = UsageSummary::from_attempts(
            path.iter()
                .filter(|node| matches!(node.kind, CountedNodeKind::Step))
                .flat_map(|node| node.attempts.iter().flatten().map(UsageAttempt::metering)),
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
        while let Some(user) = nodes.next() {
            let invalid_turn = || {
                corrupt(
                    "nodes",
                    &user.turn_id.to_string(),
                    "invalid usage path turn",
                )
            };
            if !matches!(user.kind, CountedNodeKind::UserMessage) {
                return Err(invalid_turn());
            }
            let provider = user.provider.ok_or_else(invalid_turn)?;
            let model = user.model.ok_or_else(invalid_turn)?;
            let mut attempts = Vec::new();
            while nodes
                .peek()
                .is_some_and(|node| matches!(node.kind, CountedNodeKind::Step))
            {
                let step = nodes.next().expect("peeked step");
                if step.turn_id != user.turn_id {
                    return Err(invalid_turn());
                }
                attempts.extend(step.attempts.ok_or_else(invalid_turn)?);
            }
            turns.push(TurnUsageRow {
                turn_id: user.turn_id,
                ordinal: turns.len() as u64 + 1,
                provider,
                model,
                usage: UsageSummary::from_attempts(attempts.iter().map(UsageAttempt::metering)),
            });
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

    /// 按 TurnId 索引找回该轮 UserMessage 节点位置，并校验所属 Turn。
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

    pub fn turn_input(&self, session: SessionId, turn: TurnId) -> Result<String> {
        self.local(session, Selection::Turn(turn))?
            .turn_input(session, turn)
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
        let mut r = self.local(id, Selection::Session)?;
        let current_agent = r.current_agent(id)?;
        let unclosed_turn = r.unclosed_turn(id);
        let session = r
            .sessions
            .remove(&id)
            .ok_or_else(|| missing("session", id))?;
        let table = self.tx.open_table(NODES).map_err(table_error)?;
        let mut nodes = Vec::new();
        for entry in table
            .range(node_key(id, NodeId(0)).as_slice()..=node_key(id, NodeId(u64::MAX)).as_slice())?
        {
            let (key, _) = entry?;
            let (_, n) = decode_node_key(key.value())?;
            let value = self.node(NodeRef {
                session_id: id,
                node: n,
            })?;
            let kind = match &value {
                Node::Root => NodeKind::Root,
                Node::UserMessage { .. } => NodeKind::UserMessage,
                Node::Step { step_ordinal, .. } => NodeKind::Step {
                    ordinal: *step_ordinal,
                },
            };
            let turn_start = if let Node::UserMessage { turn_id, .. } = &value {
                let detail = self.turn_detail(id, *turn_id)?;
                Some(TurnLogSummary {
                    turn_id: *turn_id,
                    summary: detail.summary,
                    agent_name: detail.agent_name,
                })
            } else {
                None
            };
            nodes.push(LogRow {
                node: NodeHeader {
                    id: n,
                    parent: value.parent(),
                    turn_id: value.turn_id(),
                    kind,
                },
                current: n == session.current,
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

    /// 同一读事务内取得会话头与当前路径最近的若干轮，包含尚未关闭的 Step。
    pub fn view(&self, session: SessionId, turns: u32) -> Result<SessionView> {
        positive_turns(turns)?;
        let record = self
            .find_session(session)?
            .ok_or_else(|| missing("session", session))?;
        let (path, earlier_turns) = self.conversation_window(session, record.current, turns)?;
        let current_agent = Some(self.session_settings(session)?.profile.agent_name);
        Ok(SessionView {
            session: record,
            current_agent,
            entries: entries(session, path, earlier_turns)?,
        })
    }

    /// before 必须是本会话的 UserMessage；读取只取它固定父链上的更早轮次。
    pub fn history(
        &self,
        session: SessionId,
        before: NodeId,
        turns: u32,
    ) -> Result<Vec<ConversationEntry>> {
        positive_turns(turns)?;
        self.find_session(session)?
            .ok_or_else(|| missing("session", session))?;
        let Some(Node::UserMessage { parent, .. }) = self.find_node(session, before)? else {
            return Err(invalid("before", "expected a user message in this session"));
        };
        let (path, earlier_turns) = self.conversation_window(session, parent, turns)?;
        entries(session, path, earlier_turns)
    }

    /// 读取节点的最新完整条目，供观察者在节点变化后整条替换。
    pub fn entry(&self, node: NodeRef) -> Result<ConversationEntry> {
        self.find_session(node.session_id)?
            .ok_or_else(|| missing("session", node.session_id))?;
        let content = self
            .find_node(node.session_id, node.node)?
            .ok_or_else(|| missing("node", node.node.0))?;
        match &content {
            Node::Root => Err(invalid("node", "root has no conversation entry")),
            Node::UserMessage { parent, .. } => {
                let (_, earlier_turns) = self.conversation_window(node.session_id, *parent, 0)?;
                Ok(ConversationEntry::UserMessage {
                    node: node.node,
                    content,
                    ordinal: earlier_turns + 1,
                })
            }
            Node::Step { turn_id, .. } => {
                let user = self.turn_user_node(node.session_id, *turn_id)?;
                if !matches!(
                    self.path_node(node.session_id, user)?,
                    Node::UserMessage { .. }
                ) {
                    return Err(corrupt(
                        "nodes",
                        &user.0.to_string(),
                        "expected user message",
                    ));
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

    /// 经 TurnId 索引定位本会话的 UserMessage，索引缺失或归属不符时报损坏。
    pub fn turn_user_node(&self, session: SessionId, turn: TurnId) -> Result<NodeId> {
        self.raw_turn_node(turn)?
            .filter(|node| node.session_id == session)
            .map(|node| node.node)
            .ok_or_else(|| corrupt("turn_index", &turn.to_string(), "missing turn user node"))
    }

    /// 只读指定 Step 的尝试标签，供生成中文字的推送守卫使用。
    pub fn attempt_is_sent(
        &self,
        session: SessionId,
        node: NodeId,
        key: ModelAttemptKey,
    ) -> Result<bool> {
        let step: StepAttempts = self.projected_node(session, node)?;
        if !matches!(step.kind, CountedNodeKind::Step) {
            return Err(corrupt(
                "nodes",
                &format!("{session}/{}", node.0),
                "expected step node",
            ));
        }
        Ok(step.turn_id == key.turn_id
            && step.step_ordinal == key.model_step_ordinal
            && step
                .attempts
                .get(key.attempt_ordinal as usize)
                .is_some_and(|attempt| matches!(attempt.progress.progress, AttemptStateKind::Sent)))
    }

    /// 经 TurnId 索引只读取本轮 agent，不装载输入与冻结配置。
    pub fn turn_agent(&self, session: SessionId, turn: TurnId) -> Result<AgentName> {
        let node = self.turn_user_node(session, turn)?;
        let user: TurnAgent = self.projected_node(session, node)?;
        if !matches!(user.kind, CountedNodeKind::UserMessage) || user.turn_id != turn {
            return Err(corrupt(
                "nodes",
                &format!("{session}/{}", node.0),
                "expected turn user message",
            ));
        }
        Ok(user.agent_name)
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
    ) -> Result<(Vec<(NodeId, Node)>, u64)> {
        let mut path = Vec::new();
        let mut earlier_turns = 0;
        // 窗口之外只解码计数字段；经 TurnId 索引跳过同轮其余 Step。
        while head != NodeId(0) {
            if turns > 0 {
                let node = self.path_node(session, head)?;
                let Some(parent) = node.parent() else { break };
                if matches!(node, Node::UserMessage { .. }) {
                    turns -= 1;
                }
                path.push((head, node));
                head = parent;
            } else {
                let node: CountedNode = self.projected_node(session, head)?;
                match node.kind {
                    CountedNodeKind::UserMessage => {
                        earlier_turns += 1;
                        head = node.parent;
                    }
                    CountedNodeKind::Step => {
                        head = self.turn_user_node(session, node.turn_id)?;
                    }
                }
            }
        }
        path.reverse();
        Ok((path, earlier_turns))
    }
}
