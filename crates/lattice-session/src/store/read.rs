// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::*;
use super::{codec::*, local::Selection};
use lancet_protocol::InvocationId;
use lattice_conversation::ToolResultOutcome;
use lattice_store::ProjectStoreKey;
use redb::ReadableTable;

fn payload_summary(
    payload: &ResultPayload,
) -> (ToolResultOutcome, Option<OperatorReason>, MaterialSummary) {
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
            Some(OperatorReason::Failed {
                code: e.code,
                retriability: e.retriability.clone(),
            }),
            MaterialSummary::NoContent,
        ),
        ResultPayload::NotExecuted => (None, MaterialSummary::NoContent),
        ResultPayload::Indeterminate(cause) => (
            Some(OperatorReason::Indeterminate(*cause)),
            MaterialSummary::NoContent,
        ),
    };
    (payload.outcome(), reason, material)
}

pub(super) fn operator(slot: &InvocationSlot) -> OperatorInvocation {
    let state = match &slot.progress {
        SlotProgress::Registered => OperatorInvocationState::Registered,
        SlotProgress::Started => OperatorInvocationState::Started,
        SlotProgress::Terminal { result, evidence } => {
            let (outcome, reason, material) = payload_summary(&result.payload);
            OperatorInvocationState::Terminal {
                outcome,
                reason,
                material,
                evidence: evidence.clone(),
            }
        }
    };
    OperatorInvocation {
        invocation_id: slot.binding.invocation_id,
        binding: slot.binding.identity.clone(),
        state,
    }
}

fn slot_detail(slot: &InvocationSlot) -> SlotDetail {
    let (result, evidence) = close::resolve_slot(slot);
    let (outcome, reason, material) = payload_summary(&result.payload);
    SlotDetail {
        invocation_id: slot.binding.invocation_id,
        binding: slot.binding.identity.clone(),
        progress: operator(slot).state,
        outcome,
        reason,
        material,
        evidence,
    }
}

fn attempt(a: &AttemptRecord) -> AttemptDetail {
    let progress = match &a.progress {
        AttemptProgress::Sent => AttemptProgressDetail::Sent,
        AttemptProgress::Ended { end, diagnostic } => AttemptProgressDetail::Ended {
            end: end.clone(),
            diagnostic: diagnostic.provider_error.clone(),
        },
    };
    let (end, diagnostic) = close::resolve_attempt(a);
    let end = match end {
        AttemptEnd::Completed { step } => AttemptEndDetail::Completed { step },
        AttemptEnd::Stopped { cause } => AttemptEndDetail::Stopped {
            cause,
            diagnostic: diagnostic.provider_error,
        },
    };
    AttemptDetail {
        attempt: a.attempt,
        origin: a.origin,
        usage: a.usage,
        progress,
        end,
        retry_delay_ms: a.retry_delay_ms,
    }
}

fn detail(node: NodeId, step: &Node) -> Result<StepDetail> {
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
        attempts: attempts.iter().map(attempt).collect(),
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

impl Records {
    pub(super) fn current_agent(&self, s: SessionId) -> Result<Option<String>> {
        let session = self.sessions.get(&s).ok_or_else(|| missing("session", s))?;
        let node = self
            .nodes
            .get(&(s, session.current))
            .ok_or_else(|| corrupt("nodes", &s.to_string(), "current node missing"))?;
        node.turn_id()
            .map(|t| {
                let user_ref = self
                    .turn_index
                    .get(&t)
                    .filter(|r| r.session_id == s)
                    .ok_or_else(|| corrupt("turn_index", &t.to_string(), "node turn missing"))?;
                match self.nodes.get(&(s, user_ref.node)) {
                    Some(Node::UserMessage { agent_name, .. }) => Ok(agent_name.clone()),
                    _ => Err(corrupt(
                        "nodes",
                        &format!("{s}/{}", user_ref.node.0),
                        "missing turn user node",
                    )),
                }
            })
            .transpose()
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
            .map(|(node_id, node)| detail(*node_id, node))
            .collect::<Result<Vec<_>>>()?;
        Ok(TurnDetail {
            turn_id: *turn_id,
            session_id: s,
            user_node: user_ref.node,
            agent_name: agent_name.clone(),
            accepted_at: *accepted_at,
            ended_at: *ended_at,
            summary,
            usage,
            steps,
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
            |o| matches!(row.state, OperatorInvocationState::Terminal { outcome, .. } if outcome == o),
        )
}

impl SessionRead {
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

    fn raw_session(&self, id: SessionId) -> Result<SessionRecord> {
        let s: SessionRecord = self.decode(
            "sessions",
            &id.to_string(),
            self.tx
                .open_table(SESSIONS)
                .map_err(table_error)?
                .get(id.to_string().as_str())?
                .ok_or_else(|| corrupt("sessions", &id.to_string(), "missing session"))?
                .value(),
        )?;
        if s.session_id != id {
            return Err(corrupt("sessions", &id.to_string(), "key/value mismatch"));
        }
        Ok(s)
    }

    fn latest_change(&self) -> Result<ChangeSeq> {
        let meta = self.tx.open_table(META).map_err(table_error)?;
        let seq = self.decode(
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
                project_store_key: s.project_store_key,
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
        let current_node = r
            .nodes
            .get(&(session, s.current))
            .ok_or_else(|| corrupt("nodes", &session.to_string(), "current node missing"))?;
        let selected = selected.or_else(|| current_node.turn_id());
        let selected_turn = selected.map(|t| r.turn_detail(session, t)).transpose()?;
        let (head, unclosed_step) = match current_node {
            Node::Step {
                parent, progress, ..
            } if !progress.is_closed() => (*parent, Some((s.current, current_node.clone()))),
            _ => (s.current, None),
        };
        Ok(DurableSnapshot {
            history: HistoryAnchor {
                session_id: session,
                head,
            },
            current_agent: r.current_agent(session)?,
            session: s,
            selected_turn,
            unclosed_step,
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
        self.session(anchor.session_id)?;
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
            if n == anchor.head
                && matches!(&node, Node::Step { progress, .. } if !progress.is_closed())
            {
                return Err(StoreError::InvalidCursor);
            }
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
            let node = self.node(NodeRef {
                session_id: anchor.session_id,
                node: *n,
            })?;
            let turn_id = node.turn_id().ok_or(StoreError::InvalidCursor)?;
            let turn = self.turn_detail(anchor.session_id, turn_id)?;
            let retries = node.retry_notices()?;
            let text = String::from_utf8(codec::canonical(&node)?)
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
                agent_name: turn.agent_name,
                summary: turn.summary,
                retries,
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
        self.decode(
            "invocation_index",
            &id.to_string(),
            table
                .get(id.to_string().as_str())?
                .ok_or_else(|| corrupt("invocation_index", &id.to_string(), "missing index"))?
                .value(),
        )
    }

    fn stored_change(&self, seq: ChangeSeq) -> Result<StoredInvocationChange> {
        let table = self.tx.open_table(CHANGES).map_err(table_error)?;
        let change: StoredInvocationChange = self.decode(
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
        let session = r.sessions.get(&address.session_id).ok_or_else(|| {
            corrupt(
                "sessions",
                &address.session_id.to_string(),
                "missing session",
            )
        })?;
        let slot = r
            .nodes
            .values()
            .filter_map(|node| match node {
                Node::Step { slots, .. } => Some(slots.as_slice()),
                _ => None,
            })
            .flatten()
            .find(|slot| slot.binding.invocation_id == id)
            .ok_or_else(|| corrupt("invocation_index", &id.to_string(), "missing slot"))?;
        let change = r.changes.get(&index.last_change).ok_or_else(|| {
            corrupt(
                "invocation_changes",
                &id.to_string(),
                "missing latest change",
            )
        })?;
        if change.sequence != index.last_change
            || change.invocation_id != id
            || change.stage != validate::stage(&slot.progress)
        {
            return Err(corrupt(
                "invocation_changes",
                &id.to_string(),
                "latest change mismatch",
            ));
        }
        Ok(InvocationChange {
            sequence: index.last_change,
            project_store_key: session.project_store_key,
            row: operator(slot),
        })
    }

    fn resolve_change(&self, stored: StoredInvocationChange) -> Result<InvocationChange> {
        let latest = self.checked_invocation(stored.invocation_id)?;
        let latest_stage = match &latest.row.state {
            OperatorInvocationState::Registered => InvocationStage::Registered,
            OperatorInvocationState::Started => InvocationStage::Started,
            OperatorInvocationState::Terminal { .. } => InvocationStage::Terminal,
        };
        let valid_relation = if stored.stage == latest_stage {
            stored.sequence == latest.sequence
        } else {
            matches!(
                (stored.stage, latest_stage),
                (InvocationStage::Registered, InvocationStage::Started)
                    | (InvocationStage::Registered, InvocationStage::Terminal)
                    | (InvocationStage::Started, InvocationStage::Terminal)
            ) && stored.sequence < latest.sequence
        };
        if !valid_relation {
            return Err(corrupt(
                "invocation_changes",
                &stored.sequence.0.to_string(),
                "invalid change relation",
            ));
        }
        let state = match stored.stage {
            InvocationStage::Registered => OperatorInvocationState::Registered,
            InvocationStage::Started => OperatorInvocationState::Started,
            InvocationStage::Terminal => latest.row.state,
        };
        Ok(InvocationChange {
            sequence: stored.sequence,
            project_store_key: latest.project_store_key,
            row: OperatorInvocation {
                invocation_id: stored.invocation_id,
                binding: latest.row.binding,
                state,
            },
        })
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
                self.decode("invocation_index", key.value(), value.value())?;
            let stored = self.stored_change(index.last_change)?;
            if stored.invocation_id != id {
                return Err(corrupt(
                    "invocation_index",
                    &id.to_string(),
                    "latest change mismatch",
                ));
            }
            let session = self.raw_session(index.address.message.session_id)?;
            if session.project_store_key != filter.project_store_key
                || filter
                    .session_id
                    .is_some_and(|s| s != index.address.message.session_id)
                || filter
                    .turn_id
                    .is_some_and(|t| t != index.address.message.turn_id)
            {
                continue;
            }
            let change = self.checked_invocation(id)?;
            if !matches(filter, &change.project_store_key, &change.row) {
                continue;
            }
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
            let stored: StoredInvocationChange = self.decode(
                "invocation_changes",
                &key.value().to_string(),
                value.value(),
            )?;
            next_after = ChangeSeq(key.value());
            if stored.sequence != next_after {
                return Err(corrupt(
                    "invocation_changes",
                    &key.value().to_string(),
                    "key/value mismatch",
                ));
            }
            let index = self.index_entry(stored.invocation_id)?;
            let session = self.raw_session(index.address.message.session_id)?;
            if session.project_store_key != filter.project_store_key
                || filter
                    .session_id
                    .is_some_and(|s| s != index.address.message.session_id)
                || filter
                    .turn_id
                    .is_some_and(|t| t != index.address.message.turn_id)
            {
                continue;
            }
            let change = self.resolve_change(stored)?;
            if matches(filter, &change.project_store_key, &change.row) {
                rows.push(change);
            }
        }
        if let Some(last) = rows.last() {
            let mut histories: std::collections::BTreeMap<_, _> = rows
                .iter()
                .map(|change| (change.row.invocation_id, None))
                .collect();
            // 无调用前态索引，跨页须扫描日志前缀；若成本成为瓶颈再加索引。
            // 只核验本页匹配调用，不加载无关 Session/Turn，也不以 outcome 过滤前态。
            for entry in table.range(..=last.sequence.0)? {
                let (key, value) = entry?;
                let change: StoredInvocationChange = self.decode(
                    "invocation_changes",
                    &key.value().to_string(),
                    value.value(),
                )?;
                let Some(previous) = histories.get_mut(&change.invocation_id) else {
                    continue;
                };
                if change.sequence != ChangeSeq(key.value())
                    || !validate::invocation_transition(*previous, change.stage)
                {
                    return Err(corrupt(
                        "invocation_changes",
                        &key.value().to_string(),
                        "illegal lifecycle transition or identity mismatch",
                    ));
                }
                *previous = Some(change.stage);
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
        let s = r
            .sessions
            .get(&session)
            .ok_or_else(|| missing("session", session))?;
        let steps = r.turn_steps(session, turn)?;
        if s.project_store_key != *project {
            return Err(invalid("project", "turn project mismatch"));
        }
        let calls = steps
            .iter()
            .flat_map(|(_, node)| match node {
                Node::Step { slots, .. } => slots.as_slice(),
                _ => &[],
            })
            .map(operator)
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
            let stored: StoredInvocationChange = self.decode(
                "invocation_changes",
                &key.value().to_string(),
                value.value(),
            )?;
            if stored.sequence != ChangeSeq(key.value()) {
                return Err(corrupt(
                    "invocation_changes",
                    &key.value().to_string(),
                    "key/value mismatch",
                ));
            }
            let index = self.index_entry(stored.invocation_id)?;
            let session = self.raw_session(index.address.message.session_id)?;
            if session.project_store_key == *project && seen.insert(stored.invocation_id) {
                let latest = self.checked_invocation(stored.invocation_id)?;
                let change = self.resolve_change(stored)?;
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
