// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use super::{codec::*, local::Selection};
use lancet_protocol::InvocationId;
use lattice_store::ProjectStoreKey;
use redb::ReadableTable;

pub(super) fn operator(slot: &InvocationSlot) -> OperatorInvocation {
    let state = match &slot.state {
        SlotState::Registered => OperatorInvocationState::Registered,
        SlotState::Started => OperatorInvocationState::Started,
        SlotState::Terminal(t) => {
            let (reason, material) = match &t.result.payload {
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
                    Some(OperatorReason::Failed {
                        code: e.code,
                        retriability: e.retriability.clone(),
                    }),
                    MaterialSummary::NoContent,
                ),
                ResultPayload::Rejected(r) => (
                    Some(OperatorReason::Rejected(r.clone())),
                    MaterialSummary::NoContent,
                ),
                ResultPayload::Cancelled(r) => (
                    Some(OperatorReason::Cancelled(r.clone())),
                    MaterialSummary::NoContent,
                ),
                ResultPayload::NotExecuted(r) => (
                    Some(OperatorReason::NotExecuted(r.clone())),
                    MaterialSummary::NoContent,
                ),
                ResultPayload::Indeterminate(r) => (
                    Some(OperatorReason::Indeterminate(r.clone())),
                    MaterialSummary::NoContent,
                ),
            };
            OperatorInvocationState::Terminal {
                outcome: t.result.payload.outcome(),
                reason,
                material,
                evidence: t.evidence.clone(),
            }
        }
    };
    OperatorInvocation {
        invocation_id: slot.binding.invocation_id,
        binding: slot.binding.identity.clone(),
        state,
    }
}
fn attempt(a: &AttemptRecord) -> AttemptDetail {
    AttemptDetail {
        attempt: a.attempt,
        origin: a.origin,
        usage: a.usage,
        end: match &a.end {
            AttemptEnd::Completed { step } => AttemptEndDetail::Completed { step: *step },
            AttemptEnd::Stopped { cause, diagnostic } => AttemptEndDetail::Stopped {
                cause: cause.clone(),
                diagnostic: diagnostic.provider_error.clone(),
            },
        },
    }
}
fn closed_operator(c: &ClosedInvocation) -> OperatorInvocation {
    operator(&InvocationSlot {
        binding: c.binding.clone(),
        state: SlotState::Terminal(c.terminal.clone()),
    })
}
fn detail(step: &StepRecord) -> StepDetail {
    StepDetail {
        key: step.key,
        attempts: step.attempts.iter().map(attempt).collect(),
        state: match &step.state {
            StepState::Active {
                active: ActiveStep::Requesting { attempt },
            } => StepDetailState::Requesting { attempt: *attempt },
            StepState::Active {
                active: ActiveStep::RetryReady { next_attempt },
            } => StepDetailState::RetryReady {
                next_attempt: *next_attempt,
            },
            StepState::Active {
                active: ActiveStep::Executing { slots, .. },
            } => StepDetailState::Executing {
                slots: slots.iter().map(operator).collect(),
            },
            StepState::Closed { end, invocations } => StepDetailState::Closed {
                end: end.clone(),
                slots: invocations.iter().map(closed_operator).collect(),
            },
        },
    }
}
fn positive(limit: usize) -> Result<()> {
    if limit == 0 {
        Err(invalid("limit", "page budget must be positive"))
    } else {
        Ok(())
    }
}
impl Records {
    pub(super) fn current_agent(&self, s: SessionId) -> Result<Option<String>> {
        let session = self.sessions.get(&s).ok_or_else(|| missing("session", s))?;
        let node = self
            .nodes
            .get(&(s, session.current))
            .ok_or_else(|| corrupt("nodes", &s.to_string(), "current node missing"))?;
        node.turn_id()
            .map(|t| {
                self.turns
                    .get(&t)
                    .map(|t| t.config.agent_name.clone())
                    .ok_or_else(|| corrupt("turns", &t.to_string(), "node turn missing"))
            })
            .transpose()
    }
    fn turn_detail(&self, s: SessionId, t: TurnId) -> Result<TurnDetail> {
        let t = self
            .turns
            .get(&t)
            .filter(|t| t.session_id == s)
            .ok_or_else(|| missing("turn", t))?;
        let steps = self.turn_steps(t.turn_id);
        Ok(TurnDetail {
            turn_id: t.turn_id,
            session_id: s,
            user_node: t.user_node,
            agent_name: t.config.agent_name.clone(),
            accepted_at: t.accepted_at,
            ended_at: match t.state {
                TurnState::Running => None,
                TurnState::Closed { ended_at, .. } => Some(ended_at),
            },
            summary: close::summary(&t.state),
            usage: close::usage(&steps),
            steps: steps.iter().map(detail).collect(),
        })
    }
}
fn matches(filter: &AuditFilter, project: &ProjectStoreKey, row: &OperatorInvocation) -> bool {
    *project == filter.project_store_key
        && filter
            .session_id
            .is_none_or(|s| s == row.binding.session_id)
        && filter.turn_id.is_none_or(|t| t == row.binding.turn_id)
        && filter
            .method
            .as_ref()
            .is_none_or(|m| *m == row.binding.method)
        && filter.outcome.is_none_or(
            |o| matches!(row.state,OperatorInvocationState::Terminal {outcome,..} if outcome==o),
        )
}

impl SessionRead {
    fn local(&self, session: SessionId, selection: Selection) -> Result<Records> {
        local::read(&self.tx, session, selection)
    }
    fn raw_turn(&self, id: TurnId) -> Result<Option<TurnRecord>> {
        self.tx
            .open_table(TURNS)
            .map_err(table_error)?
            .get(id.to_string().as_str())?
            .map(|v| decode("turns", &id.to_string(), v.value()))
            .transpose()
    }
    fn latest_change(&self) -> Result<ChangeSeq> {
        let meta = self.tx.open_table(META).map_err(table_error)?;
        let seq = decode(
            "meta",
            "last_change_seq",
            meta.get("last_change_seq")?
                .ok_or_else(|| corrupt("meta", "last_change_seq", "missing metadata"))?
                .value(),
        )?;
        let last = self
            .tx
            .open_table(CHANGES)
            .map_err(table_error)?
            .last()?
            .map_or(0, |(k, _)| k.value());
        if seq != ChangeSeq(last) {
            return Err(corrupt(
                "meta",
                "last_change_seq",
                "high water mark mismatch",
            ));
        }
        Ok(seq)
    }
    pub fn session(&self, id: SessionId) -> Result<SessionRecord> {
        self.local(id, Selection::Session)?
            .sessions
            .remove(&id)
            .ok_or_else(|| missing("session", id))
    }
    /// 内部组合层找回请求使用；不要把冻结配置直接序列化到公开 IPC。
    pub fn turn_record(&self, id: TurnId) -> Result<Option<TurnRecord>> {
        let Some(t) = self.raw_turn(id)? else {
            return Ok(None);
        };
        Ok(self
            .local(t.session_id, Selection::Turn(id))?
            .turns
            .remove(&id))
    }
    pub fn durable_turn(&self, session: SessionId, turn: TurnId) -> Result<DurableTurnState> {
        self.local(session, Selection::Turn(turn))?
            .durable(session, turn)
    }
    pub fn current_agent(&self, session: SessionId) -> Result<Option<String>> {
        self.local(session, Selection::Session)?
            .current_agent(session)
    }
    pub fn node(&self, node: NodeRef) -> Result<Node> {
        self.local(node.session_id, Selection::Node(node.node))?
            .nodes
            .remove(&(node.session_id, node.node))
            .ok_or_else(|| missing("node", node.node.0))
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
            let s = self.session(id)?;
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
                project_store_key: s.project_store_key,
                current: s.current,
                node_count,
                active_turn: s.active_turn,
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
        let session = self.session(id)?;
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
            let node = &value;
            let n = &n;
            let kind = match node {
                Node::Root => NodeKind::Root,
                Node::UserMessage { .. } => NodeKind::UserMessage,
                Node::Step { step_ordinal, .. } => NodeKind::Step {
                    ordinal: *step_ordinal,
                },
            };
            let turn_start = if let Node::UserMessage { turn_id, .. } = node {
                let t = self
                    .turn_record(*turn_id)?
                    .ok_or_else(|| missing("turn", turn_id))?;
                Some(TurnLogSummary {
                    turn_id: *turn_id,
                    summary: close::summary(&t.state),
                    agent_name: t.config.agent_name.clone(),
                })
            } else {
                None
            };
            nodes.push(LogRow {
                node: NodeHeader {
                    id: *n,
                    parent: node.parent(),
                    turn_id: node.turn_id(),
                    kind,
                },
                current: *n == session.current,
                turn_start,
            });
        }
        Ok(SessionLog {
            current_agent: self.current_agent(id)?,
            session,
            nodes,
        })
    }
    pub fn turn_detail(&self, session: SessionId, turn: TurnId) -> Result<TurnDetail> {
        self.local(session, Selection::Turn(turn))?
            .turn_detail(session, turn)
    }
    pub fn snapshot(
        &self,
        session: SessionId,
        selected: Option<TurnId>,
    ) -> Result<DurableSnapshot> {
        let r = self.local(
            session,
            selected.map_or(Selection::Session, Selection::Turn),
        )?;
        let s = r
            .sessions
            .get(&session)
            .ok_or_else(|| missing("session", session))?
            .clone();
        let selected = selected
            .or(s.active_turn)
            .or_else(|| r.nodes[&(session, s.current)].turn_id());
        let selected_turn = selected.map(|t| r.turn_detail(session, t)).transpose()?;
        let active = selected.and_then(|t| {
            r.turn_steps(t).into_iter().find_map(|step| {
                if let StepState::Active { active } = step.state {
                    Some(ActiveStepSnapshot {
                        key: step.key,
                        attempts: step.attempts.iter().map(attempt).collect(),
                        state: active,
                    })
                } else {
                    None
                }
            })
        });
        Ok(DurableSnapshot {
            history: HistoryAnchor {
                session_id: session,
                head: s.current,
            },
            current_agent: r.current_agent(session)?,
            session: s,
            selected_turn,
            active,
        })
    }
    /// 返回固定父链上节点的规范 JSON UTF-8 片段；IPC 层仍须计量转义后的帧大小。
    pub fn history_page(
        &self,
        anchor: &HistoryAnchor,
        cursor: Option<&HistoryCursor>,
        max_bytes: usize,
    ) -> Result<HistoryPage> {
        positive(max_bytes)?;
        if self
            .tx
            .open_table(SESSIONS)
            .map_err(table_error)?
            .get(anchor.session_id.to_string().as_str())?
            .is_none()
        {
            return Err(missing("session", anchor.session_id));
        }
        let mut path = Vec::new();
        let mut n = anchor.head;
        while n != NodeId(0) {
            let node = self
                .node(NodeRef {
                    session_id: anchor.session_id,
                    node: n,
                })
                .map_err(|e| match e {
                    StoreError::NotFound { .. } => StoreError::InvalidCursor,
                    e => e,
                })?;
            path.push(n);
            n = node.parent().ok_or(StoreError::InvalidCursor)?;
        }
        path.reverse();
        let (start, mut offset) = match cursor {
            None => (0, 0),
            Some(c) => {
                if c.anchor != *anchor {
                    return Err(StoreError::InvalidCursor);
                }
                let index = path
                    .iter()
                    .position(|n| *n == c.node)
                    .ok_or(StoreError::InvalidCursor)?;
                (
                    index,
                    usize::try_from(c.utf8_offset).map_err(|_| StoreError::InvalidCursor)?,
                )
            }
        };
        let mut fragments = Vec::new();
        let mut left = max_bytes;
        let mut next = None;
        for (index, n) in path.iter().enumerate().skip(start) {
            let text = String::from_utf8(codec::canonical(&self.node(NodeRef {
                session_id: anchor.session_id,
                node: *n,
            })?)?)
            .map_err(|_| corrupt("nodes", "encoding", "non-UTF8 canonical JSON"))?;
            if offset >= text.len() || !text.is_char_boundary(offset) {
                return Err(StoreError::InvalidCursor);
            }
            let mut end = offset.saturating_add(left).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            if end == offset {
                if fragments.is_empty() {
                    return Err(invalid("max_bytes", "page cannot fit next character"));
                }
                next = Some(HistoryCursor {
                    anchor: anchor.clone(),
                    node: *n,
                    utf8_offset: offset as u64,
                });
                break;
            }
            let complete = end == text.len();
            fragments.push(NodeFragment {
                node: *n,
                utf8_offset: offset as u64,
                json_fragment: text[offset..end].into(),
                complete,
            });
            left -= end - offset;
            if !complete {
                next = Some(HistoryCursor {
                    anchor: anchor.clone(),
                    node: *n,
                    utf8_offset: end as u64,
                });
                break;
            }
            offset = 0;
            if left == 0 {
                next = path.get(index + 1).map(|n| HistoryCursor {
                    anchor: anchor.clone(),
                    node: *n,
                    utf8_offset: 0,
                });
                break;
            }
        }
        Ok(HistoryPage {
            anchor: anchor.clone(),
            fragments,
            next,
        })
    }
    fn index_entry(&self, id: InvocationId) -> Result<InvocationIndexEntry> {
        let table = self.tx.open_table(INDEX).map_err(table_error)?;
        decode(
            "invocation_index",
            &id.to_string(),
            table
                .get(id.to_string().as_str())?
                .ok_or_else(|| corrupt("invocation_index", &id.to_string(), "missing index"))?
                .value(),
        )
    }
    fn change(&self, seq: ChangeSeq) -> Result<InvocationChange> {
        let table = self.tx.open_table(CHANGES).map_err(table_error)?;
        let change: InvocationChange = decode(
            "invocation_changes",
            &seq.0.to_string(),
            table
                .get(seq.0)?
                .ok_or_else(|| corrupt("invocation_changes", &seq.0.to_string(), "missing change"))?
                .value(),
        )?;
        if change.sequence != seq {
            return Err(corrupt(
                "invocation_changes",
                &seq.0.to_string(),
                "key/value mismatch",
            ));
        }
        Ok(change)
    }
    fn checked_invocation(&self, id: InvocationId) -> Result<InvocationChange> {
        let index = self.index_entry(id)?;
        let address = index.address.message;
        let r = self.local(address.session_id, Selection::Turn(address.turn_id))?;
        if r.index.get(&id) != Some(&index) {
            return Err(corrupt("invocation_index", &id.to_string(), "orphan index"));
        }
        let change = r.changes.get(&index.last_change).ok_or_else(|| {
            corrupt(
                "invocation_changes",
                &id.to_string(),
                "missing latest change",
            )
        })?;
        Ok(change.clone())
    }
    pub fn query_audit(
        &self,
        filter: &AuditFilter,
        after: Option<InvocationId>,
        limit: usize,
    ) -> Result<AuditPage> {
        positive(limit)?;
        let latest_change = self.latest_change()?;
        let table = self.tx.open_table(INDEX).map_err(table_error)?;
        let after = after.map(|id| id.to_string());
        let mut rows = Vec::new();
        let mut next = None;
        for entry in table.range::<&str>((
            after
                .as_deref()
                .map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded),
            std::ops::Bound::Unbounded,
        ))? {
            let (key, value) = entry?;
            let id = InvocationId::parse(key.value())
                .map_err(|_| corrupt("invocation_index", "key", "invalid id"))?;
            let index: InvocationIndexEntry =
                decode("invocation_index", key.value(), value.value())?;
            let change = self.change(index.last_change)?;
            if !matches(filter, &change.project_store_key, &change.row) {
                continue;
            }
            let change = self.checked_invocation(id)?;
            if rows.len() == limit {
                next = rows.last().map(|r: &OperatorInvocation| r.invocation_id);
                break;
            }
            rows.push(change.row);
        }
        Ok(AuditPage {
            latest_change,
            rows,
            next,
        })
    }
    pub fn tail_audit(
        &self,
        filter: &AuditFilter,
        after: ChangeSeq,
        limit: usize,
    ) -> Result<AuditTailPage> {
        positive(limit)?;
        let latest_change = self.latest_change()?;
        if after > latest_change {
            return Err(StoreError::InvalidCursor);
        }
        let table = self.tx.open_table(CHANGES).map_err(table_error)?;
        let mut next_after = after;
        let mut rows = Vec::new();
        for entry in table
            .range((
                std::ops::Bound::Excluded(after.0),
                std::ops::Bound::Unbounded,
            ))?
            .take(limit)
        {
            let (key, value) = entry?;
            if key.value() != next_sequence(next_after.0, "change")? {
                return Err(corrupt(
                    "invocation_changes",
                    &key.value().to_string(),
                    "missing sequence",
                ));
            }
            let change: InvocationChange = decode(
                "invocation_changes",
                &key.value().to_string(),
                value.value(),
            )?;
            next_after = ChangeSeq(key.value());
            if change.sequence != next_after {
                return Err(corrupt(
                    "invocation_changes",
                    &key.value().to_string(),
                    "key/value mismatch",
                ));
            }
            if matches(filter, &change.project_store_key, &change.row) {
                let latest = self.checked_invocation(change.row.invocation_id)?;
                let valid_state = match &change.row.state {
                    OperatorInvocationState::Registered => true,
                    OperatorInvocationState::Started => !matches!(
                        latest.row.state,
                        OperatorInvocationState::Registered
                            | OperatorInvocationState::Terminal {
                                outcome: lattice_conversation::ToolResultOutcome::NotExecuted,
                                ..
                            }
                    ),
                    OperatorInvocationState::Terminal { .. } => change == latest,
                };
                if latest.project_store_key != change.project_store_key
                    || latest.row.binding != change.row.binding
                    || change.sequence > latest.sequence
                    || !valid_state
                {
                    return Err(corrupt(
                        "invocation_changes",
                        &key.value().to_string(),
                        "invalid change relation",
                    ));
                }
                rows.push(change);
            }
        }
        if let Some(last) = rows.last() {
            let mut histories: std::collections::BTreeMap<_, _> = rows
                .iter()
                .map(|change| (change.row.invocation_id, (change, None)))
                .collect();
            // ponytail: 无调用前态索引，跨页须扫描日志前缀；若成本成为瓶颈再加索引。
            // 只核验本页匹配调用，不加载无关 Session/Turn，也不以 outcome 过滤前态。
            for entry in table.range(..=last.sequence.0)? {
                let (key, value) = entry?;
                let change: InvocationChange = decode(
                    "invocation_changes",
                    &key.value().to_string(),
                    value.value(),
                )?;
                let Some((target, previous)) = histories.get_mut(&change.row.invocation_id) else {
                    continue;
                };
                if change.sequence != ChangeSeq(key.value())
                    || change.project_store_key != target.project_store_key
                    || change.row.binding != target.row.binding
                    || !validate::invocation_transition(previous.as_ref(), &change.row.state)
                {
                    return Err(corrupt(
                        "invocation_changes",
                        &key.value().to_string(),
                        "illegal lifecycle transition or identity mismatch",
                    ));
                }
                *previous = Some(change.row.state);
            }
        }
        Ok(AuditTailPage {
            latest_change,
            rows,
            next_after,
            done: next_after == latest_change,
        })
    }
    pub fn replay_audit(
        &self,
        project: &ProjectStoreKey,
        session: SessionId,
        turn: TurnId,
    ) -> Result<AuditReplay> {
        let r = self.local(session, Selection::Turn(turn))?;
        let state = r.durable(session, turn)?;
        if state.turn.project_store_key != *project {
            return Err(invalid("project", "turn project mismatch"));
        }
        let calls = state
            .steps
            .iter()
            .flat_map(validate::slots)
            .map(|s| operator(&s))
            .collect();
        Ok(AuditReplay {
            session_id: session,
            turn_id: turn,
            latest_change: r.last_change,
            calls,
        })
    }
    pub fn operator_status(&self, project: &ProjectStoreKey) -> Result<OperatorStatus> {
        let latest_change = self.latest_change()?;
        let table = self.tx.open_table(CHANGES).map_err(table_error)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut recent = Vec::new();
        for entry in table.iter()?.rev() {
            let (key, value) = entry?;
            let change: InvocationChange = decode(
                "invocation_changes",
                &key.value().to_string(),
                value.value(),
            )?;
            if change.sequence != ChangeSeq(key.value()) {
                return Err(corrupt(
                    "invocation_changes",
                    &key.value().to_string(),
                    "key/value mismatch",
                ));
            }
            if change.project_store_key == *project && seen.insert(change.row.invocation_id) {
                let latest = self.checked_invocation(change.row.invocation_id)?;
                if latest != change {
                    return Err(corrupt(
                        "invocation_changes",
                        &key.value().to_string(),
                        "latest change mismatch",
                    ));
                }
                recent.push(latest.row);
                if recent.len() == 20 {
                    break;
                }
            }
        }
        Ok(OperatorStatus {
            latest_change,
            recent,
        })
    }
}
