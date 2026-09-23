// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 尽力投递的观察通道；序号在第一个可能丢失的位置之前分配。

use std::collections::{HashSet, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use tokio::sync::mpsc;

use super::types::{SequencedTurnEvent, TurnEvent};

pub(super) const OBSERVER_SEND_TIMEOUT: Duration = Duration::from_millis(500);

fn is_streaming_delta(event: &TurnEvent) -> bool {
    matches!(
        event,
        TurnEvent::TokenDelta { .. } | TurnEvent::ReasoningDelta { .. }
    )
}

enum Sender {
    #[cfg(test)]
    Legacy(mpsc::Sender<TurnEvent>),
    Sequenced(mpsc::Sender<SequencedTurnEvent>),
}

pub(super) struct ObserverSink {
    sender: Sender,
    sequence: Arc<AtomicU64>,
    gave_up: bool,
}

impl ObserverSink {
    #[cfg(test)]
    pub(super) fn new(sender: mpsc::Sender<TurnEvent>) -> Self {
        Self {
            sender: Sender::Legacy(sender),
            sequence: Arc::default(),
            gave_up: false,
        }
    }

    pub(super) fn sequenced(
        sender: mpsc::Sender<SequencedTurnEvent>,
        sequence: Arc<AtomicU64>,
    ) -> Self {
        Self {
            sender: Sender::Sequenced(sender),
            sequence,
            gave_up: false,
        }
    }

    /// 模型增量在唯一丢弃点之前编号；回调不等待观察者，也不建立额外转发队列。
    pub(super) fn emit_delta(&mut self, event: TurnEvent) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if self.gave_up {
            return;
        }
        match &self.sender {
            #[cfg(test)]
            Sender::Legacy(sender) => {
                let _ = sender.try_send(event);
            }
            Sender::Sequenced(sender) => {
                let _ = sender.try_send(SequencedTurnEvent { sequence, event });
            }
        }
    }

    pub(super) async fn emit(&mut self, event: TurnEvent) {
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if self.gave_up {
            return;
        }
        let delta = is_streaming_delta(&event);
        match &self.sender {
            #[cfg(test)]
            Sender::Legacy(sender) => {
                if delta {
                    let _ = sender.try_send(event);
                } else if !matches!(
                    tokio::time::timeout(OBSERVER_SEND_TIMEOUT, sender.send(event)).await,
                    Ok(Ok(()))
                ) {
                    self.gave_up = true;
                }
            }
            Sender::Sequenced(sender) => {
                let event = SequencedTurnEvent { sequence, event };
                if delta {
                    let _ = sender.try_send(event);
                } else if !matches!(
                    tokio::time::timeout(OBSERVER_SEND_TIMEOUT, sender.send(event)).await,
                    Ok(Ok(()))
                ) {
                    self.gave_up = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sequence_includes_dropped_deltas_and_the_unobserved_tail() {
        let (tx, mut rx) = mpsc::channel(1);
        let count = Arc::new(AtomicU64::new(0));
        let mut sink = ObserverSink::sequenced(tx, count.clone());
        sink.emit_delta(TurnEvent::TokenDelta {
            content: "first".into(),
        });
        sink.emit_delta(TurnEvent::TokenDelta {
            content: "lost".into(),
        });
        assert_eq!(rx.recv().await.unwrap().sequence, 1);
        sink.emit(TurnEvent::TurnCommitting).await;
        assert_eq!(rx.recv().await.unwrap().sequence, 3);
        drop(rx);
        sink.emit(TurnEvent::TurnCommitting).await;
        sink.emit(TurnEvent::TokenDelta {
            content: "tail".into(),
        })
        .await;
        assert_eq!(count.load(Ordering::Relaxed), 5);
    }
}

#[derive(Default)]
pub(super) struct CepOrderedEmitter {
    completed_calls: HashSet<lancet_protocol::InvocationId>,
    buffered: VecDeque<(Option<lancet_protocol::InvocationId>, TurnEvent)>,
}

impl CepOrderedEmitter {
    pub(super) async fn emit(&mut self, observer: &mut ObserverSink, event: TurnEvent) {
        match event {
            TurnEvent::CepCallStarted { .. } => observer.emit(event).await,
            TurnEvent::CepCallFinished { invocation_id, .. } => {
                observer.emit(event).await;
                self.completed_calls.insert(invocation_id);
                self.flush_call(observer, invocation_id).await;
            }
            event @ TurnEvent::ToolCallFinished { .. } => {
                let invocation_id = tool_call_finished_id(&event);
                if invocation_id.is_some_and(|id| self.completed_calls.remove(&id)) {
                    observer.emit(event).await;
                } else {
                    self.buffered.push_back((invocation_id, event));
                }
            }
            event => observer.emit(event).await,
        }
    }

    pub(super) async fn finish(&mut self, observer: &mut ObserverSink) {
        self.completed_calls.clear();
        while let Some((_, event)) = self.buffered.pop_front() {
            observer.emit(event).await;
        }
    }

    async fn flush_call(
        &mut self,
        observer: &mut ObserverSink,
        invocation_id: lancet_protocol::InvocationId,
    ) {
        let mut buffered = std::mem::take(&mut self.buffered);
        let mut emitted = false;
        while let Some((buffered_id, event)) = buffered.pop_front() {
            if buffered_id == Some(invocation_id) {
                observer.emit(event).await;
                emitted = true;
            } else {
                self.buffered.push_back((buffered_id, event));
            }
        }
        if emitted {
            self.completed_calls.remove(&invocation_id);
        }
    }
}

fn tool_call_finished_id(event: &TurnEvent) -> Option<lancet_protocol::InvocationId> {
    let TurnEvent::ToolCallFinished { id, .. } = event else {
        return None;
    };
    lancet_protocol::InvocationId::parse(id).ok()
}
