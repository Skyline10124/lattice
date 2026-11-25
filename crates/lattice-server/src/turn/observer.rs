// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 观察者只保存待读取节点与已发位置；正文只有当前尝试的一份累积。
use super::types::{AttemptSegments, TextSegment, TurnEvent};
use crate::local::{SessionHead, TextUpdate, ViewUpdate, WATCH_TURNS};
use lattice_session::store::{
    AttemptProgressDetail, ConversationEntry, NodeId, NodeRef, SessionRead, SessionSettings,
    SessionStore, StoreError,
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
    start_node: Option<NodeId>,
    current: Option<NodeId>,
    attempt: Option<AttemptSegments>,
    closed_turn: Option<lattice_conversation::TurnId>,
    watchers: Vec<Weak<Mutex<PushState>>>,
    #[cfg(test)]
    events: Vec<TurnEvent>,
}
#[cfg(any(test, feature = "test-utils"))]
type AcceptedReadFailure = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);
pub(crate) struct ObserverSink {
    state: Mutex<ObservationState>,
    changed: tokio::sync::watch::Sender<()>,
    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) accepted_read_failure: Mutex<Option<AcceptedReadFailure>>,
    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) before_accept: Mutex<Option<AcceptedReadFailure>>,
}
impl Default for ObserverSink {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changed: tokio::sync::watch::channel(()).0,
            #[cfg(any(test, feature = "test-utils"))]
            accepted_read_failure: Mutex::default(),
            #[cfg(any(test, feature = "test-utils"))]
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
    pub(crate) fn has_watchers(&self) -> bool {
        let mut state = self.state.lock().expect("observation lock");
        state.watchers.retain(|watcher| watcher.strong_count() != 0);
        !state.watchers.is_empty()
    }
    /// 返回指定轮次当前模型尝试中的助手正文快照（剔除推理段）；若该轮已关闭则返回 `None`。
    #[cfg_attr(not(any(test, feature = "test-utils")), allow(dead_code))]
    pub(crate) fn assistant_draft(&self, turn_id: lattice_conversation::TurnId) -> Option<String> {
        let state = self.state.lock().expect("观察状态锁不应中毒");
        if state.closed_turn == Some(turn_id) {
            return None;
        }
        let Some(attempt) = state
            .attempt
            .as_ref()
            .filter(|attempt| attempt.key.turn_id == turn_id)
        else {
            return Some(String::new());
        };
        let mut text = String::new();
        for segment in &attempt.segments {
            if let TextSegment::Text { text: part } = segment {
                text.push_str(part);
            }
        }
        Some(text)
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
        state.owner_generation += 1;
        state.start_node = None;
        state.current = (position != NodeId(0)).then_some(position);
        state.attempt = None;
        state.closed_turn = None;
        Self::mark(&mut state, &[], true);
        self.changed.send_replace(());
    }
    /// 记录 `feed` 或 `context_op` 落库后的最新节点，触发全部观察者增量推送。
    pub(crate) fn record_appended_node(&self, node: NodeId) {
        let mut state = self.state.lock().expect("观察状态锁不应中毒");
        state.owner_generation += 1;
        state.current = Some(node);
        Self::mark(&mut state, &[node], false);
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
    #[cfg(test)]
    pub(crate) fn release(&self) {
        self.release_turn(None);
    }
    pub(crate) fn release_turn(&self, turn_id: Option<lattice_conversation::TurnId>) {
        let mut state = self.state.lock().expect("观察状态锁不应中毒");
        if let Some(turn_id) = turn_id {
            state.closed_turn = Some(turn_id);
        }
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
            TurnEvent::TurnStarted {
                input_nodes,
                start_node,
                ..
            } => {
                state.start_node = Some(*start_node);
                state.current = Some(*start_node);
                state.closed_turn = None;
                nodes.extend(input_nodes.iter().copied());
                nodes.push(*start_node);
            }
            TurnEvent::ModelStarted { key, .. } => {
                if let Some(start) = state.start_node {
                    state.current = Some(NodeId(start.0 + u64::from(key.model_step_ordinal)));
                }
                if state.closed_turn == Some(key.turn_id) {
                    state.closed_turn = None;
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
            TurnEvent::TurnCompleted { turn, step, .. }
            | TurnEvent::TurnFailed { turn, step, .. }
            | TurnEvent::TurnCancelled { turn, step, .. } => {
                nodes.push(step.node);
                state.closed_turn = Some(turn.turn_id);
                state.attempt = None;
            }
            TurnEvent::ModelFinished { .. }
            | TurnEvent::ToolCallStarted { .. }
            | TurnEvent::ToolCallFinished { .. } => nodes.extend(state.current),
        }
        Self::mark(&mut state, &nodes, false);
        #[cfg(test)]
        state.events.push(event);
        self.changed.send_replace(());
    }
    #[cfg(test)]
    pub(crate) fn notify_without_marking(&self) {
        self.changed.send_replace(());
    }
    /// 锁内只复制尚未发送的文字，再开启读事务，避免阻塞模型回调。
    pub(crate) fn capture(
        &self,
        store: &SessionStore,
        session_id: lattice_conversation::SessionId,
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
                let turn_id = if view.session.current == NodeId(0) {
                    None
                } else if let Some(entry) = view
                    .entries
                    .iter()
                    .rev()
                    .find(|entry| entry.node() == view.session.current)
                {
                    entry.turn_id()
                } else {
                    read.entry(NodeRef {
                        session_id,
                        node: view.session.current,
                    })?
                    .turn_id()
                };
                let head = build_head(
                    session_id,
                    view.session.current,
                    turn_id,
                    &view.settings,
                    replaced,
                    Some(view.has_more),
                )?;
                (head, view.entries)
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
                let turn_id = if record.current == NodeId(0) {
                    None
                } else {
                    entries
                        .iter()
                        .find(|entry| entry.node() == record.current)
                        .and_then(ConversationEntry::turn_id)
                };
                let settings = read.session_settings(session_id)?;
                let head = build_head(session_id, record.current, turn_id, &settings, false, None)?;
                (head, entries)
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
                ViewUpdate::Entry { entry } if entry.node() == node => Some(entry.as_ref()),
                _ => None,
            });
            let sent = if let Some(entry) = captured {
                matches!(
                    entry,
                    ConversationEntry::Step { step, .. }
                        if step.key.turn_id == key.turn_id
                            && step.key.ordinal == key.model_step_ordinal
                            && step
                                .attempts
                                .get(key.attempt_ordinal as usize)
                                .is_some_and(|a| matches!(a.progress, AttemptProgressDetail::Sent))
                )
            } else {
                read.attempt_is_sent(session_id, node, key)?
            };
            // 事务已提交而标记未到时，已 Ended 的尝试也不能重新发出半截文字。
            if sent {
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
fn session_record(
    read: &SessionRead,
    session_id: lattice_conversation::SessionId,
) -> lattice_session::store::Result<lattice_session::store::SessionRecord> {
    read.find_session(session_id)?
        .ok_or_else(|| StoreError::NotFound {
            kind: "session",
            id: session_id.to_string(),
        })
}
pub(crate) fn session_head(
    read: &SessionRead,
    session_id: lattice_conversation::SessionId,
) -> lattice_session::store::Result<SessionHead> {
    let record = session_record(read, session_id)?;
    let turn_id = if record.current == NodeId(0) {
        None
    } else {
        read.path_node(session_id, record.current)?.turn_id()
    };
    let settings = read.session_settings(session_id)?;
    build_head(session_id, record.current, turn_id, &settings, false, None)
}
fn build_head(
    session_id: lattice_conversation::SessionId,
    position: NodeId,
    turn_id: Option<lattice_conversation::TurnId>,
    settings: &SessionSettings,
    path_replaced: bool,
    has_older: Option<bool>,
) -> lattice_session::store::Result<SessionHead> {
    let settings_version = settings.version()?;
    let (provider, model) = match &settings.model {
        Some(target) => (
            Some(target.provider().clone()),
            Some(target.model().clone()),
        ),
        None => (None, None),
    };
    Ok(SessionHead {
        session_id,
        position,
        agent: settings.profile.agent_name.as_str().to_owned(),
        model,
        provider,
        effort: settings.effort,
        settings_version,
        turn_id,
        owner: false,
        diagnostic: None,
        path_replaced,
        has_older,
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
