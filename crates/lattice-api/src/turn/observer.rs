// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Owner 的无损内存切点：先同步更新段、窗口与序号，再发送可合并唤醒。

use super::types::{AttemptSegments, SequencedTurnEvent, TextSegment, TurnEvent};
use lattice_session::store::{AttemptProgress, DurableSnapshot, Node, SessionStore};
use std::{collections::VecDeque, sync::Mutex};

#[derive(Default)]
struct ObservationState {
    seq: u64,
    events: VecDeque<SequencedTurnEvent>,
    attempt: Option<AttemptSegments>,
    terminal: bool,
}

pub(crate) struct ObserverSink {
    state: Mutex<ObservationState>,
    changed: tokio::sync::watch::Sender<u64>,
}
impl Default for ObserverSink {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            changed: tokio::sync::watch::channel(0).0,
        }
    }
}
impl ObserverSink {
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// 模型回调直接进入此处；没有可能在保存段之前丢失 delta 的通道。
    pub(crate) fn emit(&self, event: TurnEvent) {
        let mut state = self.state.lock().expect("observation lock poisoned");
        if state.terminal {
            return;
        }
        match &event {
            TurnEvent::ModelStarted { key, .. } => {
                state.attempt = Some(AttemptSegments {
                    key: *key,
                    segments: Vec::new(),
                });
            }
            TurnEvent::TokenDelta { key, content } | TurnEvent::ReasoningDelta { key, content } => {
                let Some(attempt) = state.attempt.as_mut().filter(|a| a.key == *key) else {
                    return;
                };
                let text = matches!(&event, TurnEvent::TokenDelta { .. });
                match (attempt.segments.last_mut(), text) {
                    (Some(TextSegment::Text { text }), true)
                    | (Some(TextSegment::Reasoning { text }), false) => text.push_str(content),
                    _ => attempt.segments.push(if text {
                        TextSegment::Text {
                            text: content.clone(),
                        }
                    } else {
                        TextSegment::Reasoning {
                            text: content.clone(),
                        }
                    }),
                }
            }
            TurnEvent::LlmRetry { key, .. } => {
                if state.attempt.as_ref().is_some_and(|a| a.key == *key) {
                    state.attempt = None;
                }
            }
            TurnEvent::StepClosed { .. } => state.attempt = None,
            TurnEvent::TurnCompleted { .. }
            | TurnEvent::TurnFailed { .. }
            | TurnEvent::TurnCancelled { .. } => {
                state.attempt = None;
                state.terminal = true;
            }
            TurnEvent::TurnStarted { .. }
            | TurnEvent::ModelFinished { .. }
            | TurnEvent::CallsRegistered { .. }
            | TurnEvent::ToolCallStarted { .. }
            | TurnEvent::ToolCallFinished { .. }
            | TurnEvent::CepCallStarted { .. }
            | TurnEvent::CepCallFinished { .. } => {}
        }
        state.seq = state
            .seq
            .checked_add(1)
            .expect("observation sequence exhausted");
        let sequence = state.seq;
        state
            .events
            .push_back(SequencedTurnEvent { sequence, event });
        if state.events.len() > 64 {
            state.events.pop_front();
        }
        // 通知在同锁更新之后发布，watch 合并不会丢掉内存权威。
        self.changed.send_replace(sequence);
    }

    /// None 表示必须重新快照；空 Vec 表示已赶上切点，而非丢失事件。
    pub(crate) fn events_after(&self, sequence: u64) -> Option<Vec<SequencedTurnEvent>> {
        let state = self.state.lock().expect("observation lock poisoned");
        if sequence > state.seq
            || state
                .events
                .front()
                .is_some_and(|e| sequence.saturating_add(1) < e.sequence)
        {
            return None;
        }
        Some(
            state
                .events
                .iter()
                .filter(|e| e.sequence > sequence)
                .cloned()
                .collect(),
        )
    }

    /// 调用方在阻塞边界内持当前代租约；同锁 begin_read，锁外解码，事务不返回 async 层。
    pub(crate) fn snapshot(
        &self,
        store: &SessionStore,
        session: lancet_protocol::SessionId,
        turn: Option<lancet_protocol::TurnId>,
    ) -> lattice_session::store::Result<(DurableSnapshot, Option<AttemptSegments>, u64)> {
        let (read, mut attempt, seq) = {
            let state = self.state.lock().expect("observation lock poisoned");
            (store.begin_read()?, state.attempt.clone(), state.seq)
        };
        let snapshot = read.snapshot(session, turn)?;
        if !snapshot.unclosed_step.as_ref().is_some_and(|(_, node)| {
            let Node::Step {
                turn_id,
                step_ordinal,
                attempts,
                ..
            } = node
            else {
                return false;
            };
            attempt.as_ref().is_some_and(|a| {
                a.key.turn_id == *turn_id
                    && a.key.model_step_ordinal == *step_ordinal
                    && attempts.iter().any(|stored| {
                        stored.attempt == a.key.attempt_ordinal
                            && matches!(stored.progress, AttemptProgress::Sent)
                    })
            })
        }) {
            attempt = None;
        }
        Ok((snapshot, attempt, seq))
    }
}

#[cfg(test)]
#[path = "observer_tests.rs"]
mod redb_tests;
