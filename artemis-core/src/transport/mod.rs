//! Provider transport layer — format conversion between internal types
//! and provider-specific API shapes.
//!
//! Each transport implements the [`Transport`] trait, providing:
//! - `base_url` / `extra_headers` / `api_mode`: HTTP client configuration
//! - `normalize_request`: internal [`ChatRequest`] → provider-native JSON body
//! - `denormalize_response`: provider-native response → internal [`ChatResponse`]
//! - `normalize_messages`: internal [`Message`] → provider-native messages
//! - `normalize_tools`: internal [`ToolDefinition`] → provider-native tools
//! - `denormalize_stream_chunk`: provider SSE event → internal [`StreamEvent`]

pub mod anthropic;
pub mod chat_completions;
pub mod dispatcher;
pub mod gemini;
pub mod openai_compat;

use std::collections::HashMap;

use crate::provider::{ChatRequest, ChatResponse};
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
///
/// Used for characterisation/testing of format-level transports that need
/// access to reasoning/thinking content before it gets discarded by the
/// unified `denormalize_response`.
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

/// Error type for transport-level operations.
///
/// Re-exported from [`chat_completions`] for backward compatibility.
pub use chat_completions::TransportError;

// ---------------------------------------------------------------------------
// Unified Transport trait
// ---------------------------------------------------------------------------

/// Unified transport trait for provider-specific format conversion.
///
/// Each transport handles one API format (e.g. OpenAI Chat Completions,
/// Anthropic Messages, Gemini generateContent). The transport is responsible for:
///
/// - Converting internal [`ChatRequest`]s into API-specific JSON bodies
/// - Converting API-specific JSON responses into internal [`ChatResponse`]s
/// - Converting internal messages/tools into provider-native formats
/// - Converting provider SSE stream chunks into internal [`StreamEvent`]s
/// - Providing the base URL and any extra headers for the HTTP client
/// - Identifying the API mode (for logging / routing)
pub trait Transport: Send + Sync {
    // ── HTTP client configuration ────────────────────────────────────

    /// The API base URL for constructing the HTTP client.
    fn base_url(&self) -> &str;

    /// Extra HTTP headers to include in requests (e.g. OpenRouter's `HTTP-Referer`).
    fn extra_headers(&self) -> &HashMap<String, String>;

    /// A string identifying the API mode (e.g. `"chat_completions"`, `"anthropic"`).
    fn api_mode(&self) -> &str;

    // ── Request / response conversion ────────────────────────────────

    /// Convert an internal [`ChatRequest`] into an API-specific JSON body.
    fn normalize_request(&self, request: &ChatRequest) -> Result<Value, TransportError>;

    /// Convert an API-specific JSON response into an internal [`ChatResponse`].
    fn denormalize_response(&self, response: &Value) -> Result<ChatResponse, TransportError>;

    // ── Message / tool format conversion (with defaults) ─────────────

    /// Convert internal messages to provider-native format.
    ///
    /// Default: simple role+content JSON, no system extraction.
    fn normalize_messages(&self, messages: &[Message]) -> NormalizedMessages {
        let result: Vec<Value> = messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": match m.role {
                        crate::types::Role::System => "system",
                        crate::types::Role::User => "user",
                        crate::types::Role::Assistant => "assistant",
                        crate::types::Role::Tool => "tool",
                    },
                    "content": m.content,
                })
            })
            .collect();
        NormalizedMessages {
            system: None,
            messages: result,
        }
    }

    /// Convert internal tool definitions to provider-native format.
    ///
    /// Default: OpenAI-style type/function/parameters format.
    fn normalize_tools(&self, tools: &[ToolDefinition]) -> Vec<Value> {
        tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect()
    }

    /// Convert a provider SSE stream chunk to internal [`StreamEvent`].
    ///
    /// `event_type` is the SSE event field (e.g. `"content_block_delta"`).
    /// `data` is the raw data payload (after stripping `data: `).
    ///
    /// Default: returns an empty vec (no streaming support).
    fn denormalize_stream_chunk(&self, _event_type: &str, _data: &Value) -> Vec<StreamEvent> {
        vec![]
    }
}

// ---------------------------------------------------------------------------
// Re-exports
// ---------------------------------------------------------------------------

pub use chat_completions::ChatCompletionsTransport;
pub use dispatcher::TransportDispatcher;
pub use gemini::GeminiTransport;
pub use openai_compat::OpenAICompatTransport;
