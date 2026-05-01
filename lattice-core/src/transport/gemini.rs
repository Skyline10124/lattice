//! Gemini transport — message normalizer for Google's `generateContent` API format.
//!
//! Converts between LATTICE internal types ([`ChatRequest`], [`ChatResponse`])
//! and the Gemini-native JSON schema used by `models/{model}:generateContent`.
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

use std::collections::HashMap;

use crate::provider::{ChatRequest, ChatResponse};
use crate::streaming::TokenUsage;
use crate::transport::chat_completions::{Transport, TransportError};
use crate::transport::TransportBase;
use crate::types::Role;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Stream chunk (lightweight intermediate for streaming)
// ---------------------------------------------------------------------------

/// A single piece of content extracted from a streaming chunk.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamChunk {
    Token {
        content: String,
    },
    Thinking {
        content: String,
    },
    ToolCallDelta {
        index: usize,
        id: String,
        name: String,
        arguments: String,
    },
    Done {
        finish_reason: String,
    },
    Usage {
        usage: TokenUsage,
    },
}

// ---------------------------------------------------------------------------
// GeminiTransport
// ---------------------------------------------------------------------------

/// Message-format normalizer for the Gemini `generateContent` API.
pub struct GeminiTransport {
    base: TransportBase,
}

impl GeminiTransport {
    pub fn new() -> Self {
        Self {
            base: TransportBase::new("https://generativelanguage.googleapis.com/v1beta"),
        }
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base: TransportBase::new(base_url),
        }
    }

    /// Map Gemini finish reasons to our internal finish-reason strings.
    ///
    /// | Gemini          | Internal          |
    /// |-----------------|-------------------|
    /// | STOP            | stop              |
    /// | MAX_TOKENS      | length            |
    /// | SAFETY          | content_filter    |
    /// | RECITATION      | content_filter    |
    /// | OTHER           | unknown           |
    fn map_finish_reason(reason: &str) -> String {
        match reason.to_uppercase().as_str() {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            "SAFETY" | "RECITATION" => "content_filter".to_string(),
            "OTHER" => "unknown".to_string(),
            _ => "unknown".to_string(),
        }
    }

    fn generate_call_id(name: &str, index: usize) -> String {
        format!("tc_{name}_{index}")
    }

    /// Build the Gemini `contents` array and `systemInstruction` from internal messages.
    fn build_contents(messages: &[crate::types::Message]) -> (Value, Option<Value>) {
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
                        contents.push(json!({"role": "user", "parts": parts}));
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
                        contents.push(json!({"role": "model", "parts": parts}));
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
                        serde_json::from_str(&msg.content)
                            .unwrap_or_else(|_| json!({"output": msg.content}))
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
                        "parts": [{"functionResponse": {"name": tool_name, "response": response_obj}}],
                    }));
                }
            }
        }

        let system_instruction = if system_parts.is_empty() {
            None
        } else {
            Some(json!({"parts": system_parts}))
        };

        (json!(contents), system_instruction)
    }

    /// Build the Gemini `tools` array from internal tool definitions.
    fn build_tools(tools: &[crate::types::ToolDefinition]) -> Value {
        let declarations: Vec<Value> = tools
            .iter()
            .map(|td| {
                let mut decl = json!({"name": td.name, "description": td.description});
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

    /// Parse a full Gemini response into a ChatResponse.
    fn parse_response(response: &Value) -> Result<ChatResponse, TransportError> {
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
                return Ok(ChatResponse {
                    content: None,
                    reasoning_content: None,
                    tool_calls: None,
                    usage: None,
                    finish_reason: "stop".to_string(),
                    model: String::new(),
                });
            }
        };

        let mut text_pieces: Vec<String> = Vec::new();
        let mut tool_calls: Vec<crate::types::ToolCall> = Vec::new();

        let mut tc_index = 0;
        for part in content_parts.iter() {
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
                tool_calls.push(crate::types::ToolCall {
                    id: Self::generate_call_id(name, tc_index),
                    function: crate::types::FunctionCall {
                        name: name.to_string(),
                        arguments: args_str,
                    },
                });
                tc_index += 1;
            }
        }

        let has_tool_calls = !tool_calls.is_empty();
        let finish_reason = if has_tool_calls {
            "tool_calls".to_string()
        } else {
            Self::map_finish_reason(&finish_reason_raw)
        };

        let model = response
            .get("modelVersion")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();

        let usage = response.get("usageMetadata").map(|u| TokenUsage {
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

        Ok(ChatResponse {
            content: if text_pieces.is_empty() {
                None
            } else {
                Some(text_pieces.join(""))
            },
            reasoning_content: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            usage,
            finish_reason,
            model,
        })
    }

    /// Parse a single streaming chunk from the Gemini API.
    ///
    /// Returns a list of [`StreamChunk`] items extracted from this chunk
    /// (Gemini can emit multiple parts per candidate).
    pub fn denormalize_stream_chunk(&self, chunk: &Value) -> Vec<StreamChunk> {
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
        let mut has_tool_calls_in_chunk = false;
        let mut tc_index = 0;

        for part in &content_parts {
            if part.get("thought").and_then(|v| v.as_bool()) == Some(true) {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    results.push(StreamChunk::Thinking {
                        content: text.to_string(),
                    });
                }
                continue;
            }
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if !text.is_empty() {
                    results.push(StreamChunk::Token {
                        content: text.to_string(),
                    });
                }
            }
            if let Some(fc) = part.get("functionCall") {
                let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = fc.get("args").cloned().unwrap_or(json!({}));
                let args_str = serde_json::to_string(&args).unwrap_or_else(|_| "{}".to_string());
                let id = Self::generate_call_id(name, tc_index);
                tc_index += 1;
                results.push(StreamChunk::ToolCallDelta {
                    index: tc_index - 1,
                    id,
                    name: name.to_string(),
                    arguments: args_str,
                });
                has_tool_calls_in_chunk = true;
            }
        }

        if let Some(ref reason) = finish_reason_raw {
            let mapped_reason = if has_tool_calls_in_chunk {
                "tool_calls".to_string()
            } else {
                Self::map_finish_reason(reason)
            };
            results.push(StreamChunk::Done {
                finish_reason: mapped_reason,
            });
        }

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
// send_gemini_nonstreaming_request — non-streaming Gemini REST API call
// ---------------------------------------------------------------------------

/// POST the normalized request body to the Gemini `:generateContent` endpoint,
/// parse the JSON response, convert it to a stream of [`StreamEvent`]s.
///
/// Gemini uses a different URL pattern than OpenAI/Anthropic:
/// `{base_url}/models/{model}:generateContent` for non-streaming.
pub async fn send_gemini_nonstreaming_request(
    transport: &dyn crate::transport::Transport,
    client: &reqwest::Client,
    resolved: &crate::catalog::ResolvedModel,
    body: &serde_json::Value,
) -> Result<
    std::pin::Pin<Box<dyn futures::Stream<Item = crate::streaming::StreamEvent> + Send>>,
    crate::LatticeError,
> {
    use crate::LatticeError;

    let base_url = resolved.base_url.trim_end_matches('/');
    let model = &resolved.api_model_id;
    let url = format!("{}/models/{}:generateContent", base_url, model);

    let mut req = client.post(&url).json(body);

    if let Some(ref api_key) = resolved.api_key {
        req = transport.apply_auth_to_request(req, api_key.as_str());
    }

    for (key, value) in &resolved.provider_specific {
        if let Some(header_name) = key.strip_prefix("header:") {
            req = req.header(header_name, value);
        }
    }

    let response = req.send().await.map_err(|e| LatticeError::Network {
        message: format!("HTTP request failed: {}", e),
        status: e.status().map(|s| s.as_u16()),
    })?;

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_default();
        return Err(crate::errors::ErrorClassifier::classify(
            status.as_u16(),
            &body_text,
            &resolved.provider,
        ));
    }

    let body_json: serde_json::Value =
        response.json().await.map_err(|e| LatticeError::Streaming {
            message: format!("Failed to parse Gemini response JSON: {}", e),
        })?;

    let chat_response =
        transport
            .denormalize_response(&body_json)
            .map_err(|e| LatticeError::Streaming {
                message: format!("Failed to denormalize Gemini response: {}", e),
            })?;

    let events = crate::transport::chat_response_to_stream(chat_response);
    Ok(Box::pin(futures::stream::iter(events)))
}

impl Default for GeminiTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for GeminiTransport {
    fn apply_auth_to_request(
        &self,
        req: reqwest::RequestBuilder,
        api_key: &str,
    ) -> reqwest::RequestBuilder {
        req.header("x-goog-api-key", api_key)
    }
    fn base_url(&self) -> &str {
        self.base.base_url()
    }

    fn extra_headers(&self) -> &HashMap<String, String> {
        self.base.extra_headers()
    }

    fn api_mode(&self) -> &str {
        "gemini"
    }

    fn normalize_request(&self, request: &ChatRequest) -> Result<Value, TransportError> {
        let (contents, system_instruction) = Self::build_contents(&request.messages);
        let mut body = json!({"contents": contents});

        if let Some(sys) = system_instruction {
            body["systemInstruction"] = sys;
        }

        let tools = Self::build_tools(&request.tools);
        if let Some(tools_arr) = tools.as_array() {
            if !tools_arr.is_empty() {
                body["tools"] = tools;
            }
        }

        let mut generation_config = json!({});
        if let Some(temp) = request.temperature {
            generation_config["temperature"] = json!(temp);
        }
        if let Some(max_tokens) = request.max_tokens {
            generation_config["maxOutputTokens"] = json!(max_tokens);
        }
        if generation_config
            .as_object()
            .map(|o| !o.is_empty())
            .unwrap_or(false)
        {
            body["generationConfig"] = generation_config;
        }

        Ok(body)
    }

    fn denormalize_response(&self, response: &Value) -> Result<ChatResponse, TransportError> {
        Self::parse_response(response)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ApiProtocol, CredentialStatus, ResolvedModel};
    use crate::types::{FunctionCall, Message, Role, ToolCall, ToolDefinition};
    use std::collections::HashMap;

    fn make_request(messages: Vec<Message>, tools: Vec<ToolDefinition>) -> ChatRequest {
        ChatRequest {
            messages,
            tools,
            model: "gemini-2.5-flash".to_string(),
            temperature: None,
            max_tokens: None,
            stream: false,
            thinking: None,
            reasoning_effort: None,
            resolved: ResolvedModel {
                canonical_id: "gemini-2.5-flash".into(),
                provider: "gemini".into(),
                api_key: Some("test-key".into()),
                base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
                api_protocol: ApiProtocol::GeminiGenerateContent,
                api_model_id: "gemini-2.5-flash".into(),
                context_length: 1048576,
                provider_specific: HashMap::new(),
                credential_status: CredentialStatus::Present,
            },
        }
    }

    // ── normalize_messages ───────────────────────────────────────────

    #[test]
    fn test_system_instruction_extraction() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "You are a helpful assistant.".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: "Hello!".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        let transport = GeminiTransport::new();
        let body = transport
            .normalize_request(&make_request(messages, vec![]))
            .unwrap();

        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");

        let sys = &body["systemInstruction"];
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
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];

        let transport = GeminiTransport::new();
        let body = transport
            .normalize_request(&make_request(messages, vec![]))
            .unwrap();

        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");

        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["text"], "What is Rust?");
    }

    #[test]
    fn test_normalize_model_with_function_call() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: String::new(),
            reasoning_content: None,
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
        let body = transport
            .normalize_request(&make_request(messages, vec![]))
            .unwrap();

        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "model");

        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);

        let fc = &parts[0]["functionCall"];
        assert_eq!(fc["name"], "get_weather");
        assert_eq!(fc["args"]["city"], "Tokyo");
        assert!(fc["args"].is_object());
    }

    #[test]
    fn test_normalize_function_response() {
        let messages = vec![Message {
            role: Role::Tool,
            content: r#"{"temperature": 22, "condition": "sunny"}"#.to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_abc".to_string()),
            name: Some("get_weather".to_string()),
        }];

        let transport = GeminiTransport::new();
        let body = transport
            .normalize_request(&make_request(messages, vec![]))
            .unwrap();

        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");

        let fr = &contents[0]["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "get_weather");
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
        let response = transport.denormalize_response(&gemini_response).unwrap();

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
        let response = transport.denormalize_response(&gemini_response).unwrap();

        assert!(response.content.is_none());
        let tool_calls = response.tool_calls.unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert!(tool_calls[0].id.starts_with("tc_"));
        assert_eq!(tool_calls[0].function.name, "get_weather");
        let args: Value = serde_json::from_str(&tool_calls[0].function.arguments).unwrap();
        assert_eq!(args["city"], "Paris");
        assert_eq!(response.finish_reason, "tool_calls");
    }

    #[test]
    fn test_denormalize_empty_response() {
        let gemini_response = json!({"candidates": []});

        let transport = GeminiTransport::new();
        let response = transport.denormalize_response(&gemini_response).unwrap();

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
        let response = transport.denormalize_response(&gemini_response).unwrap();

        assert_eq!(response.finish_reason, "content_filter");
    }

    #[test]
    fn test_denormalize_recitation_finish() {
        let gemini_response = json!({
            "candidates": [{
                "content": {"parts": [], "role": "model"},
                "finishReason": "RECITATION"
            }]
        });

        let transport = GeminiTransport::new();
        let response = transport.denormalize_response(&gemini_response).unwrap();

        assert_eq!(response.finish_reason, "content_filter");
    }

    #[test]
    fn test_denormalize_max_tokens_finish() {
        let gemini_response = json!({
            "candidates": [{
                "content": {
                    "parts": [{"text": "Once upon a"}],
                    "role": "model"
                },
                "finishReason": "MAX_TOKENS"
            }]
        });

        let transport = GeminiTransport::new();
        let response = transport.denormalize_response(&gemini_response).unwrap();

        assert_eq!(response.finish_reason, "length");
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
        let body = transport
            .normalize_request(&make_request(vec![], tools))
            .unwrap();

        let tools_arr = body["tools"].as_array().unwrap();
        assert_eq!(tools_arr.len(), 1);
        let decls = tools_arr[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(decls.len(), 1);
        assert_eq!(decls[0]["name"], "search");
        assert_eq!(decls[0]["description"], "Search the web");
        assert!(decls[0]["parameters"].is_object());
    }

    #[test]
    fn test_normalize_tools_empty() {
        let transport = GeminiTransport::new();
        let body = transport
            .normalize_request(&make_request(vec![], vec![]))
            .unwrap();
        assert!(body.get("tools").is_none());
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
                "content": {"parts": [], "role": "model"},
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

        assert!(results.len() >= 2);
        let has_done = results
            .iter()
            .any(|r| matches!(r, StreamChunk::Done { .. }));
        let has_usage = results
            .iter()
            .any(|r| matches!(r, StreamChunk::Usage { .. }));
        assert!(has_done);
        assert!(has_usage);
    }

    #[test]
    fn test_denormalize_stream_chunk_tool_calls_finish_reason() {
        // When a streaming chunk contains functionCall parts,
        // finish_reason must be "tool_calls" even though Gemini sends "STOP".
        let chunk = json!({
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
        let results = transport.denormalize_stream_chunk(&chunk);

        // Should have ToolCallDelta + Done("tool_calls") + Usage
        let done = results.iter().find_map(|r| match r {
            StreamChunk::Done { finish_reason } => Some(finish_reason.clone()),
            _ => None,
        });
        assert_eq!(done, Some("tool_calls".to_string()));
    }

    // ── finish reason mapping ────────────────────────────────────────

    #[test]
    fn test_finish_reason_mapping() {
        assert_eq!(GeminiTransport::map_finish_reason("STOP"), "stop");
        assert_eq!(GeminiTransport::map_finish_reason("MAX_TOKENS"), "length");
        assert_eq!(
            GeminiTransport::map_finish_reason("SAFETY"),
            "content_filter"
        );
        assert_eq!(
            GeminiTransport::map_finish_reason("RECITATION"),
            "content_filter"
        );
        assert_eq!(GeminiTransport::map_finish_reason("OTHER"), "unknown");
        assert_eq!(GeminiTransport::map_finish_reason("UNKNOWN"), "unknown");
        assert_eq!(GeminiTransport::map_finish_reason("stop"), "stop");
    }

    // ── transport trait ──────────────────────────────────────────────

    #[test]
    fn test_api_mode() {
        let transport = GeminiTransport::new();
        assert_eq!(transport.api_mode(), "gemini");
    }

    #[test]
    fn test_default_base_url() {
        let transport = GeminiTransport::new();
        assert_eq!(
            transport.base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn test_custom_base_url() {
        let transport = GeminiTransport::with_base_url("https://custom.googleapis.com/v1");
        assert_eq!(transport.base_url(), "https://custom.googleapis.com/v1");
    }

    // ── edge cases ───────────────────────────────────────────────────

    #[test]
    fn test_normalize_function_response_plain_text() {
        let messages = vec![Message {
            role: Role::Tool,
            content: "The weather is sunny".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_xyz".to_string()),
            name: Some("get_weather".to_string()),
        }];

        let transport = GeminiTransport::new();
        let body = transport
            .normalize_request(&make_request(messages, vec![]))
            .unwrap();

        let fr = &body["contents"][0]["parts"][0]["functionResponse"];
        assert_eq!(fr["name"], "get_weather");
        assert_eq!(fr["response"]["output"], "The weather is sunny");
    }

    #[test]
    fn test_denormalize_thinking_part_skipped_in_content() {
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
        let response = transport.denormalize_response(&gemini_response).unwrap();

        assert_eq!(response.content.as_deref(), Some("The answer is 42"));
        assert_eq!(response.finish_reason, "stop");
    }

    #[test]
    fn test_normalize_multiple_system_messages() {
        let messages = vec![
            Message {
                role: Role::System,
                content: "System part 1".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::System,
                content: "System part 2".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        let transport = GeminiTransport::new();
        let body = transport
            .normalize_request(&make_request(messages, vec![]))
            .unwrap();

        let sys_parts = body["systemInstruction"]["parts"].as_array().unwrap();
        assert_eq!(sys_parts.len(), 2);
        assert_eq!(sys_parts[0]["text"], "System part 1");
        assert_eq!(sys_parts[1]["text"], "System part 2");
    }

    #[test]
    fn test_normalize_request_includes_generation_config() {
        let messages = vec![Message {
            role: Role::User,
            content: "Hello".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];

        let mut req = make_request(messages, vec![]);
        req.temperature = Some(0.7);
        req.max_tokens = Some(100);

        let transport = GeminiTransport::new();
        let body = transport.normalize_request(&req).unwrap();

        assert_eq!(body["generationConfig"]["temperature"], 0.7);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 100);
    }
}
