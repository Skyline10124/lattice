// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 观察者只保存待读取节点与已发位置；正文只有当前尝试的一份累积。
use super::types::{AttemptSegments, TextSegment, TurnEvent};
use crate::local::{SessionHead, TextUpdate, ViewUpdate, WATCH_TURNS};
use lattice_session::store::{AttemptProgress, Node, NodeId, NodeRef, SessionStore};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex, Weak},
};

#[derive(Default)]
pub(crate) struct PushState {
    initial: bool,
    reload: bool,
    refresh_position: bool,
    path_replaced: bool,
    pending: BTreeSet<NodeId>,
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
}
impl Default for ObserverSink {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changed: tokio::sync::watch::channel(()).0,
            #[cfg(test)]
            accepted_read_failure: Mutex::default(),
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
        self.state
            .lock()
            .expect("observation lock")
            .owner_generation += 1;
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
        // 接受后的读取可能先于 TurnStarted 失败，退场必须按库中的当前位置补推。
        state.watchers.retain(|watcher| {
            watcher.upgrade().is_some_and(|watcher| {
                watcher.lock().expect("push state lock").refresh_position = true;
                true
            })
        });
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
    /// 复制段列表与位置后才开读事务；事务可能比标记多看到一次提交，不能反向拼回旧值。
    pub(crate) fn capture(
        &self,
        store: &SessionStore,
        session_id: lancet_protocol::SessionId,
        cursor: &Mutex<PushState>,
    ) -> lattice_session::store::Result<Vec<ViewUpdate>> {
        let (initial, reload, replaced, refresh_position, mut pending, attempt, sent) = {
            let state = self.state.lock().expect("observation lock");
            let mut cursor = cursor.lock().expect("push state lock");
            let initial = std::mem::take(&mut cursor.initial);
            let replaced = std::mem::take(&mut cursor.path_replaced) && !initial;
            (
                initial,
                std::mem::take(&mut cursor.reload),
                replaced,
                std::mem::take(&mut cursor.refresh_position),
                std::mem::take(&mut cursor.pending),
                state.attempt.clone(),
                cursor.sent,
            )
        };
        let read = store.begin_read()?;
        let (record, agent, entries) = if initial || reload || replaced {
            let mut view = read.view(session_id, WATCH_TURNS)?;
            if reload && !initial && !replaced {
                for node in pending {
                    if !view.entries.iter().any(|entry| entry.node() == node) {
                        view.entries.push(read.entry(NodeRef { session_id, node })?);
                    }
                }
                view.entries.sort_by_key(|entry| entry.node());
            }
            (view.session, view.current_agent, view.entries)
        } else {
            let record = read.session(session_id)?;
            if refresh_position && record.current != NodeId(0) {
                pending.insert(record.current);
            }
            let agent = read.current_agent(session_id)?;
            let entries = pending
                .into_iter()
                .map(|node| read.entry(NodeRef { session_id, node }))
                .collect::<lattice_session::store::Result<Vec<_>>>()?;
            (record, agent, entries)
        };
        let current = read.node(NodeRef {
            session_id,
            node: record.current,
        })?;
        let mut updates = vec![ViewUpdate::Head(SessionHead {
            session_id,
            position: record.current,
            agent,
            turn_id: current.turn_id(),
            owner: false,
            diagnostic: None,
            path_replaced: replaced,
        })];
        updates.extend(entries.into_iter().map(|entry| ViewUpdate::Entry {
            entry: Box::new(entry),
        }));
        if let Some(attempt) = attempt {
            let Some(user) = read.turn_node(attempt.key.turn_id)? else {
                return Ok(updates);
            };
            let node = NodeId(user.node.0 + 1 + u64::from(attempt.key.model_step_ordinal));
            let content = read.node(NodeRef { session_id, node })?;
            // 事务已提交而标记未到时，已 Ended 的尝试也不能重新发出半截文字。
            if matches!(content, Node::Step { ref attempts, .. } if attempts.get(attempt.key.attempt_ordinal as usize).is_some_and(|a| matches!(a.progress, AttemptProgress::Sent)))
            {
                let offset = sent
                    .filter(|(sent_node, sent_attempt, _)| {
                        *sent_node == node && *sent_attempt == attempt.key.attempt_ordinal
                    })
                    .map_or(0, |(_, _, offset)| offset);
                let (segments, _) = suffix(&attempt.segments, offset);
                if !segments.is_empty() {
                    updates.push(ViewUpdate::Text(TextUpdate {
                        node,
                        attempt: attempt.key.attempt_ordinal,
                        offset,
                        segments,
                    }));
                }
            }
        }
        Ok(updates)
    }
    #[cfg(test)]
    pub(crate) fn events(&self) -> Vec<TurnEvent> {
        self.state.lock().unwrap().events.clone()
    }
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
