// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Anthropic provider: request JSON construction, SSE parsing,
//! and auth headers for the Messages API.
//!
//! Uses Anthropic's native message format with `x-api-key` auth,
//! `anthropic-version: 2023-06-01` header, system message extraction,
//! and content-block-based SSE parsing.

use lattice_conversation::{ProviderOrigin, ToolCallValidation};
use std::collections::BTreeMap;

use crate::provider_projection::{ProjectionError, request_messages_to_wire, wire_usage};
use crate::router::ResolvedModel;
use crate::types::{ChatRequest, LlmEvent};

/// Output ceiling applied when the caller names none.
///
/// Anthropic requires `max_tokens`, so some default is unavoidable, and the
/// production `ChatRequest` never carries one (`lattice-agent` leaves it
/// `None`) — this value alone decides where every Claude reply is cut. The
/// previous 4096 silently truncated long replies into `finish_reason:
/// "length"`, which the agent loop does not continue from.
///
/// Safe as a flat constant because `ModelRouter` maps exactly three closed
/// aliases to this provider (`opus`/`sonnet`/`haiku`, all Claude 4.x); any
/// other name is `UnknownModel` and an arbitrary model string can only reach
/// the OpenAI-compatible provider. Per-model ceilings need a capability table
/// that tracks provider releases, which is deliberately out of scope (#100).
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 32_768;

/// Maps Anthropic stop reasons to OpenAI-style finish reasons.
const STOP_REASON_MAP: &[(&str, &str)] = &[
    ("end_turn", "stop"),
    ("tool_use", "tool_calls"),
    ("max_tokens", "length"),
    ("stop_sequence", "stop"),
    ("error", "error"),
];

fn map_stop_reason(reason: &str) -> String {
    STOP_REASON_MAP
        .iter()
        .find(|(k, _)| *k == reason)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| reason.to_string())
}

/// Build the JSON body for an Anthropic Messages API request.
pub fn build_request_json(
    request: &ChatRequest,
    resolved: &ResolvedModel,
) -> Result<serde_json::Value, ProjectionError> {
    let projected = request_messages_to_wire(ProviderOrigin::Anthropic, &request.messages)?;
    let max_tokens = request.max_tokens.unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
    let mut body = serde_json::json!({
        "model": resolved.provider_model,
        "messages": projected.messages,
        "max_tokens": max_tokens,
        "stream": true,
    });

    if let Some(system) = projected.system {
        body["system"] = serde_json::Value::String(system);
    }

    if !request.tools.is_empty() {
        let tools: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect();
        body["tools"] = serde_json::Value::Array(tools);
    }

    if let Some(temperature) = request.temperature {
        body["temperature"] = serde_json::json!(temperature);
    }

    Ok(body)
}

/// Build the Anthropic auth header value (just the API key itself).
pub fn auth_header_value(api_key: &str) -> String {
    api_key.to_string()
}

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

enum AnthropicBlock {
    Text(String),
    Thinking {
        text: String,
        signature: String,
    },
    Tool {
        provider_call_id: Option<String>,
        name: String,
        arguments: Option<String>,
        argument_deltas_started: bool,
    },
}

/// Stateful parser for Anthropic message-streaming SSE.
#[derive(Default)]
pub struct AnthropicSseParser {
    blocks: BTreeMap<u32, AnthropicBlock>,
    input_tokens: Option<u64>,
    completed: bool,
}

impl AnthropicSseParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn complete_message(
        &mut self,
    ) -> Result<
        lattice_conversation::NativeMessage<lattice_conversation::UnboundToolCall>,
        ProjectionError,
    > {
        let mut tool_ordinal = 0;
        let blocks = std::mem::take(&mut self.blocks)
            .into_values()
            .map(|block| -> Result<serde_json::Value, ProjectionError> {
                Ok(match block {
                    AnthropicBlock::Text(text) => {
                        serde_json::json!({"type": "text", "text": text})
                    }
                    AnthropicBlock::Thinking { text, signature } => serde_json::json!({
                        "type": "thinking",
                        "thinking": text,
                        "signature": signature,
                    }),
                    AnthropicBlock::Tool {
                        provider_call_id,
                        name,
                        arguments,
                        ..
                    } => {
                        let call_ordinal = tool_ordinal;
                        tool_ordinal += 1;
                        let input = match arguments {
                            None => serde_json::json!({}),
                            Some(arguments) => serde_json::from_str(&arguments).map_err(|_| {
                                ProjectionError::InvalidToolCall(
                                    ToolCallValidation::MalformedArguments { call_ordinal },
                                )
                            })?,
                        };
                        let mut wire = serde_json::json!({
                            "type": "tool_use",
                            "name": name,
                            "input": input,
                        });
                        if let Some(id) = provider_call_id {
                            wire["id"] = serde_json::json!(id);
                        }
                        wire
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        crate::provider_projection::wire_to_native(
            ProviderOrigin::Anthropic,
            &serde_json::json!({"role": "assistant", "content": blocks}),
        )
    }

    pub fn parse_chunk(&mut self, event_type: &str, data: &str) -> Vec<LlmEvent> {
        if self.completed {
            return Vec::new();
        }
        let root = match super::parse_json_data("Anthropic", data) {
            Ok(Some(v)) => v,
            Ok(None) => return vec![],
            Err(e) => return vec![e],
        };

        match event_type {
            "message_start" => {
                if let Some(msg) = root.get("message") {
                    self.input_tokens = msg["usage"]["input_tokens"].as_u64();
                }
                vec![]
            }
            "content_block_start" => {
                let index = root["index"].as_u64().unwrap_or(0) as u32;
                let block = &root["content_block"];
                match block["type"].as_str() {
                    Some("text") => {
                        let text = block["text"].as_str().unwrap_or("").to_string();
                        self.blocks
                            .insert(index, AnthropicBlock::Text(text.clone()));
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Token { content: text }]
                        }
                    }
                    Some("thinking") => {
                        let text = block["thinking"].as_str().unwrap_or("").to_string();
                        let signature = block["signature"].as_str().unwrap_or("").to_string();
                        self.blocks.insert(
                            index,
                            AnthropicBlock::Thinking {
                                text: text.clone(),
                                signature,
                            },
                        );
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Reasoning { content: text }]
                        }
                    }
                    Some("tool_use") => {
                        let provider_call_id = block["id"].as_str().map(str::to_string);
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        let arguments = block.get("input").and_then(|input| {
                            if input.as_object().is_some_and(serde_json::Map::is_empty) {
                                None
                            } else {
                                Some(input.to_string())
                            }
                        });
                        self.blocks.insert(
                            index,
                            AnthropicBlock::Tool {
                                provider_call_id: provider_call_id.clone(),
                                name: name.clone(),
                                arguments,
                                argument_deltas_started: false,
                            },
                        );
                        vec![LlmEvent::ToolCallStart {
                            index: index as usize,
                            provider_call_id,
                            name,
                        }]
                    }
                    _ => vec![],
                }
            }
            "content_block_delta" => {
                let index = root["index"].as_u64().unwrap_or(0) as u32;
                let delta = &root["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or("");
                        if let Some(AnthropicBlock::Text(content)) = self.blocks.get_mut(&index) {
                            content.push_str(text);
                        }
                        if text.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Token {
                                content: text.to_string(),
                            }]
                        }
                    }
                    Some("thinking_delta") => {
                        let thinking = delta["thinking"].as_str().unwrap_or("");
                        if let Some(AnthropicBlock::Thinking { text, .. }) =
                            self.blocks.get_mut(&index)
                        {
                            text.push_str(thinking);
                        }
                        if thinking.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Reasoning {
                                content: thinking.to_string(),
                            }]
                        }
                    }
                    Some("input_json_delta") => {
                        let partial = delta["partial_json"].as_str().unwrap_or("");
                        let Some(AnthropicBlock::Tool {
                            arguments,
                            argument_deltas_started,
                            ..
                        }) = self.blocks.get_mut(&index)
                        else {
                            return vec![LlmEvent::error(
                                "malformed Anthropic wire: argument delta has no Tool block",
                                false,
                            )];
                        };
                        if !*argument_deltas_started {
                            if arguments.is_some() {
                                return vec![LlmEvent::error(
                                    "malformed Anthropic wire: Tool input is present both at start and in deltas",
                                    false,
                                )];
                            }
                            *arguments = Some(String::new());
                            *argument_deltas_started = true;
                        }
                        arguments
                            .as_mut()
                            .expect("argument delta state initialized")
                            .push_str(partial);
                        if partial.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::ToolCallDelta {
                                index: index as usize,
                                arguments_delta: partial.to_string(),
                            }]
                        }
                    }
                    Some("citation_delta") => {
                        let cite_text = delta["cite_text"].as_str().unwrap_or("");
                        if cite_text.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Citation {
                                index: index as usize,
                                cite_text: cite_text.to_string(),
                            }]
                        }
                    }
                    Some("signature_delta") => {
                        let signature = delta["signature"].as_str().unwrap_or("");
                        if let Some(AnthropicBlock::Thinking {
                            signature: stored, ..
                        }) = self.blocks.get_mut(&index)
                        {
                            stored.push_str(signature);
                        }
                        if signature.is_empty() {
                            vec![]
                        } else {
                            vec![LlmEvent::Signature {
                                index: index as usize,
                                signature: signature.to_string(),
                            }]
                        }
                    }
                    _ => vec![],
                }
            }
            "content_block_stop" => {
                let index = root["index"].as_u64().unwrap_or(0) as u32;
                if matches!(self.blocks.get(&index), Some(AnthropicBlock::Tool { .. })) {
                    vec![LlmEvent::ToolCallEnd {
                        index: index as usize,
                    }]
                } else {
                    vec![]
                }
            }
            "message_delta" => {
                self.completed = true;
                let stop_reason = root["delta"]["stop_reason"].as_str().unwrap_or("end_turn");
                let finish_reason = map_stop_reason(stop_reason);
                let usage_wire = serde_json::json!({
                    "usage": {
                        "input_tokens": self.input_tokens,
                        "output_tokens": root["usage"].get("output_tokens"),
                    }
                });
                let usage = match wire_usage(ProviderOrigin::Anthropic, &usage_wire) {
                    Ok(usage) => usage,
                    Err(error) => {
                        return vec![LlmEvent::error(error.to_string(), false)];
                    }
                };
                let completion = super::projection_result_event(self.complete_message());
                if matches!(
                    completion,
                    LlmEvent::Error { .. } | LlmEvent::OutputLimit { .. }
                ) {
                    vec![completion]
                } else {
                    vec![
                        completion,
                        LlmEvent::Done {
                            finish_reason,
                            usage,
                        },
                    ]
                }
            }
            "message_stop" | "ping" => vec![],
            "error" => vec![
                super::extract_api_error(ProviderOrigin::Anthropic, &root)
                    .unwrap_or_else(|| LlmEvent::error("unknown Anthropic streaming error", false)),
            ],
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use lattice_conversation::{
        BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage,
    };

    use super::*;
    use crate::endpoints::ANTHROPIC_BASE_URL;
    use crate::router::{CredentialStatus, ProviderKind};
    use crate::types::{ChatRequest, ModelTarget, ToolDefinition};

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            alias: "sonnet".to_string(),
            provider: ProviderKind::Anthropic,
            provider_model: "claude-sonnet-4-6".to_string(),
            base_url: ANTHROPIC_BASE_URL.to_string(),
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
            credential_status: CredentialStatus::Present,
        }
    }

    fn text(role: MessageRole, content: &str) -> lattice_conversation::ConversationItem {
        lattice_conversation::ConversationItem::Message(NativeMessage::<BoundToolCall>::new(
            role,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: content.into(),
            })],
        ))
    }

    fn start_message(parser: &mut AnthropicSseParser) {
        assert!(
            parser
                .parse_chunk(
                    "message_start",
                    r#"{"message":{"usage":{"input_tokens":1}}}"#,
                )
                .is_empty()
        );
    }

    #[test]
    fn request_envelope_uses_typed_projection() {
        let request = ChatRequest {
            messages: vec![
                text(MessageRole::System, "First prompt."),
                text(MessageRole::System, "Second prompt."),
                text(MessageRole::User, "Hello"),
            ],
            tools: vec![ToolDefinition::new(
                "get_weather",
                "Get weather",
                serde_json::json!({"type": "object"}),
            )],
            target: ModelTarget::new("sonnet"),
            temperature: Some(0.2),
            max_tokens: None,
            max_completion_tokens: None,
            client_request_id: None,
        };
        let body = build_request_json(&request, &make_resolved()).expect("projection");
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["system"], "First prompt.\n\nSecond prompt.");
        assert_eq!(body["messages"][0]["content"][0]["text"], "Hello");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["stream"], true);
        assert!(body["max_tokens"].as_u64().unwrap() > 4096);
    }

    #[test]
    fn parser_persists_signature_and_real_tool_id() {
        let mut parser = AnthropicSseParser::new();
        start_message(&mut parser);
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"thinking_delta","thinking":"reason"}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"signature_delta","signature":"sig"}}"#,
        );
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"observe__workspace","input":{}}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        );
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#,
        );
        let message = events
            .iter()
            .find_map(|event| match event {
                LlmEvent::MessageComplete { message } => Some(message),
                _ => None,
            })
            .expect("complete message");
        assert_eq!(
            message.parts[0].replay_state.as_ref().unwrap().as_str(),
            "sig"
        );
        assert_eq!(
            message
                .tool_calls()
                .next()
                .expect("ToolCall")
                .1
                .provider_call_id
                .as_deref(),
            Some("toolu_1")
        );
    }

    #[test]
    fn parser_preserves_missing_tool_id() {
        let mut parser = AnthropicSseParser::new();
        start_message(&mut parser);
        let started = parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","name":"observe__workspace","input":{}}}"#,
        );
        assert!(started.iter().any(|event| matches!(
            event,
            LlmEvent::ToolCallStart {
                provider_call_id: None,
                ..
            }
        )));
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        );
        let message = events
            .iter()
            .find_map(|event| match event {
                LlmEvent::MessageComplete { message } => Some(message),
                _ => None,
            })
            .expect("complete message");
        assert_eq!(
            message
                .tool_calls()
                .next()
                .expect("ToolCall")
                .1
                .provider_call_id,
            None
        );
        assert_eq!(
            message.tool_calls().next().expect("ToolCall").1.arguments,
            serde_json::json!({}),
            "no argument delta preserves the provider's legal empty input object",
        );
        assert!(
            parser
                .parse_chunk(
                    "message_delta",
                    r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
                )
                .is_empty()
        );
    }

    #[test]
    fn malformed_argument_delta_rejects_the_whole_message() {
        let mut parser = AnthropicSseParser::new();
        start_message(&mut parser);
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"observe__workspace","input":{}}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"unterminated"}}"#,
        );
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::MessageRejected {
                rejection: lattice_conversation::AttemptRejection::InvalidToolCall {
                    validation: ToolCallValidation::MalformedArguments { call_ordinal: 0 },
                },
            }
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, LlmEvent::MessageComplete { .. }))
        );

        let mut parser = AnthropicSseParser::new();
        start_message(&mut parser);
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"observe__workspace","input":{"path":"already complete"}}}"#,
        );
        let events = parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Error { failure } if !failure.retryable && failure.message.contains("both at start and in deltas")
        )));
    }
}
