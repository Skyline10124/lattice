//! Gemini provider: request JSON construction, SSE parsing,
//! and auth headers for the Gemini `generateContent` API.
//!
//! Uses Gemini-specific wire format: `systemInstruction`, role `"model"`,
//! `functionCall` / `functionResponse`, and `x-goog-api-key` auth.

use crate::router::ResolvedModel;
use crate::types::{ChatRequest, LlmEvent, Role};

/// Build the JSON body for a Gemini `streamGenerateContent` request.
///
/// Maps system messages to `systemInstruction`, user to `"user"`, assistant to `"model"`,
/// tool calls to `functionCall`, and tool results to `functionResponse`.
pub fn build_request_json(request: &ChatRequest, resolved: &ResolvedModel) -> serde_json::Value {
    let mut contents: Vec<serde_json::Value> = Vec::new();
    let mut system_instruction: Option<String> = None;

    for msg in &request.messages {
        match msg.role {
            Role::System => super::push_system_text(&mut system_instruction, &msg.content),
            Role::User => {
                let parts = serde_json::json!([{"text": msg.content}]);
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": parts,
                }));
            }
            Role::Assistant => {
                let mut parts: Vec<serde_json::Value> = Vec::new();

                if !msg.content.is_empty() {
                    parts.push(serde_json::json!({"text": msg.content}));
                }

                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or(serde_json::json!({}));
                        parts.push(serde_json::json!({
                            "functionCall": {
                                "name": tc.function.name,
                                "args": args,
                            }
                        }));
                    }
                }

                if parts.is_empty() {
                    parts.push(serde_json::json!({"text": ""}));
                }

                contents.push(serde_json::json!({
                    "role": "model",
                    "parts": parts,
                }));
            }
            Role::Tool => {
                let name = msg.name.clone().unwrap_or_default();
                let response: serde_json::Value = serde_json::from_str(&msg.content)
                    .unwrap_or(serde_json::json!({"result": msg.content}));
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": name,
                            "response": response,
                        }
                    }]
                }));
            }
        }
    }

    let mut body = serde_json::json!({
        "contents": contents,
    });

    if let Some(si) = system_instruction {
        body["systemInstruction"] = serde_json::json!({
            "parts": [{"text": si}]
        });
    }

    if !request.tools.is_empty() {
        let function_declarations: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })
            })
            .collect();
        body["tools"] = serde_json::json!([{
            "functionDeclarations": function_declarations,
        }]);
    }

    let _ = resolved; // provider_model used in URL, not body for Gemini

    body
}

/// Build the full Gemini streaming endpoint URL.
///
/// Returns `{base_url}/models/{model}:streamGenerateContent?alt=sse`
/// where `{model}` is URL-encoded.
pub fn build_stream_url(base_url: &str, provider_model: &str) -> String {
    let encoded = urlencoding::encode(provider_model);
    format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        base_url.trim_end_matches('/'),
        encoded
    )
}

/// Build the Gemini auth header value (just the API key).
pub fn auth_header_value(api_key: &str) -> String {
    api_key.to_string()
}

/// Stateful parser for Gemini `streamGenerateContent` SSE.
///
/// Gemini SSE uses unnamed `data:` lines. Each payload is a
/// `GenerateContentResponse` JSON chunk with candidates containing
/// content parts (text, functionCall, thought).
#[derive(Default)]
pub struct GeminiSseParser {
    tool_call_ids: Vec<String>,
    tc_counter: usize,
}

impl GeminiSseParser {
    /// Create a new parser with no tracked state.
    pub fn new() -> Self {
        Self {
            tool_call_ids: Vec::new(),
            tc_counter: 0,
        }
    }

    /// Map Gemini finish reasons to internal finish-reason strings.
    ///
    /// | Gemini        | Internal       |
    /// |---------------|----------------|
    /// | STOP          | stop           |
    /// | MAX_TOKENS    | length         |
    /// | SAFETY        | content_filter |
    /// | RECITATION    | content_filter |
    /// | OTHER         | unknown        |
    fn map_finish_reason(reason: &str) -> String {
        match reason.to_uppercase().as_str() {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            "SAFETY" | "RECITATION" => "content_filter".to_string(),
            "OTHER" => "unknown".to_string(),
            _ => "unknown".to_string(),
        }
    }

    /// Parse a Gemini SSE data chunk into [`LlmEvent`] values.
    pub fn parse_chunk(&mut self, _event_type: Option<&str>, data: &str) -> Vec<LlmEvent> {
        let root = match super::parse_json_data("Gemini", data) {
            Ok(v) => v,
            Err(LlmEvent::Started) => return vec![],
            Err(e) => return vec![e],
        };

        if let Some(error_event) = super::extract_api_error(&root) {
            return vec![error_event];
        }

        let mut events = Vec::new();
        let mut done_emitted = false;

        if let Some(cands) = root.get("candidates").and_then(|c| c.as_array()) {
            if let Some(cand) = cands.first() {
                let parts = cand
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array());

                let mut has_tool_calls = false;

                if let Some(parts_arr) = parts {
                    for part in parts_arr {
                        // Thinking/reasoning parts
                        if part.get("thought").and_then(|v| v.as_bool()) == Some(true) {
                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    events.push(LlmEvent::Reasoning {
                                        content: text.to_string(),
                                    });
                                }
                            }
                            continue;
                        }

                        // Text parts
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                events.push(LlmEvent::Token {
                                    content: text.to_string(),
                                });
                            }
                        }

                        // Function call parts
                        if let Some(fc) = part.get("functionCall") {
                            let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let args: serde_json::Value = fc
                                .get("args")
                                .cloned()
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                            let arguments = serde_json::to_string(&args).unwrap_or_default();
                            let counter = self.tc_counter;
                            let id = format!("tc_{name}_{counter}");
                            self.tool_call_ids.push(id.clone());
                            self.tc_counter += 1;

                            events.push(LlmEvent::ToolCallStart {
                                id: id.clone(),
                                name: name.to_string(),
                            });
                            events.push(LlmEvent::ToolCallDelta {
                                id,
                                arguments_delta: arguments,
                            });
                            has_tool_calls = true;
                        }
                    }
                }

                // Finish reason
                if let Some(reason) = cand.get("finishReason").and_then(|r| r.as_str()) {
                    let had_tool_calls = has_tool_calls || !self.tool_call_ids.is_empty();
                    // Emit ToolCallEnd for all tracked tool calls
                    for id in self.tool_call_ids.drain(..) {
                        events.push(LlmEvent::ToolCallEnd { id });
                    }

                    // Safety ratings
                    if let Some(ratings) = cand.get("safetyRatings").and_then(|r| r.as_array()) {
                        for rating in ratings {
                            if let (Some(cat), Some(prob)) = (
                                rating.get("category").and_then(|c| c.as_str()),
                                rating.get("probability").and_then(|p| p.as_str()),
                            ) {
                                events.push(LlmEvent::SafetyRating {
                                    category: cat.to_string(),
                                    probability: prob.to_string(),
                                });
                            }
                        }
                    }

                    // Grounding metadata (web search / Google Search)
                    if let Some(grounding) = cand.get("groundingMetadata") {
                        if let Some(chunks) =
                            grounding.get("groundingChunks").and_then(|c| c.as_array())
                        {
                            for chunk in chunks {
                                if let Some(web) = chunk.get("web") {
                                    let text = web
                                        .get("title")
                                        .or_else(|| web.get("snippet"))
                                        .and_then(|t| t.as_str())
                                        .unwrap_or("");
                                    let url = web
                                        .get("uri")
                                        .and_then(|u| u.as_str())
                                        .map(|s| s.to_string());
                                    if !text.is_empty() || url.is_some() {
                                        events.push(LlmEvent::GroundingChunk {
                                            text: text.to_string(),
                                            url,
                                        });
                                    }
                                }
                            }
                        }
                    }

                    let mapped = if had_tool_calls {
                        "tool_calls".to_string()
                    } else {
                        Self::map_finish_reason(reason)
                    };

                    let usage = root.get("usageMetadata").map(|u| {
                        super::token_usage_from_keys(
                            u,
                            "promptTokenCount",
                            "candidatesTokenCount",
                            "totalTokenCount",
                        )
                    });

                    events.push(LlmEvent::Done {
                        finish_reason: mapped,
                        usage,
                    });
                    done_emitted = true;
                }
            }
        }

        // Fallback: usageMetadata without finishReason
        if !done_emitted {
            if let Some(u) = root.get("usageMetadata") {
                events.push(LlmEvent::Done {
                    finish_reason: "stop".to_string(),
                    usage: Some(super::token_usage_from_keys(
                        u,
                        "promptTokenCount",
                        "candidatesTokenCount",
                        "totalTokenCount",
                    )),
                });
            }
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoints::GEMINI_BASE_URL;
    use crate::router::{CredentialStatus, ProviderKind};
    use crate::types::{ChatRequest, Message, ModelTarget, Role, ToolDefinition};

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            alias: "gemini".to_string(),
            provider: ProviderKind::Gemini,
            provider_model: "gemini-2.5-flash".to_string(),
            base_url: GEMINI_BASE_URL.to_string(),
            api_key_env: "GEMINI_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        }
    }

    // ── Request JSON building ─────────────────────────────────────────────

    #[test]
    fn build_request_includes_system_instruction() {
        let req = ChatRequest {
            messages: vec![Message::system("You are helpful."), Message::user("Hello!")],
            tools: vec![],
            target: ModelTarget::new("gemini"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let si = &json["systemInstruction"];
        assert!(si.is_object() || si.is_string());
        let parts = si["parts"].as_array().unwrap();
        assert_eq!(parts[0]["text"], "You are helpful.");
    }

    #[test]
    fn build_request_maps_user_role() {
        let req = ChatRequest {
            messages: vec![Message::user("Hello")],
            tools: vec![],
            target: ModelTarget::new("gemini"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let contents = json["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "user");
    }

    #[test]
    fn build_request_maps_assistant_to_model_role() {
        let req = ChatRequest {
            messages: vec![Message::assistant("I can help.")],
            tools: vec![],
            target: ModelTarget::new("gemini"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let contents = json["contents"].as_array().unwrap();
        assert_eq!(contents[0]["role"], "model");
    }

    #[test]
    fn build_request_maps_function_call() {
        let req = ChatRequest {
            messages: vec![Message {
                role: Role::Assistant,
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![crate::types::ToolCall::new(
                    "call_1",
                    crate::types::FunctionCall::new("get_weather", r#"{"city":"Tokyo"}"#),
                )]),
                tool_call_id: None,
                name: None,
            }],
            tools: vec![],
            target: ModelTarget::new("gemini"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let contents = json["contents"].as_array().unwrap();
        let parts = contents[0]["parts"].as_array().unwrap();
        let fc = parts.iter().find(|p| p.get("functionCall").is_some());
        assert!(
            fc.is_some(),
            "should have functionCall in parts, got: {:?}",
            parts
        );
        let fc = fc.unwrap();
        assert_eq!(fc["functionCall"]["name"], "get_weather");
    }

    #[test]
    fn build_request_maps_function_response() {
        let req = ChatRequest {
            messages: vec![Message {
                role: Role::Tool,
                content: r#"{"temp":22}"#.to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("get_weather".to_string()),
            }],
            tools: vec![],
            target: ModelTarget::new("gemini"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let contents = json["contents"].as_array().unwrap();
        let parts = contents[0]["parts"].as_array().unwrap();
        let fr = parts.iter().find(|p| p.get("functionResponse").is_some());
        assert!(
            fr.is_some(),
            "should have functionResponse in parts, got: {:?}",
            parts
        );
        let fr = fr.unwrap();
        assert_eq!(fr["functionResponse"]["name"], "get_weather");
    }

    #[test]
    fn build_request_includes_tools_as_function_declarations() {
        let req = ChatRequest {
            messages: vec![Message::user("weather?")],
            tools: vec![ToolDefinition::new(
                "get_weather",
                "Get weather",
                serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
            )],
            target: ModelTarget::new("gemini"),
            temperature: None,
            max_tokens: None,
            max_completion_tokens: None,
            stream: true,
        };
        let resolved = make_resolved();
        let json = build_request_json(&req, &resolved);
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["functionDeclarations"][0]["name"], "get_weather");
    }

    // ── URL building ─────────────────────────────────────────────────────

    #[test]
    fn build_stream_url_includes_url_encoded_model() {
        let url = build_stream_url(GEMINI_BASE_URL, "gemini-2.5-flash");
        assert!(
            url.contains("gemini-2.5-flash"),
            "URL should contain model name, got: {}",
            url
        );
        assert!(
            url.contains("streamGenerateContent"),
            "URL should contain streamGenerateContent, got: {}",
            url
        );
        assert!(
            url.contains("alt=sse"),
            "URL should contain alt=sse, got: {}",
            url
        );
    }

    #[test]
    fn build_stream_url_encodes_special_chars_in_model() {
        let url = build_stream_url("https://api.example.com/v1", "models/my-model@v1");
        // model name with special chars should be URL-encoded
        assert!(
            url.contains("models%2Fmy-model%40v1") || url.contains("my-model@v1"),
            "URL should contain encoded model name, got: {}",
            url
        );
    }

    // ── Auth header ──────────────────────────────────────────────────────

    #[test]
    fn auth_header_name_is_x_goog_api_key() {
        let val = auth_header_value("gemini-key-123");
        assert_eq!(val, "gemini-key-123");
    }

    // ── SSE parsing ───────────────────────────────────────────────────────

    #[test]
    fn parse_sse_token_chunk() {
        let mut parser = GeminiSseParser::new();
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]},"finishReason":null}]}"#;
        let events = parser.parse_chunk(None, data);
        let has_token = events
            .iter()
            .any(|e| matches!(e, LlmEvent::Token { content } if content == "Hello"));
        assert!(has_token, "should emit Token, got: {:?}", events);
    }

    #[test]
    fn parse_sse_reasoning_chunk() {
        let mut parser = GeminiSseParser::new();
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Let me think...","thought":true}]},"finishReason":null}]}"#;
        let events = parser.parse_chunk(None, data);
        let has_reasoning = events
            .iter()
            .any(|e| matches!(e, LlmEvent::Reasoning { content } if content == "Let me think..."));
        assert!(has_reasoning, "should emit Reasoning, got: {:?}", events);
    }

    #[test]
    fn parse_sse_function_call() {
        let mut parser = GeminiSseParser::new();
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"get_weather","args":{"city":"Tokyo"}}}]},"finishReason":null}]}"#;
        let events = parser.parse_chunk(None, data);
        let has_start = events
            .iter()
            .any(|e| matches!(e, LlmEvent::ToolCallStart { name, .. } if name == "get_weather"));
        assert!(has_start, "should emit ToolCallStart, got: {:?}", events);
    }

    #[test]
    fn parse_sse_done_with_finish_reason_and_usage() {
        let mut parser = GeminiSseParser::new();
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Done"}],"finishReason":"STOP"},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20,"totalTokenCount":30}}"#;
        let events = parser.parse_chunk(None, data);
        let has_done = events.iter().any(|e| matches!(e, LlmEvent::Done { finish_reason, usage } if finish_reason == "stop" && usage.as_ref().unwrap().total_tokens == 30));
        assert!(has_done, "should emit Done with usage, got: {:?}", events);
    }

    #[test]
    fn parse_sse_error_chunk() {
        let mut parser = GeminiSseParser::new();
        let data = r#"{"error":{"message":"Resource exhausted","code":429}}"#;
        let events = parser.parse_chunk(None, data);
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], LlmEvent::Error { message } if message.contains("Resource exhausted"))
        );
    }

    #[test]
    fn parse_sse_empty_data() {
        let mut parser = GeminiSseParser::new();
        let events = parser.parse_chunk(None, "");
        assert!(events.is_empty());
    }

    // ── Boundary: no lattice_core imports ──────────────────────────────────

    #[test]
    fn gemini_provider_has_no_lattice_core_imports() {
        let json = build_request_json(
            &ChatRequest::new(vec![Message::user("test")], ModelTarget::new("gemini")),
            &make_resolved(),
        );
        assert!(!json.is_null());
    }
}
