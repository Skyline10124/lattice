// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral conversation history container (ADR 0005 §7).
//!
//! A data container only: ordered bound messages plus the per-attempt
//! records that produced them. The composition layer reads/restores the
//! SessionStore and hands a plain `ConversationHistory` to the Agent
//! (grill R10); this type owns no persistence, locking, or I/O.

use serde::{Deserialize, Serialize};

use crate::attempt::ModelAttemptRecord;
use crate::message::NativeMessage;
use crate::tool_call::BoundToolCall;

/// Ordered conversation history: completed (fully bound) messages and the
/// per-attempt records behind them.
///
/// Completed conversation only accepts `NativeMessage<BoundToolCall>`; an
/// unbound message can exist only transiently in a pending binding stage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConversationHistory {
    /// Ordered conversation messages in durable order.
    pub messages: Vec<NativeMessage<BoundToolCall>>,
    /// Per-attempt records, keyed internally by their `ModelAttemptKey`.
    /// Retry attempts appear as separate entries.
    pub attempts: Vec<ModelAttemptRecord>,
}

impl ConversationHistory {
    /// An empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one completed message.
    pub fn push_message(&mut self, message: NativeMessage<BoundToolCall>) {
        self.messages.push(message);
    }

    /// Record one provider attempt.
    pub fn push_attempt(&mut self, attempt: ModelAttemptRecord) {
        self.attempts.push(attempt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{MessagePart, MessageRole, MessageSemantic};

    #[test]
    fn history_holds_bound_messages_and_attempts() {
        let mut history = ConversationHistory::new();
        let message: NativeMessage<BoundToolCall> = NativeMessage::new(
            MessageRole::Assistant,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: "done".to_string(),
            })],
        );
        history.push_message(message.clone());
        assert_eq!(history.messages.len(), 1);
        let json = serde_json::to_string(&history).expect("serialize");
        let back: ConversationHistory = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(history, back);
    }
}
