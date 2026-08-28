// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Call-scoped cancellation, events, and execution receipts.

use lancet_protocol::{CellEvidenceTriple, CepError, InvocationId, ToolEvent};
use serde_json::Value;
use tokio::sync::{mpsc, watch};

/// Cancellation signal for one Component call.
#[derive(Clone, Debug)]
pub struct Cancellation {
    rx: watch::Receiver<bool>,
}

impl Cancellation {
    /// Create a cancellation handle from a watch receiver.
    pub fn new(rx: watch::Receiver<bool>) -> Self {
        Self { rx }
    }

    /// Returns whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Wait until cancellation is requested.
    pub async fn cancelled(&mut self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            if self.rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Sender paired with a call-scoped [`Cancellation`].
#[derive(Clone, Debug)]
pub struct CancellationSender {
    tx: watch::Sender<bool>,
}

impl CancellationSender {
    /// Create an independent sender/receiver pair.
    pub fn pair() -> (Self, Cancellation) {
        let (tx, rx) = watch::channel(false);
        (Self { tx }, Cancellation::new(rx))
    }

    /// Request cancellation. Repeated calls are idempotent.
    pub fn cancel(&self) {
        let _ = self.tx.send(true);
    }
}

/// Non-blocking sink for exact, ordered Tool events emitted during one call.
#[derive(Clone, Debug)]
pub struct EventSink {
    tx: mpsc::UnboundedSender<ToolEvent>,
    invocation_id: InvocationId,
}

impl EventSink {
    /// Bind an event sink to the exact invocation identity.
    pub fn new(tx: mpsc::UnboundedSender<ToolEvent>, invocation_id: InvocationId) -> Self {
        Self { tx, invocation_id }
    }

    /// Emit an event, overwriting any caller-supplied invocation identity.
    pub fn emit(&self, method: impl Into<String>, mut params: Value) {
        let method = method.into();
        bind_invocation_id(&mut params, self.invocation_id);
        let _ = self.tx.send(ToolEvent::new(method, params));
    }
}

fn bind_invocation_id(params: &mut Value, invocation_id: InvocationId) {
    if let Some(object) = params.as_object_mut() {
        object.insert(
            "invocation_id".to_string(),
            Value::from(invocation_id.to_string()),
        );
    } else {
        *params = serde_json::json!({
            "invocation_id": invocation_id,
            "payload": params.take(),
        });
    }
}

/// Result and content-free Cell evidence for one known Component outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReceipt {
    pub result: Result<Value, CepError>,
    pub evidence: CellEvidenceTriple,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation_id() -> InvocationId {
        InvocationId::parse("10000000-0000-4000-8000-000000000001").unwrap()
    }

    #[tokio::test]
    async fn event_sink_overwrites_spoofed_invocation_identity() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let sink = EventSink::new(tx, invocation_id());
        sink.emit(
            "tool.event",
            serde_json::json!({"invocation_id": "spoofed", "value": 1}),
        );
        let event = rx.recv().await.unwrap();
        assert_eq!(
            event.params["invocation_id"],
            serde_json::json!(invocation_id().to_string())
        );
    }
}
