// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{codec::*, *};
use lancet_protocol::InvocationId;
use redb::ReadableTable;
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub(super) enum Selection {
    Session,
    Turn(TurnId),
    Accept(TurnId),
    Invocation(InvocationId),
    Node(NodeId),
}
pub(super) struct Scope {
    pub session: SessionId,
    pub turn: Option<TurnId>,
    pub nodes: BTreeSet<NodeId>,
}

// read/write 的表句柄不同；共用按键/范围加载，不创建另一份缓存或持久索引。
macro_rules! load_local {
    ($tx:expr, $session:expr, $selection:expr) => {{
        let mut session = $session;
        let selection = $selection;
        let sessions = $tx.open_table(SESSIONS).map_err(table_error)?;
        let turns = $tx.open_table(TURNS).map_err(table_error)?;
        let nodes = $tx.open_table(NODES).map_err(table_error)?;
        let steps = $tx.open_table(STEPS).map_err(table_error)?;
        let index = $tx.open_table(INDEX).map_err(table_error)?;
        let changes = $tx.open_table(CHANGES).map_err(table_error)?;
        let meta = $tx.open_table(META).map_err(table_error)?;
        let mut r = Records {
            last_change: decode("meta", "last_change_seq", meta.get("last_change_seq")?.ok_or_else(|| corrupt("meta", "last_change_seq", "missing metadata"))?.value())?,
            ..Default::default()
        };
        if r.last_change != ChangeSeq(changes.last()?.map_or(0, |(key, _)| key.value())) {
            return Err(corrupt("meta", "last_change_seq", "high water mark mismatch"));
        }
        let turn_id = match selection {
            Selection::Turn(id) | Selection::Accept(id) => Some(id),
            Selection::Invocation(id) => {
                let entry: InvocationIndexEntry = decode("invocation_index", &id.to_string(), index.get(id.to_string().as_str())?.ok_or_else(|| missing("invocation", id))?.value())?;
                Some(entry.address.message.turn_id)
            }
            _ => None,
        };
        if let Some(id) = turn_id {
            if let Some(value) = turns.get(id.to_string().as_str())? {
                let turn: TurnRecord = decode("turns", &id.to_string(), value.value())?;
                if matches!(selection, Selection::Accept(_)) { session = turn.session_id; }
                if turn.turn_id != id { return Err(corrupt("turns", &id.to_string(), "key/value mismatch")); }
                r.turns.insert(id, turn);
            }
        }
        let mut scope = Scope { session, turn: turn_id, nodes: BTreeSet::from([NodeId(0)]) };
        if let Some(value) = sessions.get(session.to_string().as_str())? {
            let s: SessionRecord = decode("sessions", &session.to_string(), value.value())?;
            if s.session_id != session { return Err(corrupt("sessions", &session.to_string(), "key/value mismatch")); }
            scope.nodes.insert(s.current);
            if matches!(selection, Selection::Session) { scope.turn = s.active_turn; }
            r.sessions.insert(session, s);
        }
        if let Selection::Node(node) = selection { scope.nodes.insert(node); }
        for n in &scope.nodes {
            if let Some(value) = nodes.get(node_key(session, *n).as_slice())? {
                let node: Node = decode("nodes", &format!("{session}/{}", n.0), value.value())?;
                if matches!(selection, Selection::Node(target) if target == *n) || matches!(selection, Selection::Session) && scope.turn.is_none() {
                    scope.turn = node.turn_id();
                }
                r.nodes.insert((session, *n), node);
            }
        }
        if let Some(id) = scope.turn {
            if !r.turns.contains_key(&id) {
                if let Some(value) = turns.get(id.to_string().as_str())? {
                    r.turns.insert(id, decode("turns", &id.to_string(), value.value())?);
                }
            }
            if let Some(t) = r.turns.get(&id) {
                if t.session_id != session { return Err(missing("turn", id)); }
                scope.nodes.insert(t.user_node);
                for entry in steps.range(step_key(StepKey {turn_id:id, ordinal:0}).as_slice()..=step_key(StepKey {turn_id:id, ordinal:u32::MAX}).as_slice())? {
                    let (key, value) = entry?;
                    let key = decode_step_key(key.value())?;
                    let step: StepRecord = decode("steps", &format!("{id}/{}", key.ordinal), value.value())?;
                    if step.key != key { return Err(corrupt("steps", &id.to_string(), "key/value mismatch")); }
                    if let StepState::Closed {end, ..} = &step.state { scope.nodes.extend(end.node()); }
                    r.steps.insert(key, step);
                }
            }
        }
        for n in &scope.nodes {
            if let std::collections::btree_map::Entry::Vacant(entry) = r.nodes.entry((session, *n)) {
                if let Some(value) = nodes.get(node_key(session, *n).as_slice())? {
                    entry.insert(decode("nodes", &format!("{session}/{}", n.0), value.value())?);
                }
            }
        }
        // 仅取直接引用的父节点和所属 Turn 头；不追读其历史 Step/调用。
        let parents: BTreeSet<_> = r.nodes.values().filter_map(Node::parent).collect();
        let mut owners: BTreeSet<_> = r.nodes.values().filter_map(Node::turn_id).collect();
        owners.extend(r.sessions.values().filter_map(|s| s.active_turn));
        for parent in parents {
            if let std::collections::btree_map::Entry::Vacant(entry) = r.nodes.entry((session, parent)) {
                if let Some(value) = nodes.get(node_key(session, parent).as_slice())? {
                    entry.insert(decode("nodes", &format!("{session}/{}", parent.0), value.value())?);
                }
            }
        }
        for owner in owners {
            if let std::collections::btree_map::Entry::Vacant(entry) = r.turns.entry(owner) {
                let value = turns.get(owner.to_string().as_str())?.ok_or_else(|| corrupt("turns", &owner.to_string(), "node owner missing"))?;
                entry.insert(decode("turns", &owner.to_string(), value.value())?);
            }
        }
        for n in &scope.nodes {
            if let Some(Node::Step {turn_id, step_ordinal, ..}) = r.nodes.get(&(session, *n)) {
                let key = StepKey {turn_id:*turn_id, ordinal:*step_ordinal};
                if let std::collections::btree_map::Entry::Vacant(entry) = r.steps.entry(key) {
                    let value = steps.get(step_key(key).as_slice())?.ok_or_else(|| corrupt("steps", &turn_id.to_string(), "node step missing"))?;
                    entry.insert(decode("steps", &turn_id.to_string(), value.value())?);
                }
            }
        }
        r.node_tip = nodes.range(node_key(session, NodeId(0)).as_slice()..=node_key(session, NodeId(u64::MAX)).as_slice())?.next_back().transpose()?.map(|(k, _)| decode_node_key(k.value()).map(|(_, n)| n)).transpose()?;
        for step in r.steps.values() {
            for slot in validate::slots(step) {
                let id = slot.binding.invocation_id;
                let entry: InvocationIndexEntry = decode("invocation_index", &id.to_string(), index.get(id.to_string().as_str())?.ok_or_else(|| corrupt("invocation_index", &id.to_string(), "missing index"))?.value())?;
                let seq = entry.last_change;
                let change = decode("invocation_changes", &seq.0.to_string(), changes.get(seq.0)?.ok_or_else(|| corrupt("invocation_changes", &seq.0.to_string(), "missing latest change"))?.value())?;
                r.index.insert(id, entry);
                r.changes.insert(seq, change);
            }
        }
        validate::local(&r, &scope)?;
        Ok::<_, StoreError>((r, scope))
    }};
}

pub(super) fn read(
    tx: &redb::ReadTransaction,
    session: SessionId,
    selection: Selection,
) -> Result<Records> {
    let (r, _) = load_local!(tx, session, selection)?;
    Ok(r)
}
pub(super) fn write(
    tx: &redb::WriteTransaction,
    session: SessionId,
    selection: Selection,
) -> Result<(Records, Scope)> {
    load_local!(tx, session, selection)
}
