//! Gemini transport — message normalizer for Google's `generateContent` API format.
//!
//! Converts between Artemis internal types ([`Message`], [`ToolDefinition`],
//! [`ChatResponse`]) and the Gemini-native JSON schema used by
//! `models/{model}:generateContent`.
//!
//! Key differences from OpenAI's Chat Completions format:
//!
//! - Roles are `"user"` and `"model"` (not `"assistant"`)
//! - System messages go into a separate `systemInstruction` field, not the
//!   `contents` array
//! - Message body uses `"parts"` (array of part objects), not `"content"` (string)
//! - Function calls use `"functionCall"` with `"args"` (object), not
//!   `"arguments"` (JSON string)
//! - Tool results use `"functionResponse"` with `"response"` (object), not
//!   `"tool_call_id"` + `"content"` (string)
//! - Finish reasons are upper-case: `"STOP"`, `"MAX_TOKENS"`, `"SAFETY"`,
//!   `"RECITATION"`, `"OTHER"`

use crate::provider::ChatResponse;
use crate::streaming::TokenUsage;
use crate::types::{Message, Role, ToolCall, ToolDefinition};
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// Interface for converting between internal types and a provider's API format.
///
/// Each transport handles normalization (internal → API) and denormalization
/// (API → internal) for one API format.
pub trait Transport: Send + Sync {
    /// Convert a list of internal messages into the provider's request body
    /// (the `contents` array and any extra top-level fields like
    /// `systemInstruction`).
    ///
    /// Returns `(contents, extra_fields)` where `extra_fields` is a JSON
    /// object that may contain `systemInstruction` and other non-contents
    /// top-level keys.
    fn normalize_messages(&self, messages: &[Message]) -> (Value, Value);

    /// Convert a list of tool definitions into the provider's tools format.
    fn normalize_tools(&self, tools: &[ToolDefinition]) -> Value;

    /// Parse a full (non-streaming) response from the provider into our
    /// internal [`ChatResponse`].
    fn denormalize_response(&self, response: &Value) -> ChatResponse;

    /// Parse a single streaming chunk from the provider.
    ///
    /// Returns a list of [`StreamChunk`] items extracted from this chunk
    /// (Gemini can emit multiple parts per candidate).
    fn denormalize_stream_chunk(&self, chunk: &Value) -> Vec<StreamChunk>;
}

// ---------------------------------------------------------------------------
// Stream chunk (lightweight intermediate for streaming)
// ---------------------------------------------------------------------------

/// A single piece of content extracted from a streaming chunk.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamChunk {
    /// A text token.
    Token { content: String },

    /// A reasoning/thinking token.
    Thinking { content: String },

    /// A tool call delta (may arrive incrementally).
    ToolCallDelta {
        index: usize,
        id: String,
        name: String,
        arguments: String,
    },

    /// The stream is done.
    Done { finish_reason: String },

    /// Token usage reported in the final chunk.
    Usage { usage: TokenUsage },
}

// ---------------------------------------------------------------------------
// GeminiTransport
// ---------------------------------------------------------------------------

/// Message-format normalizer for the Gemini `generateContent` API.
pub struct GeminiTransport;

impl GeminiTransport {
    pub fn new() -> Self {
        GeminiTransport
    }

    /// Map Gemini finish reasons to our internal finish-reason strings.
    ///
    /// | Gemini          | Internal          |
    /// |-----------------|-------------------|
    /// | STOP            | stop              |
    /// | MAX_TOKENS      | length            |
    /// | SAFETY          | content_filter    |
    /// | RECITATION      | content_filter    |
    /// | OTHER           | stop              |
    fn map_finish_reason(reason: &str) -> String {
        match reason.to_uppercase().as_str() {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            "SAFETY" | "RECITATION" => "content_filter".to_string(),
            _ => "stop".to_string(),
        }
    }

    /// Generate a deterministic-ish tool call ID. Gemini doesn't provide
    /// call IDs, so we synthesize them.
    fn generate_call_id() -> String {
        format!("call_{}", &Uuid::new_v4().to_string().replace("-", "")[..12])
    }
}

impl Default for GeminiTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for GeminiTransport {
    fn normalize_messages(&self, messages: &[Message]) -> (Value, Value) {
        let mut system_parts: Vec<Value> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    if !msg.content.is_empty() {
                        system_parts.push(json!({"text": msg.content}));
                    }
                }
                Role::User => {
                    let mut parts: Vec<Value> = Vec::new();
                    if !msg.content.is_empty() {
                        parts.push(json!({"text": msg.content}));
                    }
                    if !parts.is_empty() {
                        contents.push(json!({
                            "role": "user",
                            "parts": parts,
                        }));
                    }
                }
                Role::Assistant => {
                    let mut parts: Vec<Value> = Vec::new();
                    if !msg.content.is_empty() {
                        parts.push(json!({"text": msg.content}));
                    }
                    if let Some(ref tool_calls) = msg.tool_calls {
                        for tc in tool_calls {
                            let args: Value =
                                serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
                            let args_obj = if args.is_object() {
                                args
                            } else {
                                json!({"_value": args})
                            };
                            parts.push(json!({
                                "functionCall": {
                                    "name": tc.function.name,
                                    "args": args_obj,
                                }
                            }));
                        }
                    }
                    if !parts.is_empty() {
                        contents.push(json!({
                            "role": "model",
                            "parts": parts,
                        }));
                    }
                }
                Role::Tool => {
                    let tool_name = msg
                        .name
                        .as_deref()
                        .unwrap_or(msg.tool_call_id.as_deref().unwrap_or("tool"));
                    let response: Value = if msg.content.trim().starts_with('{')
                        || msg.content.trim().starts_with('[')
                    {
                        serde_json::from_str(&msg.content).unwrap_or_else(|_| {
                            json!({"output": msg.content})
                        })
                    } else {
                        json!({"output": msg.content})
                    };
                    let response_obj = if response.is_object() {
                        response
                    } else {
                        json!({"output": response})
                    };
                    contents.push(json!({
                        "role": "user",
                        "parts": [{
                            "functionResponse": {
                                "name": tool_name,
                                "response": response_obj,
                            }
                        }],
                    }));
                }
            }
        }

        let mut extra = json!({});
        if !system_parts.is_empty() {
            extra["systemInstruction"] = json!({"parts": system_parts});
        }

        (json!(contents), extra)
    }

    fn normalize_tools(&self, tools: &[ToolDefinition]) -> Value {
        let declarations: Vec<Value> = tools
            .iter()
            .map(|td| {
                let mut decl = json!({
                    "name": td.name,
                    "description": td.description,
                });
                if !td.parameters.is_null() && td.parameters != json!({}) {
                    decl["parameters"] = td.parameters.clone();
                }
                decl
            })
            .collect();

        if declarations.is_empty() {
            json!([])
        } else {
            json!([{"functionDeclarations": declarations}])
        }
    }

    fn denormalize_response(&self, response: &Value) -> ChatResponse {
        let candidates = response.get("candidates").and_then(|c| c.as_array());
        let (content_parts, finish_reason_raw) = match candidates {
            Some(cands) if !cands.is_empty() => {
                let cand = &cands[0];
                let parts = cand
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                    .cloned()
                    .unwrap_or_default();
                let reason = cand
                    .get("finishReason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("STOP")
                    .to_string();
                (parts, reason)
            }
            _ => {
                return ChatResponse {
                    content: None,
                    tool_calls: None,
                    usage: None,
                    finish_reason: "stop".to_string(),
                    model: String::new(),
                };
            }
        };

        let mut text_pieces: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for part in &content_parts {
            // Thinking/reasoning part — skip for content but don't error
            if part.get("thought").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                text_pieces.push(text.to_string());
            }
            if let Some(fc) = part.get("functionCall") {
                let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = fc.get("args").cloned().unwrap_or(json!({}));
                let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(ToolCall {
                    id: Self::generate_call_id(),
                    function: crate::types::FunctionCall {
                        name: name.to_string(),
                        arguments: args_str,
                    },
                });
            }
        }

        let has_tool_calls = !tool_calls.is_empty();
        let finish_reason = if has_tool_calls {
            "tool_calls".to_string()
        } else {
            Self::map_finish_reason(&finish_reason_raw)
        };

        let usage_meta = response.get("usageMetadata");
        let usage = usage_meta.map(|u| TokenUsage {
            prompt_tokens: u
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            completion_tokens: u
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            total_tokens: u
                .get("totalTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        });

        ChatResponse {
            content: if text_pieces.is_empty() {
                None
            } else {
                Some(text_pieces.join(""))
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            usage,
            finish_reason,
            model: String::new(),
        }
    }

    fn denormalize_stream_chunk(&self, chunk: &Value) -> Vec<StreamChunk> {
        let candidates = chunk.get("candidates").and_then(|c| c.as_array());
        let (content_parts, finish_reason_raw) = match candidates {
            Some(cands) if !cands.is_empty() => {
                let cand = &cands[0];
                let parts = cand
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                    .cloned()
                    .unwrap_or_default();
                let reason = cand
                    .get("finishReason")
                    .and_then(|r| r.as_str())
                    .map(|s| s.to_string());
                (parts, reason)
            }
            _ => return Vec::new(),
        };

        let mut results: Vec<StreamChunk> = Vec::new();

        for part in &content_parts {
            // Thinking part
            if part.get("thought").and_then(|v| v.as_bool()) == Some(true) {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    results.push(StreamChunk::Thinking {
                        content: text.to_string(),
                    });
                }
                continue;
            }
            // Text token
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    results.push(StreamChunk::Token {
                        content: text.to_string(),
                    });
                }
            }
            // Function call
            if let Some(fc) = part.get("functionCall") {
                let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = fc.get("args").cloned().unwrap_or(json!({}));
                let args_str =
                    serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                results.push(StreamChunk::ToolCallDelta {
                    index: results.len(),
                    id: Self::generate_call_id(),
                    name: name.to_string(),
                    arguments: args_str,
                });
            }
        }

        // Finish reason
        if let Some(ref reason) = finish_reason_raw {
            let mapped = Self::map_finish_reason(reason);
            results.push(StreamChunk::Done {
                finish_reason: mapped,
            });
        }

        // Usage metadata (appears in final chunk)
        if let Some(u) = chunk.get("usageMetadata") {
            results.push(StreamChunk::Usage {
                usage: TokenUsage {
                    prompt_tokens: u
                        .get("promptTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    completion_tokens: u
                        .get("candidatesTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    total_tokens: u
                        .get("totalTokenCount")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                },
            });
        }

        results
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};

    // ── normalize_messages ───────────────────────────────────────────

    #[test]
    fn test_system_instruction_extraction() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "You are a helpful assistant.".to_string(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: "Hello!".to_string(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        let transport = GeminiTransport::new();
        let (contents, extra) = transport.normalize_messages(&messages);

        // System message should NOT appear in contents
        let contents_arr = contents.as_array().unwrap();
        assert_eq!(contents_arr.len(), 1);
        assert_eq!(contents_arr[0]["role"], "user");

        // System message should be in systemInstruction
        let sys = &extra["systemInstruction"];
        assert!(sys.is_object());
        let sys_parts = sys["parts"].as_array().unwrap();
        assert_eq!(sys_parts.len(), 1);
        assert_eq!(sys_parts[0]["text"], "You are a helpful assistant.");
    }

    #[test]
    fn test_normalize_user_message() {
        let messages = vec![Message {
            role: Role::User,
            content: "What is Rust?".to_string(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];

        let transport = GeminiTransport::new();
        let (contents, extra) = transport.normalize_messages(&messages);

        let contents_arr = contents.as_array().unwrap();
        assert_eq!(contents_arr.len(), 1);
        assert_eq!(contents_arr[0]["role"], "user");

        let parts = contents_arr[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "What is Rust?");

        // No system instruction
        assert!(extra.get("systemInstruction").is_none());
    }

    #[test]
    fn test_normalize_model_with_function_call() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: "call_abc".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city": "Tokyo"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        }];

        let transport = GeminiTransport::new();
        let (contents, _) = transport.normalize_messages(&messages);

        let contents_arr = contents.as_array().unwrap();
        assert_eq!(contents_arr.len(), 1);
        assert_eq!(contents_arr[0]["role"], "model");

        let parts = contents_arr[0]["parts"].as_array().unwrap();
        // No text part since content is empty
        assert_eq!(parts.len(), 1);

        let fc = &parts[0]["functionCall"];
        assert_eq!(fc["name"], "get_weather");
        assert_eq!(fc["args"]["city"], "Tokyo");
        // Gemini uses "args" (object), not "arguments" (string)
        assert!(fc["args"].is_object());
    }

    #[test]
    fn test_normalize_function_response() {
        let messages = vec![Message {
            role: Role::Tool,
            content: r#"{"temperature": 22, "condition": "sunny"}"#.to_string(),
            tool_calls: None,
            tool_call_id: Some("call_abc".to_string()),
            name: Some("get_weather".to_string()),
        }];

        let transport = GeminiTransport::new();
        let (contents, _) = transport.normalize_messages(&messages);

        let contents_arr = contents.as_array().unwrap();
        assert_eq!(contents_arr.len(), 1);
        // Tool messages become "user" role in Gemini
        assert_eq!(contents_arr[0]["role"], "user");

        let parts = contents_arr[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);

        let fr = &parts[0]["functionResponse"];
        assert_eq!(fr["name"], "get_weather");
        // The JSON content was parsed into the response object
        assert_eq!(fr["response"]["temperature"], 22);
        assert_eq!(fr["response"]["condition"], "sunny");
    }

    // ── denormalize_response ─────────────────────────────────────────

    #[test]
    fn test_denormalize_text_response() {
        let gemini_response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello! How can I help?"}],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 5,
                "totalTokenCount": 15
            }
        });

        let transport = GeminiTransport::new();
        let response = transport.denormalize_response(&gemini_response);

        assert_eq!(response.content.as_deref(), Some("Hello! How can I help?"));
        assert!(response.tool_calls.is_none());
        assert_eq!(response.finish_reason, "stop");
        let usage = response.usage.unwrap();
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn test_denormalize_function_call_response() {
        let gemini_response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}}}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 20,
                "candidatesTokenCount": 8,
                "totalTokenCount": 28
            }
        });

        let transport = GeminiTransport::new();
        let response = transport.denormalize_response(&gemini_response);

        assert!(response.content.is_none());
        let tool_calls = response.tool_calls.unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].function.name, "get_weather");
        // Arguments should be a JSON string of the args object
        let args: Value =
            serde_json::from_str(&tool_calls[0].function.arguments).unwrap();
        assert_eq!(args["city"], "Paris");
        // finish_reason should be "tool_calls" when function calls are present
        assert_eq!(response.finish_reason, "tool_calls");
    }

    // ── normalize_tools ──────────────────────────────────────────────

    #[test]
    fn test_normalize_tools() {
        let tools = vec![ToolDefinition {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Search query"}
                },
                "required": ["query"]
            }),
        }];

        let transport = GeminiTransport::new();
        let result = transport.normalize_tools(&tools);

        let tools_arr = result.as_array().unwrap();
        assert_eq!(tools_arr.len(), 1);
        let decls = tools_arr[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "search");
        assert_eq!(decls[0]["description"], "Search the web");
        assert!(decls[0]["parameters"].is_object());
    }

    // ── denormalize_stream_chunk ─────────────────────────────────────

    #[test]
    fn test_denormalize_stream_chunk_text() {
        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Hello"}],
                    "role": "model"
                }
            }]
        });

        let transport = GeminiTransport::new();
        let results = transport.denormalize_stream_chunk(&chunk);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0],
            StreamChunk::Token {
                content: "Hello".to_string()
            }
        );
    }

    #[test]
    fn test_denormalize_stream_chunk_with_thinking() {
        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "Let me think...", "thought": true},
                        {"text": "The answer is 42"}
                    ],
                    "role": "model"
                }
            }]
        });

        let transport = GeminiTransport::new();
        let results = transport.denormalize_stream_chunk(&chunk);

        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0],
            StreamChunk::Thinking {
                content: "Let me think...".to_string()
            }
        );
        assert_eq!(
            results[1],
            StreamChunk::Token {
                content: "The answer is 42".to_string()
            }
        );
    }

    #[test]
    fn test_denormalize_stream_chunk_with_finish() {
        let chunk = json!({
            "candidates": [{
                "content": {
                    "parts": [],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 3,
                "totalTokenCount": 8
            }
        });

        let transport = GeminiTransport::new();
        let results = transport.denormalize_stream_chunk(&chunk);

        // Should have Done + Usage
        assert!(results.len() >= 2);
        let has_done = results.iter().any(|r| matches!(r, StreamChunk::Done { .. }));
        let has_usage = results.iter().any(|r| matches!(r, StreamChunk::Usage { .. }));
        assert!(has_done);
        assert!(has_usage);
    }

    // ── finish reason mapping ────────────────────────────────────────

    #[test]
    fn test_finish_reason_mapping() {
        assert_eq!(GeminiTransport::map_finish_reason("STOP"), "stop");
        assert_eq!(GeminiTransport::map_finish_reason("MAX_TOKENS"), "length");
        assert_eq!(GeminiTransport::map_finish_reason("SAFETY"), "content_filter");
        assert_eq!(GeminiTransport::map_finish_reason("RECITATION"), "content_filter");
        assert_eq!(GeminiTransport::map_finish_reason("OTHER"), "stop");
        assert_eq!(GeminiTransport::map_finish_reason("UNKNOWN"), "stop");
        // Case insensitive
        assert_eq!(GeminiTransport::map_finish_reason("stop"), "stop");
    }

    // ── edge cases ───────────────────────────────────────────────────

    #[test]
    fn test_denormalize_empty_response() {
        let gemini_response = json!({
            "candidates": []
        });

        let transport = GeminiTransport::new();
        let response = transport.denormalize_response(&gemini_response);

        assert!(response.content.is_none());
        assert!(response.tool_calls.is_none());
        assert_eq!(response.finish_reason, "stop");
    }

    #[test]
    fn test_denormalize_safety_finish() {
        let gemini_response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "I can't"}],
                    "role": "model"
                },
                "finishReason": "SAFETY"
            }]
        });

        let transport = GeminiTransport::new();
        let response = transport.denormalize_response(&gemini_response);

        assert_eq!(response.finish_reason, "content_filter");
    }

    #[test]
    fn test_normalize_tools_empty() {
        let transport = GeminiTransport::new();
        let result = transport.normalize_tools(&[]);
        assert_eq!(result, json!([]));
    }

    #[test]
    fn test_normalize_function_response_plain_text() {
        // Tool result with plain text (not JSON) content
        let messages = vec![Message {
            role: Role::Tool,
            content: "The weather is sunny".to_string(),
            tool_calls: None,
            tool_call_id: Some("call_xyz".to_string()),
            name: Some("get_weather".to_string()),
        }];

        let transport = GeminiTransport::new();
        let (contents, _) = transport.normalize_messages(&messages);

        let fr = &contents.as_array().unwrap()[0]["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "get_weather");
        // Plain text gets wrapped: {"output": "The weather is sunny"}
        assert_eq!(fr["response"]["output"], "The weather is sunny");
    }

    #[test]
    fn test_denormalize_thinking_part_in_full_response() {
        let gemini_response = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"text": "Reasoning step 1...", "thought": true},
                        {"text": "The answer is 42"}
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }]
        });

        let transport = GeminiTransport::new();
        let response = transport.denormalize_response(&gemini_response);

        // Thinking parts should be skipped in content
        assert_eq!(response.content.as_deref(), Some("The answer is 42"));
        assert_eq!(response.finish_reason, "stop");
    }

    #[test]
    fn test_normalize_multiple_system_messages() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "System part 1".to_string(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::System,
                content: "System part 2".to_string(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        let transport = GeminiTransport::new();
        let (_, extra) = transport.normalize_messages(&messages);

        let sys_parts = extra["systemInstruction"]["parts"].as_array().unwrap();
        assert_eq!(sys_parts.len(), 2);
        assert_eq!(sys_parts[0]["text"], "System part 1");
        assert_eq!(sys_parts[1]["text"], "System part 2");
    }
}
