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
        let nodes = $tx.open_table(NODES).map_err(table_error)?;
        let configs = $tx.open_table(CONFIGS).map_err(table_error)?;
        let turn_index = $tx.open_table(TURN_INDEX).map_err(table_error)?;
        let index = $tx.open_table(INDEX).map_err(table_error)?;
        let changes = $tx.open_table(CHANGES).map_err(table_error)?;
        let meta = $tx.open_table(META).map_err(table_error)?;
        let mut r = Records {
            last_change: decode(
                "meta",
                "last_change_seq",
                meta.get("last_change_seq")?
                    .ok_or_else(|| corrupt("meta", "last_change_seq", "missing metadata"))?
                    .value(),
            )?,
            ..Default::default()
        };
        if r.last_change != ChangeSeq(changes.last()?.map_or(0, |(key, _)| key.value())) {
            return Err(corrupt("meta", "last_change_seq", "high water mark mismatch"));
        }
        let turn_id = match selection {
            Selection::Turn(id) | Selection::Accept(id) => Some(id),
            Selection::Invocation(id) => {
                let entry: InvocationIndexEntry = decode(
                    "invocation_index",
                    &id.to_string(),
                    index
                        .get(id.to_string().as_str())?
                        .ok_or_else(|| missing("invocation", id))?
                        .value(),
                )?;
                Some(entry.address.message.turn_id)
            }
            _ => None,
        };
        if let Some(id) = turn_id {
            if let Some(value) = turn_index.get(id.to_string().as_str())? {
                let node_ref: NodeRef = decode("turn_index", &id.to_string(), value.value())?;
                if matches!(selection, Selection::Accept(_)) {
                    session = node_ref.session_id;
                }
                r.turn_index.insert(id, node_ref);
            }
        }
        let mut scope = Scope {
            session,
            turn: turn_id,
            nodes: BTreeSet::from([NodeId(0)]),
        };
        r.node_tip = nodes
            .range(
                node_key(session, NodeId(0)).as_slice()
                    ..=node_key(session, NodeId(u64::MAX)).as_slice(),
            )?
            .next_back()
            .transpose()?
            .map(|(k, _)| decode_node_key(k.value()).map(|(_, n)| n))
            .transpose()?;
        if let Some(tip) = r.node_tip {
            scope.nodes.insert(tip);
        }
        if let Some(value) = sessions.get(session.to_string().as_str())? {
            let s: SessionRecord = decode("sessions", &session.to_string(), value.value())?;
            if s.session_id != session {
                return Err(corrupt("sessions", &session.to_string(), "key/value mismatch"));
            }
            scope.nodes.insert(s.current);
            r.sessions.insert(session, s);
        }
        if let Selection::Node(node) = selection {
            scope.nodes.insert(node);
        }
        for n in &scope.nodes {
            if let Some(value) = nodes.get(node_key(session, *n).as_slice())? {
                let node: Node = decode("nodes", &format!("{session}/{}", n.0), value.value())?;
                if matches!(selection, Selection::Node(target) if target == *n)
                    || matches!(selection, Selection::Session)
                        && r.sessions.get(&session).is_some_and(|s| s.current == *n)
                {
                    scope.turn = node.turn_id();
                }
                r.nodes.insert((session, *n), node);
            }
        }
        let mut turns_to_load = BTreeSet::new();
        let mut node_turns = BTreeSet::new();
        if let Some(id) = scope.turn {
            turns_to_load.insert(id);
        }
        if let Some(tip) = r.node_tip
            && let Some(id) = r.nodes.get(&(session, tip)).and_then(Node::turn_id)
        {
            turns_to_load.insert(id);
            node_turns.insert(id);
        }
        if let Some(s) = r.sessions.get(&session)
            && let Some(id) = r.nodes.get(&(session, s.current)).and_then(Node::turn_id)
        {
            turns_to_load.insert(id);
            node_turns.insert(id);
        }
        if let Selection::Node(target) = selection
            && let Some(id) = r.nodes.get(&(session, target)).and_then(Node::turn_id)
        {
            node_turns.insert(id);
        }
        for id in turns_to_load {
            let from_node = node_turns.contains(&id);
            if !r.turn_index.contains_key(&id) {
                if let Some(value) = turn_index.get(id.to_string().as_str())? {
                    let node_ref: NodeRef = decode("turn_index", &id.to_string(), value.value())?;
                    r.turn_index.insert(id, node_ref);
                } else if from_node {
                    return Err(corrupt("turn_index", &id.to_string(), "missing turn index"));
                }
            }
            if let Some(&node_ref) = r.turn_index.get(&id) {
                if node_ref.session_id != session {
                    if !from_node && scope.turn == Some(id) {
                        return Err(missing("turn", id));
                    }
                    return Err(corrupt(
                        "turn_index",
                        &id.to_string(),
                        "turn belongs to another session",
                    ));
                }
                scope.nodes.insert(node_ref.node);
                for entry in nodes.range(
                    node_key(session, node_ref.node).as_slice()
                        ..=node_key(session, NodeId(u64::MAX)).as_slice(),
                )? {
                    let (key, value) = entry?;
                    let (_, n) = decode_node_key(key.value())?;
                    let decoded: Node =
                        decode("nodes", &format!("{session}/{}", n.0), value.value())?;
                    if n != node_ref.node && matches!(decoded, Node::UserMessage { .. } | Node::Root)
                    {
                        break;
                    }
                    scope.nodes.insert(n);
                    r.nodes.insert((session, n), decoded);
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
        // 仅取直接引用的父节点和所属 Turn 的 UserMessage 节点；不追读无关分支。
        let parents: BTreeSet<_> = r.nodes.values().filter_map(Node::parent).collect();
        for parent in parents {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                r.nodes.entry((session, parent))
            {
                if let Some(value) = nodes.get(node_key(session, parent).as_slice())? {
                    entry.insert(decode(
                        "nodes",
                        &format!("{session}/{}", parent.0),
                        value.value(),
                    )?);
                }
            }
        }
        let owners: BTreeSet<_> = r.nodes.values().filter_map(Node::turn_id).collect();
        for owner in owners {
            if let std::collections::btree_map::Entry::Vacant(entry) = r.turn_index.entry(owner) {
                let value = turn_index
                    .get(owner.to_string().as_str())?
                    .ok_or_else(|| corrupt("turn_index", &owner.to_string(), "node owner missing"))?;
                entry.insert(decode("turn_index", &owner.to_string(), value.value())?);
            }
            let node_ref = r.turn_index[&owner];
            if node_ref.session_id != session {
                return Err(corrupt(
                    "turn_index",
                    &owner.to_string(),
                    "node owner session mismatch",
                ));
            }
            if let std::collections::btree_map::Entry::Vacant(entry) =
                r.nodes.entry((session, node_ref.node))
            {
                let value = nodes
                    .get(node_key(session, node_ref.node).as_slice())?
                    .ok_or_else(|| {
                        corrupt(
                            "nodes",
                            &format!("{session}/{}", node_ref.node.0),
                            "missing turn user node",
                        )
                    })?;
                entry.insert(decode(
                    "nodes",
                    &format!("{session}/{}", node_ref.node.0),
                    value.value(),
                )?);
            }
        }
        let digests: BTreeSet<_> = r
            .nodes
            .values()
            .filter_map(|n| match n {
                Node::UserMessage { config_digest, .. } => Some(*config_digest),
                _ => None,
            })
            .collect();
        for digest in digests {
            if let std::collections::btree_map::Entry::Vacant(entry) = r.configs.entry(digest) {
                let key = config_key(&digest);
                let value = configs
                    .get(key.as_str())?
                    .ok_or_else(|| corrupt("configs", &key, "missing referenced config"))?;
                entry.insert(decode("configs", &key, value.value())?);
            }
        }
        let slots: Vec<_> = r
            .nodes
            .values()
            .filter_map(|n| match n {
                Node::Step { slots, .. } => Some(slots.as_slice()),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect();
        for slot in slots {
            let id = slot.binding.invocation_id;
            let entry: InvocationIndexEntry = decode(
                "invocation_index",
                &id.to_string(),
                index
                    .get(id.to_string().as_str())?
                    .ok_or_else(|| corrupt("invocation_index", &id.to_string(), "missing index"))?
                    .value(),
            )?;
            let seq = entry.last_change;
            let change = decode(
                "invocation_changes",
                &seq.0.to_string(),
                changes
                    .get(seq.0)?
                    .ok_or_else(|| {
                        corrupt(
                            "invocation_changes",
                            &seq.0.to_string(),
                            "missing latest change",
                        )
                    })?
                    .value(),
            )?;
            r.index.insert(id, entry);
            r.changes.insert(seq, change);
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
