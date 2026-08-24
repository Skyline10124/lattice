// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public graph DTOs plus the typed durable branch transcript.
//!
//! `GraphMessage`/`GraphToolCall` remain presentation DTOs. Session persistence
//! uses [`NativeMessage<BoundToolCall>`] so canonical invocation identity, structural
//! address, provider replay state, and ToolResult closure survive reopen.

use serde::{Deserialize, Serialize};

use crate::{BoundToolCall, NativeMessage};
/// A single transcript message: user, assistant, or tool role.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphMessage {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<GraphToolCall>>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// A tool call request in an assistant message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Complete ordered typed transcript for the current branch HEAD.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeadTranscript {
    pub session_id: String,
    pub parent_commit_id: Option<String>,
    pub messages: Vec<NativeMessage<BoundToolCall>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_message_serde_shape_is_unchanged() {
        // Byte-for-byte the pre-migration shape: field names and option
        // handling must not drift, because existing session commits embed
        // this payload.
        let message = GraphMessage {
            role: "assistant".to_string(),
            content: "calling".to_string(),
            reasoning_content: None,
            tool_calls: Some(vec![GraphToolCall {
                id: "t1".to_string(),
                name: "read_file".to_string(),
                arguments: r#"{"path":"a.txt"}"#.to_string(),
            }]),
            tool_call_id: None,
            name: None,
        };
        let json = serde_json::to_string(&message).expect("serialize");
        assert_eq!(
            json,
            r#"{"role":"assistant","content":"calling","reasoning_content":null,"tool_calls":[{"id":"t1","name":"read_file","arguments":"{\"path\":\"a.txt\"}"}],"tool_call_id":null,"name":null}"#
        );
        let back: GraphMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(message, back);
    }
}
