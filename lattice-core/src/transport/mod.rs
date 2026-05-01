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
use crate::streaming::{SseParser, StreamEvent};
use crate::types::{Message, ToolDefinition};
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

/// Error type for transport-level operations.
///
/// Re-exported from [`chat_completions`] for backward compatibility.
pub use chat_completions::TransportError;

// ---------------------------------------------------------------------------
// Utility: convert a non-streaming ChatResponse into a sequence of StreamEvents
// ---------------------------------------------------------------------------

/// Convert a complete [`ChatResponse`] into a sequence of [`StreamEvent`]s
/// suitable for wrapping as a non-streaming stream.
///
/// Produces: Token (if content exists), ToolCallStart/ToolCallDelta (if tool
/// calls exist), and a final Done event with finish_reason and usage.
pub fn chat_response_to_stream(
    response: crate::provider::ChatResponse,
) -> Vec<crate::streaming::StreamEvent> {
    let mut events = Vec::new();

    if let Some(ref content) = response.content {
        if !content.is_empty() {
            events.push(crate::streaming::StreamEvent::Token {
                content: content.clone(),
            });
        }
    }

    if let Some(ref tool_calls) = response.tool_calls {
        for tc in tool_calls {
            events.push(crate::streaming::StreamEvent::ToolCallStart {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
            });
            events.push(crate::streaming::StreamEvent::ToolCallDelta {
                id: tc.id.clone(),
                arguments_delta: tc.function.arguments.clone(),
            });
            events.push(crate::streaming::StreamEvent::ToolCallEnd { id: tc.id.clone() });
        }
    }

    events.push(crate::streaming::StreamEvent::Done {
        finish_reason: response.finish_reason,
        usage: response.usage,
    });

    events
}

// ---------------------------------------------------------------------------
// TransportBase — shared base_url + extra_headers for all transports
// ---------------------------------------------------------------------------

/// Common base for all transport implementations: base URL + extra headers.
///
/// Eliminates the `{base_url, extra_headers}` field duplication across
/// ChatCompletionsTransport, AnthropicTransport, GeminiTransport, and
/// OpenAICompatTransport.
pub struct TransportBase {
    base_url: String,
    extra_headers: HashMap<String, String>,
}

impl TransportBase {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            extra_headers: HashMap::new(),
        }
    }

    pub fn with_extra_headers(
        base_url: impl Into<String>,
        extra_headers: HashMap<String, String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            extra_headers,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn extra_headers(&self) -> &HashMap<String, String> {
        &self.extra_headers
    }
}

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
    #[deprecated(note = "Use SseParser via chat() instead. This method produces divergent output and will be removed in a future version.")]
    fn denormalize_stream_chunk(&self, _event_type: &str, _data: &Value) -> Vec<StreamEvent> {
        vec![]
    }

    // ── HTTP transport helpers for chat() ──────────────────────────────

    /// The API path for streaming chat requests (e.g. `"/chat/completions"`).
    ///
    /// Default: `"/chat/completions"` (OpenAI-compatible).
    fn chat_endpoint(&self) -> &str {
        "/chat/completions"
    }

    /// The HTTP header name for API key authentication.
    ///
    /// Default: `"authorization"` (used with Bearer token).
    fn auth_header_name(&self) -> &str {
        "authorization"
    }

    /// Format the value of the auth header from the raw API key.
    ///
    /// Default: `"Bearer {api_key}"` (OpenAI-compatible).
    fn auth_header_value(&self, api_key: &str) -> String {
        format!("Bearer {}", api_key)
    }

    /// Apply authentication to a reqwest request builder.
    ///
    /// Default: sets `Authorization: Bearer <key>` header.
    /// Gemini overrides this to set `x-goog-api-key` instead.
    fn apply_auth_to_request(
        &self,
        req: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        req.header(self.auth_header_name(), self.auth_header_value(api_key))
    }

    /// Create an SSE parser for this transport's streaming format.
    ///
    /// The returned parser is used to convert raw SSE chunks into
    /// [`StreamEvent`]s during streaming.
    ///
    /// Default: [`crate::streaming::OpenAiSseParser`].
    fn create_sse_parser(&self) -> Box<dyn SseParser> {
        Box::new(crate::streaming::OpenAiSseParser::new())
    }

    // ── Body-building helpers (used by normalize_request) ────────────────

    /// Apply the temperature field to a request body, guarding against NaN
    /// and Infinity values that some APIs reject.
    ///
    /// Default: if `temp` is NaN/Infinity, print a warning and omit the
    /// field; otherwise set `body["temperature"]` to the numeric value.
    fn apply_temperature(&self, body: &mut Value, temp: Option<f64>) {
        if let Some(temp) = temp {
            if temp.is_nan() || temp.is_infinite() {
                tracing::warn!(
                    "temperature value {} is NaN or infinite, omitting temperature field",
                    temp
                );
            } else {
                body["temperature"] = Value::Number(
                    serde_json::Number::from_f64(temp)
                        .unwrap_or_else(|| serde_json::Number::from(0)),
                );
            }
        }
    }

    /// Set the `stream` flag on a request body.
    ///
    /// Default: `body["stream"] = true` when `stream` is true.
    fn set_stream_flag(&self, body: &mut Value, stream: bool) {
        if stream {
            body["stream"] = Value::Bool(true);
        }
    }
}

// ---------------------------------------------------------------------------
// Re-exports
// ---------------------------------------------------------------------------

pub use chat_completions::ChatCompletionsTransport;
pub use dispatcher::TransportDispatcher;
pub use gemini::GeminiTransport;
pub use openai_compat::OpenAICompatTransport;
