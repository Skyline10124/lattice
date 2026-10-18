// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 观察者只保存待读取节点与已发位置；正文只有当前尝试的一份累积。
use super::types::{AttemptSegments, TextSegment, TurnEvent};
use crate::local::{SessionHead, TextUpdate, ViewUpdate, WATCH_TURNS};
use lattice_session::store::{
    AttemptProgress, ConversationEntry, Node, NodeId, NodeRef, SessionRead, SessionStore,
    StoreError,
};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, Weak},
};

#[derive(Default)]
pub(crate) struct PushState {
    initial: bool,
    reload: bool,
    path_replaced: bool,
    pending: BTreeSet<NodeId>,
    head_changed: bool,
    sent: Option<(NodeId, u32, usize)>,
}
#[derive(Default)]
struct ObservationState {
    owner_generation: u64,
    user_node: Option<NodeId>,
    current: Option<NodeId>,
    attempt: Option<AttemptSegments>,
    watchers: Vec<Weak<Mutex<PushState>>>,
    #[cfg(test)]
    events: Vec<TurnEvent>,
}
#[cfg(test)]
type AcceptedReadFailure = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);
pub(crate) struct ObserverSink {
    state: Mutex<ObservationState>,
    changed: tokio::sync::watch::Sender<()>,
    #[cfg(test)]
    pub(crate) accepted_read_failure: Mutex<Option<AcceptedReadFailure>>,
    #[cfg(test)]
    pub(crate) before_accept: Mutex<Option<AcceptedReadFailure>>,
}
impl Default for ObserverSink {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changed: tokio::sync::watch::channel(()).0,
            #[cfg(test)]
            accepted_read_failure: Mutex::default(),
            #[cfg(test)]
            before_accept: Mutex::default(),
        }
    }
}
impl ObserverSink {
    pub(crate) fn owner_generation(&self) -> u64 {
        self.state
            .lock()
            .expect("observation lock")
            .owner_generation
    }
    pub(crate) fn owner_changed(&self) {
        let mut state = self.state.lock().expect("observation lock");
        state.owner_generation += 1;
        // Owner 与诊断不在会话库里；每次接管或退场都重读，不能走纯文字分支。
        state.watchers.retain(|watcher| {
            let Some(watcher) = watcher.upgrade() else {
                return false;
            };
            watcher.lock().expect("push state lock").head_changed = true;
            true
        });
        self.changed.send_replace(());
    }
    pub(crate) fn refresh_head(&self, cursor: &Mutex<PushState>) {
        let _state = self.state.lock().expect("observation lock");
        cursor.lock().expect("push state lock").head_changed = true;
    }
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.changed.subscribe()
    }
    pub(crate) fn register(&self) -> Arc<Mutex<PushState>> {
        let cursor = Arc::new(Mutex::new(PushState {
            initial: true,
            ..Default::default()
        }));
        let mut state = self.state.lock().expect("observation lock");
        state.watchers.retain(|watcher| watcher.strong_count() != 0);
        state.watchers.push(Arc::downgrade(&cursor));
        cursor
    }
    fn mark(state: &mut ObservationState, nodes: &[NodeId], replace: bool) {
        state.watchers.retain(|watcher| {
            let Some(watcher) = watcher.upgrade() else {
                return false;
            };
            let mut watcher = watcher.lock().expect("push state lock");
            if replace {
                watcher.path_replaced = true;
                watcher.sent = None;
                watcher.pending.clear();
            }
            watcher.pending.extend(nodes);
            true
        });
    }
    pub(crate) fn replace_path(&self, position: NodeId) {
        let mut state = self.state.lock().expect("observation lock");
        state.user_node = None;
        state.current = (position != NodeId(0)).then_some(position);
        state.attempt = None;
        Self::mark(&mut state, &[], true);
        self.changed.send_replace(());
    }
    pub(crate) fn path_replacement_pending(&self, cursor: &Mutex<PushState>) -> bool {
        cursor.lock().expect("push state lock").path_replaced
    }
    pub(crate) fn retry_capture(
        &self,
        cursor: &Mutex<PushState>,
        first: bool,
        updates: &[ViewUpdate],
    ) {
        let _state = self.state.lock().expect("observation lock");
        let mut cursor = cursor.lock().expect("push state lock");
        let replaced =
            matches!(updates.first(), Some(ViewUpdate::Head(head)) if head.path_replaced);
        // 这批尚未发送。仅真正换路径才丢弃旧节点，重读窗口不能吞掉窗口外的最终更新。
        if !first && !replaced && !cursor.path_replaced {
            cursor
                .pending
                .extend(updates.iter().filter_map(|update| match update {
                    ViewUpdate::Entry { entry } => Some(entry.node()),
                    _ => None,
                }));
        }
        cursor.initial = first;
        cursor.reload = true;
        cursor.path_replaced |= replaced;
    }
    pub(crate) fn sent(&self, cursor: &Mutex<PushState>, text: &TextUpdate) {
        cursor.lock().expect("push state lock").sent = Some((
            text.node,
            text.attempt,
            text.offset
                + text
                    .segments
                    .iter()
                    .map(|segment| match segment {
                        TextSegment::Text { text } | TextSegment::Reasoning { text } => text.len(),
                    })
                    .sum::<usize>(),
        ));
    }
    pub(crate) fn release(&self) {
        let mut state = self.state.lock().expect("observation lock");
        state.attempt = None;
        let nodes: Vec<_> = state.current.into_iter().collect();
        Self::mark(&mut state, &nodes, false);
        self.changed.send_replace(());
    }
    /// 模型回调直接累积文字；持久推进的通知总在事务提交之后到达。
    pub(crate) fn emit(&self, event: TurnEvent) {
        let mut state = self.state.lock().expect("observation lock");
        let mut nodes = Vec::new();
        match &event {
            TurnEvent::TurnStarted { user_node, .. } => {
                state.user_node = Some(*user_node);
                state.current = Some(NodeId(user_node.0 + 1));
                nodes.extend([*user_node, NodeId(user_node.0 + 1)]);
            }
            TurnEvent::ModelStarted { key, .. } => {
                if let Some(user) = state.user_node {
                    state.current = Some(NodeId(user.0 + 1 + u64::from(key.model_step_ordinal)));
                }
                state.attempt = Some(AttemptSegments {
                    key: *key,
                    segments: vec![],
                });
                nodes.extend(state.current);
            }
            TurnEvent::TokenDelta { key, content } | TurnEvent::ReasoningDelta { key, content } => {
                let Some(attempt) = state.attempt.as_mut().filter(|a| a.key == *key) else {
                    return;
                };
                match (
                    attempt.segments.last_mut(),
                    matches!(event, TurnEvent::TokenDelta { .. }),
                ) {
                    (Some(TextSegment::Text { text }), true)
                    | (Some(TextSegment::Reasoning { text }), false) => text.push_str(content),
                    (_, true) => attempt.segments.push(TextSegment::Text {
                        text: content.clone(),
                    }),
                    (_, false) => attempt.segments.push(TextSegment::Reasoning {
                        text: content.clone(),
                    }),
                }
            }
            TurnEvent::LlmRetry { .. } | TurnEvent::CallsRegistered { .. } => {
                state.attempt = None;
                nodes.extend(state.current);
            }
            TurnEvent::StepClosed { step } => {
                state.attempt = None;
                nodes.push(step.node);
                if matches!(
                    step.progress,
                    lattice_session::store::StepProgress::Closed {
                        end: lattice_session::store::StepEnd::Continue
                    }
                ) {
                    state.current = Some(NodeId(step.node.0 + 1));
                    nodes.extend(state.current);
                }
            }
            TurnEvent::TurnCompleted { step, .. }
            | TurnEvent::TurnFailed { step, .. }
            | TurnEvent::TurnCancelled { step, .. } => {
                nodes.push(step.node);
                state.attempt = None;
            }
            TurnEvent::ModelFinished { .. }
            | TurnEvent::ToolCallStarted { .. }
            | TurnEvent::ToolCallFinished { .. } => nodes.extend(state.current),
            TurnEvent::CepCallStarted { .. } | TurnEvent::CepCallFinished { .. } => {}
        }
        Self::mark(&mut state, &nodes, false);
        #[cfg(test)]
        state.events.push(event);
        self.changed.send_replace(());
    }
    /// 锁内只复制尚未发送的文字，再开启读事务，避免阻塞模型回调。
    pub(crate) fn capture(
        &self,
        store: &SessionStore,
        session_id: lancet_protocol::SessionId,
        cursor: &Mutex<PushState>,
    ) -> lattice_session::store::Result<Vec<ViewUpdate>> {
        let (initial, reload, replaced, mut pending, attempt, head_changed) = {
            let state = self.state.lock().expect("observation lock");
            let mut cursor = cursor.lock().expect("push state lock");
            let initial = std::mem::take(&mut cursor.initial);
            let replaced = std::mem::take(&mut cursor.path_replaced) && !initial;
            let attempt = state
                .attempt
                .as_ref()
                .zip(state.current)
                .map(|(attempt, node)| {
                    let offset = cursor
                        .sent
                        .filter(|(sent_node, sent_attempt, _)| {
                            *sent_node == node && *sent_attempt == attempt.key.attempt_ordinal
                        })
                        .map_or(0, |(_, _, offset)| offset);
                    (
                        attempt.key,
                        node,
                        offset,
                        suffix(&attempt.segments, offset).0,
                    )
                });
            (
                initial,
                std::mem::take(&mut cursor.reload),
                replaced,
                std::mem::take(&mut cursor.pending),
                attempt,
                std::mem::take(&mut cursor.head_changed),
            )
        };
        let read = store.begin_read()?;
        let mut updates = Vec::new();
        if initial || reload || replaced || !pending.is_empty() || head_changed {
            let (head, entries) = if initial || reload || replaced {
                let mut view = read.view(session_id, WATCH_TURNS)?;
                if reload && !initial && !replaced {
                    for node in pending {
                        if !view.entries.iter().any(|entry| entry.node() == node) {
                            view.entries.push(read.entry(NodeRef { session_id, node })?);
                        }
                    }
                    view.entries.sort_by_key(|entry| entry.node());
                }
                let turn_id = view
                    .entries
                    .iter()
                    .find(|entry| entry.node() == view.session.current)
                    .and_then(|entry| content(entry).turn_id());
                (
                    SessionHead {
                        session_id,
                        position: view.session.current,
                        agent: view.current_agent,
                        turn_id,
                        owner: false,
                        diagnostic: None,
                        path_replaced: replaced,
                    },
                    view.entries,
                )
            } else {
                let record = session_record(&read, session_id)?;
                // 只要推 head，就从同一事务补上当前位置，供读者确认这一轮是否结束。
                if record.current != NodeId(0) {
                    pending.insert(record.current);
                }
                let entries = pending
                    .into_iter()
                    .map(|node| read.entry(NodeRef { session_id, node }))
                    .collect::<lattice_session::store::Result<Vec<_>>>()?;
                let current = entries
                    .iter()
                    .find(|entry| entry.node() == record.current)
                    .map_or(&Node::Root, content);
                (
                    head_from_node(&read, session_id, record.current, current)?,
                    entries,
                )
            };
            updates.push(ViewUpdate::Head(head));
            updates.extend(entries.into_iter().map(|entry| ViewUpdate::Entry {
                entry: Box::new(entry),
            }));
        }
        if let Some((key, node, offset, segments)) = attempt
            && !segments.is_empty()
        {
            let captured = updates.iter().find_map(|update| match update {
                ViewUpdate::Entry { entry } if entry.node() == node => Some(content(entry)),
                _ => None,
            });
            let current;
            let value = if let Some(value) = captured {
                value
            } else {
                current = read.path_node(session_id, node)?;
                &current
            };
            // 事务已提交而标记未到时，已 Ended 的尝试也不能重新发出半截文字。
            if matches!(value, Node::Step { turn_id, step_ordinal, attempts, .. }
                if *turn_id == key.turn_id && *step_ordinal == key.model_step_ordinal
                && attempts.get(key.attempt_ordinal as usize).is_some_and(|a| matches!(a.progress, AttemptProgress::Sent)))
            {
                updates.push(ViewUpdate::Text(TextUpdate {
                    node,
                    attempt: key.attempt_ordinal,
                    offset,
                    segments,
                }));
            }
        }
        Ok(updates)
    }
    #[cfg(test)]
    pub(crate) fn events(&self) -> Vec<TurnEvent> {
        self.state.lock().unwrap().events.clone()
    }
}
fn content(entry: &ConversationEntry) -> &Node {
    match entry {
        ConversationEntry::UserMessage { content, .. }
        | ConversationEntry::Step { content, .. } => content,
    }
}
fn session_record(
    read: &SessionRead,
    session_id: lancet_protocol::SessionId,
) -> lattice_session::store::Result<lattice_session::store::SessionRecord> {
    read.find_session(session_id)?
        .ok_or_else(|| StoreError::NotFound {
            kind: "session",
            id: session_id.to_string(),
        })
}
pub(crate) fn session_head(
    read: &SessionRead,
    session_id: lancet_protocol::SessionId,
) -> lattice_session::store::Result<SessionHead> {
    let record = session_record(read, session_id)?;
    let current = read.path_node(session_id, record.current)?;
    head_from_node(read, session_id, record.current, &current)
}
fn head_from_node(
    read: &SessionRead,
    session_id: lancet_protocol::SessionId,
    position: NodeId,
    current: &Node,
) -> lattice_session::store::Result<SessionHead> {
    let agent = match current {
        Node::Root => None,
        Node::UserMessage { agent_name, .. } => Some(agent_name.clone()),
        Node::Step { turn_id, .. } => {
            let node = read.turn_user_node(session_id, *turn_id)?;
            match read.path_node(session_id, node)? {
                Node::UserMessage {
                    agent_name,
                    turn_id: id,
                    ..
                } if id == *turn_id => Some(agent_name),
                _ => {
                    return Err(StoreError::Corruption {
                        table: "nodes".into(),
                        key: node.0.to_string(),
                        detail: "expected turn user message".into(),
                    });
                }
            }
        }
    };
    Ok(SessionHead {
        session_id,
        position,
        agent,
        turn_id: current.turn_id(),
        owner: false,
        diagnostic: None,
        path_replaced: false,
    })
}
fn suffix(segments: &[TextSegment], offset: usize) -> (Vec<TextSegment>, usize) {
    let mut total = 0;
    let mut result = Vec::new();
    for segment in segments {
        let (text, reasoning) = match segment {
            TextSegment::Text { text } => (text, false),
            TextSegment::Reasoning { text } => (text, true),
        };
        let start = offset.saturating_sub(total).min(text.len());
        total += text.len();
        if start < text.len() {
            let text = text[start..].to_owned();
            result.push(if reasoning {
                TextSegment::Reasoning { text }
            } else {
                TextSegment::Text { text }
            });
        }
    }
    (result, total)
}

#[cfg(test)]
#[path = "observer_tests.rs"]
mod tests;
