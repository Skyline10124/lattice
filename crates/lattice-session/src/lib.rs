// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 全局事务会话库：Session、会话树、Step/Turn、调用槽位与只读投影。

pub use lattice_store::{ProjectStoreKey, StateLayout};

pub mod store;
#[cfg(any(test, feature = "test-support"))]
mod test_support;
#[cfg(any(test, feature = "test-support"))]
pub use test_support::ScopedXdgStateHome;

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
