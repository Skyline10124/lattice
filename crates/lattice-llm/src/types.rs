// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! These types define the direct wire contract for LATTICE LLM calls
//! and are independent of `lattice-core`.

use lattice_conversation::{ConversationItem, NativeMessage, TokenUsage};
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
    pub messages: Vec<ConversationItem>,
    pub tools: Vec<ToolDefinition>,
    pub target: ModelTarget,
    pub temperature: Option<f64>,
    pub max_tokens: Option<u32>,
    /// OpenAI-specific: max completion tokens (replaces max_tokens for o-series)
    pub max_completion_tokens: Option<u32>,
}

impl ChatRequest {
    pub fn new(messages: impl Into<Vec<ConversationItem>>, target: ModelTarget) -> Self {
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

/// provider 的错误分类独立于已收到的 delta 数量。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderFailure {
    pub message: String,
    pub retryable: bool,
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
        failure: ProviderFailure,
    },
    OutputLimit {
        interruption: lattice_conversation::AttemptInterruption,
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

impl LlmEvent {
    pub(crate) fn error(message: impl Into<String>, retryable: bool) -> Self {
        Self::Error {
            failure: ProviderFailure {
                message: message.into(),
                retryable,
            },
        }
    }
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
