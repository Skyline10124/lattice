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
    pending_calls: HashSet<lancet_protocol::InvocationId>,
    buffered: VecDeque<TurnEvent>,
}

impl CepOrderedEmitter {
    pub(super) async fn emit(&mut self, observer: &mut ObserverSink, event: TurnEvent) {
        match event {
            TurnEvent::CepCallStarted { invocation_id, .. } => {
                self.pending_calls.insert(invocation_id);
                observer.emit(event).await;
            }
            TurnEvent::CepCallFinished { invocation_id, .. } => {
                observer.emit(event).await;
                self.pending_calls.remove(&invocation_id);
                if self.pending_calls.is_empty() {
                    self.flush_buffered(observer).await;
                }
            }
            event if is_cep_event(&event) => {
                observer.emit(event).await;
                if self.pending_calls.is_empty() {
                    self.flush_buffered(observer).await;
                }
            }
            event if self.pending_calls.is_empty() && !is_tool_call_finished(&event) => {
                self.flush_buffered(observer).await;
                observer.emit(event).await;
            }
            event => {
                self.buffered.push_back(event);
            }
        }
    }

    pub(super) async fn finish(&mut self, observer: &mut ObserverSink) {
        self.pending_calls.clear();
        self.flush_buffered(observer).await;
    }

    async fn flush_buffered(&mut self, observer: &mut ObserverSink) {
        while let Some(event) = self.buffered.pop_front() {
            observer.emit(event).await;
        }
    }
}

fn is_tool_call_finished(event: &TurnEvent) -> bool {
    matches!(event, TurnEvent::ToolCallFinished { .. })
}

fn is_cep_event(event: &TurnEvent) -> bool {
    matches!(
        event,
        TurnEvent::CepCallStarted { .. } | TurnEvent::CepCallFinished { .. }
    )
}
