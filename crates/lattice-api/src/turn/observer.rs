// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Best-effort public turn-event observer sink shared by live + saga.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use tokio::sync::mpsc;

use super::types::TurnEvent;

/// Max wait when forwarding a non-delta [`TurnEvent`] to a slow observer.
pub(super) const OBSERVER_SEND_TIMEOUT: Duration = Duration::from_millis(500);

fn is_streaming_delta(event: &TurnEvent) -> bool {
    matches!(
        event,
        TurnEvent::TokenDelta { .. } | TurnEvent::ReasoningDelta { .. }
    )
}

/// Best-effort public observer sink.
pub(super) struct ObserverSink {
    sender: mpsc::Sender<TurnEvent>,
    gave_up: bool,
}

impl ObserverSink {
    pub(super) fn new(sender: mpsc::Sender<TurnEvent>) -> Self {
        Self {
            sender,
            gave_up: false,
        }
    }

    pub(super) async fn emit(&mut self, event: TurnEvent) {
        if self.gave_up {
            return;
        }
        if is_streaming_delta(&event) {
            let _ = self.sender.try_send(event);
            return;
        }
        let send = self.sender.send(event);
        match tokio::time::timeout(OBSERVER_SEND_TIMEOUT, send).await {
            Ok(Ok(())) => {}
            _ => self.gave_up = true,
        }
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
