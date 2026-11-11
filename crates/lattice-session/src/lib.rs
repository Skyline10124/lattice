// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 全局事务会话库：Session、会话树、Step/Turn、调用槽位与只读投影。

pub use lattice_store::StateLayout;

pub mod model;
pub mod store;

pub use model::{
    AgentName, AgentNameError, MAX_AGENT_NAME_BYTES, MAX_MODEL_ATTEMPTS, MAX_RETRY_DELAY_MS,
    MAX_TURN_TIMEOUT_SECS, ProjectPath, ProjectPathError, RetryPolicy, RetryPolicyError,
    TurnTimeout, TurnTimeoutError,
};

/// Shared typed transcript helpers for tests.
#[doc(hidden)]
#[cfg(any(test, feature = "test-utils"))]
pub mod test_helpers {
    use lattice_conversation::{
        BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    };

    fn text_message(role: MessageRole, text: &str) -> NativeMessage<BoundToolCall> {
        NativeMessage::new(
            role,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: text.to_string(),
            })],
        )
    }

    pub fn user_msg(text: &str) -> NativeMessage<BoundToolCall> {
        text_message(MessageRole::User, text)
    }

    pub fn assistant_msg(text: &str) -> NativeMessage<BoundToolCall> {
        text_message(MessageRole::Assistant, text)
    }

    pub fn text(message: &NativeMessage<BoundToolCall>) -> Option<&str> {
        message.parts.iter().find_map(|part| match &part.semantic {
            MessageSemantic::Text { text } => Some(text.as_str()),
            _ => None,
        })
    }
}
