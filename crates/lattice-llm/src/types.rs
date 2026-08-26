// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! These types define the direct wire contract for LATTICE LLM calls
//! and are independent of `lattice-core`.

use lattice_conversation::{BoundToolCall, NativeMessage, TokenUsage};
use serde::{Deserialize, Serialize};

/// A tool definition providing a function specification to the model.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// JSON schema for the tool parameters.
    pub parameters: serde_json::Value,
}

impl ToolDefinition {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// A chat completion request to be sent to an LLM.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    /// Durable provider-neutral history. Provider wire is projected exactly once
    /// by the selected provider builder.
    pub messages: Vec<NativeMessage<BoundToolCall>>,
    pub tools: Vec<ToolDefinition>,
    pub target: ModelTarget,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    /// OpenAI-specific: max completion tokens (replaces max_tokens for o-series)
    pub max_completion_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(
        messages: impl Into<Vec<NativeMessage<BoundToolCall>>>,
        target: ModelTarget,
    ) -> Self {
        ChatRequest {
            messages: messages.into(),
            tools: Vec::new(),
            target,
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
        }
    }
}

/// A streaming LLM event for the API.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum LlmEvent {
    Started,
    Token {
        content: String,
    },
    Reasoning {
        content: String,
    },
    ToolCallStart {
        /// Provider message-part/tool-call ordinal used only for stream assembly.
        index: usize,
        /// Provider-native correlation ID; never synthesized.
        provider_call_id: Option<String>,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        arguments_delta: String,
    },
    ToolCallEnd {
        index: usize,
    },
    /// Authoritative complete provider message projected from official wire.
    MessageComplete {
        message: NativeMessage<lattice_conversation::UnboundToolCall>,
    },
    /// Provider completed a whole message whose Tool batch failed validation.
    MessageRejected {
        rejection: lattice_conversation::AttemptRejection,
    },
    Done {
        finish_reason: String,
        usage: Option<TokenUsage>,
    },
    Error {
        message: String,
    },
    Citation {
        index: usize,
        cite_text: String,
    },
    /// Part-local opaque provider state observed while streaming.
    Signature {
        index: usize,
        signature: String,
    },
    GroundingChunk {
        text: String,
        url: Option<String>,
    },
    SafetyRating {
        category: String,
        probability: String,
    },
}

/// A model target specifying which model alias to route to.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelTarget {
    pub model: String,
    pub provider: Option<String>,
}

impl Default for ModelTarget {
    fn default() -> Self {
        ModelTarget {
            model: "sonnet".to_string(),
            provider: None,
        }
    }
}

impl ModelTarget {
    pub fn new(model: impl Into<String>) -> Self {
        ModelTarget {
            model: model.into(),
            provider: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use lattice_conversation::{
        BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    };

    use super::*;

    fn user(text: &str) -> NativeMessage<BoundToolCall> {
        NativeMessage::new(
            MessageRole::User,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: text.to_string(),
            })],
        )
    }

    #[test]
    fn chat_request_round_trips_native_history() {
        let request = ChatRequest {
            messages: vec![user("hello")],
            tools: vec![ToolDefinition::new(
                "get_weather",
                "Get weather",
                serde_json::json!({"type": "object"}),
            )],
            target: ModelTarget::default(),
            temperature: Some(0.7),
            max_tokens: None,
            max_completion_tokens: None,
        };
        let json = serde_json::to_string(&request).unwrap();
        let decoded: ChatRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, decoded);
    }

    #[test]
    fn events_round_trip() {
        for event in [
            LlmEvent::Started,
            LlmEvent::ToolCallStart {
                index: 0,
                provider_call_id: None,
                name: "observe.workspace".to_string(),
            },
            LlmEvent::Done {
                finish_reason: "stop".to_string(),
                usage: None,
            },
        ] {
            let encoded = serde_json::to_string(&event).unwrap();
            assert_eq!(event, serde_json::from_str::<LlmEvent>(&encoded).unwrap());
        }
    }

    #[test]
    fn model_target_default_is_sonnet() {
        let target = ModelTarget::default();
        assert_eq!(target.model, "sonnet");
        assert!(target.provider.is_none());
    }
}
