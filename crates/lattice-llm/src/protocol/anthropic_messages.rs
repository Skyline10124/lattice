// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Messages 的请求构造与 SSE 解析。

use lattice_conversation::{Protocol, ReplayOrigin};
use std::collections::BTreeMap;

use crate::LlmError;
use crate::protocol::projection::{request_messages_to_wire, wire_usage};
use crate::reply::{
    PartDelta, PartKind, ProtocolEvent, ReplyProtocolError, StreamStop, StringPatch,
};
use crate::types::ChatRequest;

/// Messages 必须发送输出上限；元数据没有给出时使用此值。
const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_768;

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

/// 按模型名构造 Messages 流式请求。
///
/// `max_tokens` 必填。输出上限缺失或为 0 时用 [`DEFAULT_MAX_OUTPUT_TOKENS`]；
/// 有上下文窗口时再夹到该请求放得下的范围，避免提示与输出之和超出窗口后被拒绝；
/// 按下限估算的提示也已占满窗口时返回 [`LlmError::ContextWindowExceeded`]，不发送请求。
pub fn build_request_json(
    request: &ChatRequest,
    model: &str,
    output_limit: Option<u64>,
    context_limit: Option<u64>,
    origin: &ReplayOrigin,
) -> Result<serde_json::Value, LlmError> {
    let projected = request_messages_to_wire(origin, &request.messages)?;
    let max_tokens = fit_max_tokens(
        output_limit,
        context_limit,
        prompt_bytes(
            projected.system.as_deref(),
            &projected.messages,
            &request.tools,
        ),
    )
    .map_err(|exceeded| LlmError::ContextWindowExceeded {
        message: format!(
            "estimated prompt of at least {} tokens leaves no room for output in the {}-token context window of {model}",
            exceeded.prompt_tokens, exceeded.context
        ),
    })?;
    let mut body = serde_json::json!({
        "model": model,
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

    Ok(body)
}

/// 按下限估算的提示 token 数也已不小于上下文窗口。
struct ContextExceeded {
    prompt_tokens: u64,
    context: u64,
}

/// 上限估算已无剩余空间、下限估算还有空间时发送的输出上限。
///
/// 1,024 个 token 足以容纳一段简短回答或一次带参数的工具调用；再小，对端即使
/// 接受请求也多半在输出中途以 `max_tokens` 截断。它同时受下限估算剩余空间与
/// 模型输出上限约束，所以不会让下限估算下的提示与输出之和超出窗口。
const MIN_OUTPUT_TOKENS: u64 = 1_024;

/// 输出上限缺失或为 0 时回落到默认值，再按提示占用夹进上下文窗口。
///
/// 提示长度用两个估算，都按 UTF-8 字节数向上取整，都不是 provider 的分词结果：
///
/// - 上限估算：3 字节计 1 个 token。中日韩文字每字 3 字节、约 1 个 token，
///   基本准确；英文约 4 字节一个 token，多估约三分之一。用它夹 `max_tokens`，
///   宁可少给输出，也不让提示与输出之和超窗后被对端以 400 拒绝。
/// - 下限估算：4 字节计 1 个 token。只用它判断是否发送前拒绝：连它都放不下
///   任何输出时，提示几乎肯定超窗，返回错误。
///
/// 上限估算还有剩余空间时只用它，即使不足 [`MIN_OUTPUT_TOKENS`] 也不抬高：
/// 中日韩提示的上限估算基本准确，抬高会让请求超窗。只有上限估算已无空间、
/// 下限估算还有空间时，才改用 `MIN_OUTPUT_TOKENS` 与下限估算剩余空间中的较小者，
/// 由对端判断是否放得下。剩余空间从 1 降到 0 时输出上限会跳到下限，这是有意的：
/// 剩余空间为正说明提示按保守估算放得下，应该相信它。
fn fit_max_tokens(
    output_limit: Option<u64>,
    context_limit: Option<u64>,
    prompt_bytes: u64,
) -> Result<u64, ContextExceeded> {
    let requested = match output_limit {
        Some(0) | None => DEFAULT_MAX_OUTPUT_TOKENS,
        Some(limit) => limit,
    };
    let Some(context) = context_limit.filter(|limit| *limit > 0) else {
        return Ok(requested);
    };
    let lower = prompt_bytes.div_ceil(4);
    let lower_room = match context.checked_sub(lower) {
        Some(room) if room > 0 => room,
        _ => {
            return Err(ContextExceeded {
                prompt_tokens: lower,
                context,
            });
        }
    };
    let upper_room = context.saturating_sub(prompt_bytes.div_ceil(3));
    let room = if upper_room > 0 {
        upper_room
    } else {
        MIN_OUTPUT_TOKENS.min(lower_room)
    };
    Ok(requested.min(room))
}

fn prompt_bytes(
    system: Option<&str>,
    messages: &[serde_json::Value],
    tools: &[crate::types::ToolDefinition],
) -> u64 {
    let mut bytes = system.map(str::len).unwrap_or(0) as u64;
    bytes += messages.iter().map(json_string_bytes).sum::<u64>();
    for tool in tools {
        bytes += tool.name.len() as u64;
        bytes += tool.description.len() as u64;
        bytes += json_string_bytes(&tool.parameters);
    }
    bytes
}

fn json_string_bytes(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::String(text) => text.len() as u64,
        serde_json::Value::Array(items) => items.iter().map(json_string_bytes).sum(),
        serde_json::Value::Object(fields) => fields.values().map(json_string_bytes).sum(),
        _ => 0,
    }
}

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

struct AnthropicBlock {
    kind: PartKind,
    initial_arguments: bool,
    argument_deltas_started: bool,
}

#[derive(Default)]
pub struct AnthropicSseParser {
    blocks: BTreeMap<usize, AnthropicBlock>,
    input_tokens: Option<u64>,
    completed: bool,
}

impl AnthropicSseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse_chunk(&mut self, event_type: &str, data: &str) -> Vec<ProtocolEvent> {
        if self.completed {
            return Vec::new();
        }
        let root = match super::common::parse_json_data("Anthropic Messages", data) {
            Ok(Some(root)) => root,
            Ok(None) => return vec![],
            Err(error) => return vec![error],
        };
        let mut events = Vec::new();
        if event_type.starts_with("content_block_") {
            let Some(index) = root["index"]
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
            else {
                return vec![ProtocolEvent::error(
                    "malformed Anthropic Messages wire: content index is missing or too large",
                    false,
                )];
            };
            match event_type {
                "content_block_start" => {
                    let block = &root["content_block"];
                    let kind = match block["type"].as_str() {
                        Some("text") => PartKind::Text,
                        Some("thinking") => PartKind::Reasoning,
                        Some("tool_use") => PartKind::ToolCall,
                        other => {
                            return vec![ProtocolEvent::unsupported(
                                "anthropic-messages",
                                index,
                                other.unwrap_or("<missing>"),
                            )];
                        }
                    };
                    if self.blocks.contains_key(&index) {
                        return vec![ProtocolEvent::Failed(StreamStop::Protocol(
                            ReplyProtocolError::InvalidSequence {
                                index,
                                detail: "duplicate content block start",
                            },
                        ))];
                    }
                    events.push(ProtocolEvent::Part(PartDelta::Begin { index, kind }));
                    let mut state = AnthropicBlock {
                        kind,
                        initial_arguments: false,
                        argument_deltas_started: false,
                    };
                    match kind {
                        PartKind::Text => {
                            let text = block["text"].as_str().unwrap_or("");
                            events.push(ProtocolEvent::Part(PartDelta::Text {
                                index,
                                delta: text.into(),
                            }));
                        }
                        PartKind::Reasoning => {
                            let text = block["thinking"].as_str().unwrap_or("");
                            events.push(ProtocolEvent::Part(PartDelta::Reasoning {
                                index,
                                delta: text.into(),
                            }));
                            if let Some(signature) = block["signature"].as_str() {
                                events.push(ProtocolEvent::Part(PartDelta::ReplayState {
                                    index,
                                    patch: StringPatch::Set(signature.into()),
                                }));
                            }
                        }
                        PartKind::ToolCall => {
                            events.push(ProtocolEvent::Part(PartDelta::ToolMetadata {
                                index,
                                name: block["name"]
                                    .as_str()
                                    .map(|name| StringPatch::Set(name.into())),
                                provider_call_id: block["id"]
                                    .as_str()
                                    .map(|id| StringPatch::Set(id.into())),
                            }));
                            if let Some(input) = block.get("input").filter(|value| {
                                !value.as_object().is_some_and(serde_json::Map::is_empty)
                            }) {
                                state.initial_arguments = true;
                                events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                                    index,
                                    delta: input.to_string(),
                                }));
                            }
                        }
                    }
                    self.blocks.insert(index, state);
                }
                "content_block_delta" => {
                    let delta = &root["delta"];
                    let block_type = delta["type"].as_str().unwrap_or("<missing>");
                    let translated = match block_type {
                        "text_delta" => PartDelta::Text {
                            index,
                            delta: delta["text"].as_str().unwrap_or("").into(),
                        },
                        "thinking_delta" => PartDelta::Reasoning {
                            index,
                            delta: delta["thinking"].as_str().unwrap_or("").into(),
                        },
                        "signature_delta" => PartDelta::ReplayState {
                            index,
                            patch: StringPatch::Append(
                                delta["signature"].as_str().unwrap_or("").into(),
                            ),
                        },
                        "input_json_delta" => {
                            let Some(state) = self
                                .blocks
                                .get_mut(&index)
                                .filter(|state| state.kind == PartKind::ToolCall)
                            else {
                                return vec![ProtocolEvent::error(
                                    "malformed Anthropic Messages wire: argument delta has no Tool block",
                                    false,
                                )];
                            };
                            if state.initial_arguments {
                                return vec![ProtocolEvent::error(
                                    "malformed Anthropic Messages wire: Tool input is present both at start and in deltas",
                                    false,
                                )];
                            }
                            state.argument_deltas_started = true;
                            PartDelta::ToolArguments {
                                index,
                                delta: delta["partial_json"].as_str().unwrap_or("").into(),
                            }
                        }
                        // 已识别的可选展示元数据没有语义或回放要求。
                        "citation_delta" | "citations_delta" => return events,
                        other => {
                            return vec![ProtocolEvent::unsupported(
                                "anthropic-messages",
                                index,
                                other,
                            )];
                        }
                    };
                    events.push(ProtocolEvent::Part(translated));
                }
                "content_block_stop" => {
                    if let Some(state) = self.blocks.get(&index)
                        && state.kind == PartKind::ToolCall
                        && !state.initial_arguments
                        && !state.argument_deltas_started
                    {
                        events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                            index,
                            delta: "{}".into(),
                        }));
                    }
                    events.push(ProtocolEvent::Part(PartDelta::End { index }));
                }
                _ => {}
            }
            return events;
        }
        match event_type {
            "message_start" => {
                let message = &root["message"];
                self.input_tokens = message["usage"]["input_tokens"].as_u64();
                if message["usage"].get("output_tokens").is_some() {
                    match wire_usage(Protocol::Messages, message) {
                        Ok(Some(usage)) => events.push(ProtocolEvent::Usage(usage)),
                        Ok(None) => {}
                        Err(error) => events.push(ProtocolEvent::error(error.to_string(), false)),
                    }
                }
            }
            "message_delta" => {
                let usage_wire = serde_json::json!({"usage": {
                    "input_tokens": self.input_tokens,
                    "output_tokens": root["usage"].get("output_tokens"),
                }});
                match wire_usage(Protocol::Messages, &usage_wire) {
                    Ok(Some(usage)) => events.push(ProtocolEvent::Usage(usage)),
                    Ok(None) => {}
                    Err(error) => return vec![ProtocolEvent::error(error.to_string(), false)],
                }
                if let Some(stop_reason) = root["delta"]["stop_reason"].as_str() {
                    self.completed = true;
                    events.push(ProtocolEvent::Done {
                        finish_reason: map_stop_reason(stop_reason),
                    });
                }
            }
            "error" => events.push(
                super::common::extract_api_error(Protocol::Messages, &root).unwrap_or_else(|| {
                    ProtocolEvent::error("unknown Anthropic streaming error", false)
                }),
            ),
            "message_stop" | "ping" => {}
            _ => {}
        }
        events
    }
}
#[cfg(test)]
mod tests {
    use lattice_conversation::{
        BoundToolCall, MessagePart, MessageRole, MessageSemantic, NativeMessage, ToolCallValidation,
    };

    use super::*;
    use crate::protocol::projection::ProjectionError;
    use crate::test_reply::Parser;
    use crate::types::{ChatRequest, ModelTarget, ToolDefinition};
    use crate::{LlmEvent, ReplyOutcome, ReplyStop, StreamStop};

    #[test]
    fn unknown_blocks_and_deltas_fail_with_bounded_typed_protocol_errors() {
        for (event_type, payload) in [
            (
                "content_block_start",
                serde_json::json!({"index":9,"content_block":{"type":"redacted_thinking","data":"private-payload"}}),
            ),
            (
                "content_block_start",
                serde_json::json!({"index":9,"content_block":{"type":"unknown-block","data":"private-payload"}}),
            ),
            (
                "content_block_delta",
                serde_json::json!({"index":9,"delta":{"type":"unknown-delta","data":"private-payload"}}),
            ),
            (
                "content_block_delta",
                serde_json::json!({"index":9,"delta":{"type":"x".repeat(4096),"data":"private-payload"}}),
            ),
        ] {
            let mut parser = Parser::new(Protocol::Messages);
            let events = parser.parse_chunk(event_type, &payload.to_string());
            let [
                LlmEvent::Finished {
                    outcome:
                        ReplyOutcome::Interrupted {
                            reason:
                                ReplyStop::Stream(StreamStop::Protocol(
                                    ReplyProtocolError::UnsupportedContentBlock {
                                        protocol,
                                        index,
                                        block_type,
                                    },
                                )),
                        },
                    ..
                },
            ] = &events[..]
            else {
                panic!("typed protocol error: {events:?}");
            };
            assert_eq!(protocol, "anthropic-messages");
            assert_eq!(*index, 9);
            assert!(block_type.chars().count() <= 64);
            assert!(!format!("{events:?}").contains("private-payload"));
            assert!(
                parser
                    .parse_chunk(
                        "message_delta",
                        r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#
                    )
                    .is_empty()
            );
        }
        assert!(matches!(
            crate::test_reply::wire_to_native(
                Protocol::Messages,
                &serde_json::json!({"content":[{"type":"redacted_thinking","data":"private-payload"}]})
            ),
            Err(ProjectionError::Protocol(
                ReplyProtocolError::UnsupportedContentBlock { index: 0, .. }
            ))
        ));
    }

    #[test]
    fn thinking_signature_fragments_share_one_part_and_keep_the_next_signature_separate() {
        let mut parser = Parser::new(Protocol::Messages);
        start_message(&mut parser);
        for index in [0, 1] {
            parser.parse_chunk("content_block_start",&serde_json::json!({"index":index,"content_block":{"type":"thinking","thinking":"r","signature":""}}).to_string());
            for signature in ["first-", "second"] {
                parser.parse_chunk("content_block_delta",&serde_json::json!({"index":index,"delta":{"type":"signature_delta","signature":signature}}).to_string());
            }
            parser.parse_chunk(
                "content_block_stop",
                &serde_json::json!({"index":index}).to_string(),
            );
        }
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}"#,
        );
        let message = crate::test_reply::completed(&events);
        assert_eq!(message.parts.len(), 2);
        for part in &message.parts {
            assert_eq!(part.replay_state.as_ref().unwrap().as_str(), "first-second");
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

    fn start_message(parser: &mut Parser) {
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
            target: ModelTarget::new("anthropic", "claude-sonnet-4-6"),
            client_request_id: None,
        };
        let body = build_request_json(
            &request,
            "claude-sonnet-4-6",
            None,
            None,
            &crate::test_reply::replay_origin(Protocol::Messages),
        )
        .expect("projection");
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["system"], "First prompt.\n\nSecond prompt.");
        assert_eq!(body["messages"][0]["content"][0]["text"], "Hello");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 32_768);
    }

    #[test]
    fn parser_persists_signature_and_real_tool_id() {
        let mut parser = Parser::new(Protocol::Messages);
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
        parser.parse_chunk("content_block_stop", r#"{"index":0}"#);
        parser.parse_chunk("content_block_stop", r#"{"index":1}"#);
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}"#,
        );
        let message = events
            .iter()
            .find_map(|event| match event {
                LlmEvent::Finished {
                    outcome: ReplyOutcome::Complete { message },
                    ..
                } => Some(message.as_message()),
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
        let mut parser = Parser::new(Protocol::Messages);
        start_message(&mut parser);
        let started = parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","name":"observe__workspace","input":{}}}"#,
        );
        assert!(started.is_empty());
        assert!(parser.raw.iter().any(|event| matches!(event,
            ProtocolEvent::Part(PartDelta::ToolMetadata { provider_call_id: None, name: Some(StringPatch::Set(name)), .. }) if name == "observe__workspace"
        )));
        parser.parse_chunk("content_block_stop", r#"{"index":0}"#);
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        );
        let message = events
            .iter()
            .find_map(|event| match event {
                LlmEvent::Finished {
                    outcome: ReplyOutcome::Complete { message },
                    ..
                } => Some(message.as_message()),
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
        let mut parser = Parser::new(Protocol::Messages);
        start_message(&mut parser);
        parser.parse_chunk(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"observe__workspace","input":{}}}"#,
        );
        parser.parse_chunk(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"unterminated"}}"#,
        );
        parser.parse_chunk("content_block_stop", r#"{"index":0}"#);
        let events = parser.parse_chunk(
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Rejected {
                    rejection: lattice_conversation::AttemptRejection::InvalidToolCall {
                        validation: ToolCallValidation::MalformedArguments { call_ordinal: 0 },
                    }
                },
                ..
            }
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Complete { .. },
                ..
            }
        )));

        let mut parser = Parser::new(Protocol::Messages);
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
            LlmEvent::Finished { outcome: ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::Provider(failure)) }, .. } if !failure.retryable && failure.message.contains("both at start and in deltas")
        )));
    }
}
