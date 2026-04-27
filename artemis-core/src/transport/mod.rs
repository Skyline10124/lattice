#![allow(deprecated)]
//! Provider transport layer — format conversion between internal types
//! and provider-specific API shapes.
//!
//! Each transport implements the [`Transport`] trait, providing:
//! - `normalize_messages`: internal → provider-native messages
//! - `normalize_tools`: internal → provider-native tool definitions
//! - `denormalize_response`: provider-native response → internal
//! - `denormalize_stream_chunk`: provider SSE event → internal [`StreamEvent`]

pub mod anthropic;
pub mod chat_completions;
pub mod dispatcher;
pub mod gemini;
pub mod openai_compat;

use crate::streaming::StreamEvent;
use crate::types::{Message, ToolCall, ToolDefinition};
use serde_json::Value;

/// Result of normalizing internal messages to a provider-specific format.
///
/// For Anthropic, `system` is extracted separately because the API requires
/// it as a top-level parameter, not inline in the messages array.
pub struct NormalizedMessages {
    /// System prompt extracted from System-role messages (None if no system message).
    pub system: Option<String>,
    /// Non-system messages converted to provider-native JSON.
    pub messages: Vec<Value>,
}

/// Internal representation of a normalized provider response.
pub struct NormalizedResponse {
    /// Text content from the response.
    pub content: Option<String>,
    /// Tool calls requested by the model.
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Finish reason (e.g. "stop", "tool_calls", "length").
    pub finish_reason: String,
    /// Reasoning/thinking content (extended thinking models).
    pub reasoning: Option<String>,
}

/// Trait for provider-specific message format conversion.
///
/// Transports are **pure data transformation** — no network I/O.
/// They convert between the internal [`Message`]/[`ToolDefinition`] types
/// and the JSON shapes expected by each provider's API.
pub trait Transport: Send + Sync {
    /// Convert internal messages to provider-native format.
    ///
    /// Returns a [`NormalizedMessages`] with system prompt extracted
    /// separately and remaining messages converted to provider JSON.
    fn normalize_messages(&self, messages: &[Message]) -> NormalizedMessages;

    /// Convert internal tool definitions to provider-native format.
    ///
    /// Returns a list of JSON values, one per tool, in the provider's
    /// expected schema.
    fn normalize_tools(&self, tools: &[ToolDefinition]) -> Vec<Value>;

    /// Convert a provider-native response to internal representation.
    ///
    /// `response` is the parsed JSON body of the provider's response.
    fn denormalize_response(&self, response: &Value) -> NormalizedResponse;

    /// Convert a provider SSE stream chunk to internal [`StreamEvent`].
    ///
    /// `event_type` is the SSE event field (e.g. `"content_block_delta"`).
    /// `data` is the raw data payload (after stripping `data: `).
    fn denormalize_stream_chunk(
        &self,
        event_type: &str,
        data: &Value,
    ) -> Vec<StreamEvent>;
}

pub use chat_completions::{ChatCompletionsTransport, Transport as ChatTransport, TransportError};
pub use dispatcher::TransportDispatcher;
pub use gemini::GeminiTransport;
pub use openai_compat::OpenAICompatTransport;
