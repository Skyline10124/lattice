// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! generateContent 的请求构造与 SSE 解析。

use lattice_conversation::{Protocol, ReplayOrigin};

use crate::protocol::projection::{ProjectionError, request_messages_to_wire};
use crate::reply::{PartDelta, PartKind, ProtocolEvent, StringPatch};
use crate::types::ChatRequest;

/// 构造 generateContent 请求；模型名由端点路径指定。
pub fn build_request_json(
    request: &ChatRequest,
    origin: &ReplayOrigin,
) -> Result<serde_json::Value, ProjectionError> {
    let projected = request_messages_to_wire(origin, &request.messages)?;
    let mut body = serde_json::json!({
        "contents": projected.messages,
    });

    if let Some(system) = projected.system {
        body["systemInstruction"] = serde_json::json!({
            "parts": [{"text": system}]
        });
    }

    if !request.tools.is_empty() {
        let function_declarations: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool| {
                // `parameters` 只接受 OpenAPI 子集，会拒绝工具 schema 用到的
                // `additionalProperties`、`const`、`contains` 等关键字；
                // `parametersJsonSchema` 接受完整 JSON Schema。
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parametersJsonSchema": tool.parameters,
                })
            })
            .collect();
        body["tools"] = serde_json::json!([{
            "functionDeclarations": function_declarations,
        }]);
    }

    Ok(body)
}

/// Build the full Gemini streaming endpoint URL.
pub fn build_stream_url(base_url: &str, provider_model: &str) -> String {
    let encoded = urlencoding::encode(provider_model);
    format!(
        "{}/models/{}:streamGenerateContent?alt=sse",
        base_url.trim_end_matches('/'),
        encoded
    )
}

/// Part 的字段名标明其内容类型；诊断不携带字段值或回放元数据。
pub(super) fn unsupported_part_type(part: &serde_json::Value) -> &str {
    part.as_object()
        .and_then(|fields| {
            fields
                .keys()
                .find(|key| !matches!(key.as_str(), "thought" | "thoughtSignature"))
        })
        .map_or("<missing>", String::as_str)
}

#[derive(Default)]
pub struct GeminiSseParser {
    usage: super::usage::UsageSnapshot,
    current: Option<(usize, PartKind, bool)>,
    next_index: usize,
    had_tool_calls: bool,
    completed: bool,
}

impl GeminiSseParser {
    pub fn new() -> Self {
        Self::default()
    }
    fn close_text(&mut self, events: &mut Vec<ProtocolEvent>) {
        if let Some((index, _, _)) = self.current.take() {
            events.push(ProtocolEvent::Part(PartDelta::End { index }));
        }
    }
    fn map_finish_reason(reason: &str) -> String {
        match reason.to_uppercase().as_str() {
            "STOP" => "stop",
            "MAX_TOKENS" => "length",
            "SAFETY" | "RECITATION" => "content_filter",
            _ => "unknown",
        }
        .into()
    }
    pub fn parse_chunk(&mut self, _event_type: Option<&str>, data: &str) -> Vec<ProtocolEvent> {
        if self.completed {
            return vec![];
        }
        let root = match super::common::parse_json_data("generateContent", data) {
            Ok(Some(root)) => root,
            Ok(None) => return vec![],
            Err(error) => return vec![error],
        };
        if let Some(error) = super::common::extract_api_error(Protocol::GenerateContent, &root) {
            return vec![error];
        }
        let mut events = Vec::new();
        if let Some(candidate) = root["candidates"]
            .as_array()
            .and_then(|candidates| candidates.first())
        {
            if let Some(parts) = candidate["content"]["parts"].as_array() {
                for (part_index, part) in parts.iter().enumerate() {
                    if part.get("text").is_some() && part.get("functionCall").is_some() {
                        return vec![ProtocolEvent::error(
                            "malformed generateContent wire: one part contains text and functionCall",
                            false,
                        )];
                    }
                    if part.get("text").is_none() && part.get("functionCall").is_none() {
                        return vec![ProtocolEvent::unsupported(
                            "generate-content",
                            part_index,
                            unsupported_part_type(part),
                        )];
                    }
                    let signature = match part.get("thoughtSignature") {
                        Some(serde_json::Value::String(signature)) => Some(signature.as_str()),
                        Some(serde_json::Value::Null) | None => None,
                        _ => {
                            return vec![ProtocolEvent::error(
                                "malformed generateContent wire: thoughtSignature is not a string",
                                false,
                            )];
                        }
                    };
                    if let Some(function) = part.get("functionCall") {
                        self.close_text(&mut events);
                        let index = self.next_index;
                        self.next_index += 1;
                        self.had_tool_calls = true;
                        events.push(ProtocolEvent::Part(PartDelta::Begin {
                            index,
                            kind: PartKind::ToolCall,
                        }));
                        let Some(name) = function["name"].as_str() else {
                            return vec![ProtocolEvent::error(
                                "malformed generateContent wire: functionCall missing name",
                                false,
                            )];
                        };
                        let provider_call_id = match function.get("id") {
                            Some(serde_json::Value::String(id)) => {
                                Some(StringPatch::Set(id.clone()))
                            }
                            Some(serde_json::Value::Null) | None => None,
                            _ => {
                                return vec![ProtocolEvent::error(
                                    "malformed generateContent wire: functionCall id is not a string",
                                    false,
                                )];
                            }
                        };
                        events.push(ProtocolEvent::Part(PartDelta::ToolMetadata {
                            index,
                            name: Some(StringPatch::Set(name.into())),
                            provider_call_id,
                        }));
                        events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                            index,
                            delta: function
                                .get("args")
                                .map_or_else(|| "{}".into(), ToString::to_string),
                        }));
                        if let Some(signature) = signature {
                            events.push(ProtocolEvent::Part(PartDelta::ReplayState {
                                index,
                                patch: StringPatch::Set(signature.into()),
                            }));
                        }
                        events.push(ProtocolEvent::Part(PartDelta::End { index }));
                    } else if let Some(text) = part["text"].as_str() {
                        let kind = if part["thought"].as_bool() == Some(true) {
                            PartKind::Reasoning
                        } else {
                            PartKind::Text
                        };
                        let index = if let Some((index, _, signed)) =
                            self.current.filter(|(_, previous_kind, signed)| {
                                *previous_kind == kind
                                    && !(signature.is_some() && (text.is_empty() || *signed))
                            }) {
                            self.current = Some((index, kind, signed || signature.is_some()));
                            index
                        } else {
                            self.close_text(&mut events);
                            let index = self.next_index;
                            self.next_index += 1;
                            self.current = Some((index, kind, signature.is_some()));
                            events.push(ProtocolEvent::Part(PartDelta::Begin { index, kind }));
                            index
                        };
                        events.push(ProtocolEvent::Part(if kind == PartKind::Reasoning {
                            PartDelta::Reasoning {
                                index,
                                delta: text.into(),
                            }
                        } else {
                            PartDelta::Text {
                                index,
                                delta: text.into(),
                            }
                        }));
                        if let Some(signature) = signature {
                            events.push(ProtocolEvent::Part(PartDelta::ReplayState {
                                index,
                                patch: StringPatch::Set(signature.into()),
                            }));
                        }
                    } else {
                        return vec![ProtocolEvent::error(
                            "malformed generateContent wire: part has neither text nor functionCall",
                            false,
                        )];
                    }
                }
            }
            if let Some(reason) = candidate["finishReason"].as_str() {
                self.close_text(&mut events);
                self.completed = true;
                match self
                    .usage
                    .update(Protocol::GenerateContent, &root)
                    .and_then(|_| self.usage.finish(Protocol::GenerateContent))
                {
                    Ok(Some(usage)) => events.push(ProtocolEvent::Usage(usage)),
                    Ok(None) => {}
                    Err(error) => {
                        events.push(ProtocolEvent::error(error.to_string(), false));
                        return events;
                    }
                }
                events.push(ProtocolEvent::Done {
                    finish_reason: if self.had_tool_calls {
                        "tool_calls".into()
                    } else {
                        Self::map_finish_reason(reason)
                    },
                });
                return events;
            }
        }
        match self.usage.update(Protocol::GenerateContent, &root) {
            Ok(Some(usage)) => events.push(ProtocolEvent::Usage(usage)),
            Ok(None) => {}
            Err(error) => events.push(ProtocolEvent::error(error.to_string(), false)),
        }
        events
    }
}
#[cfg(test)]
mod tests {
    use futures::{StreamExt, stream};
    use lattice_conversation::MessageSemantic;

    use super::*;
    use crate::test_reply::Parser;
    use crate::{LlmEvent, ReplyOutcome, ReplyStop, StreamStop};

    #[tokio::test]
    async fn stream_preserves_signed_empty_text_and_tool_call_positions() {
        let wire: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/projection/gemini_thought_signature.json"
        ))
        .unwrap();
        let parts = wire["candidates"][0]["content"]["parts"]
            .as_array()
            .unwrap();
        let mut frames: Vec<_> = parts
            .iter()
            .map(|part| {
                serde_json::json!({"candidates":[{"content":{"role":"model","parts":[part]}}]})
                    .to_string()
            })
            .collect();
        frames.push(r#"{"candidates":[{"finishReason":"STOP"}]}"#.into());
        let mut parser = GeminiSseParser::new();
        let input = stream::iter(frames)
            .flat_map(move |frame| stream::iter(parser.parse_chunk(None, &frame)));
        let events = crate::reply_stream::assemble_reply_stream(
            Some(crate::test_reply::replay_origin(Protocol::GenerateContent)),
            Box::pin(input),
            crate::StreamControl::default(),
        )
        .collect::<Vec<_>>()
        .await;
        assert!(
            matches!(events.as_slice(), [LlmEvent::Started, LlmEvent::Delta(crate::LiveDelta::Reasoning(text)), LlmEvent::Finished { .. }]
            if text == "Plan: check weather, then time.")
        );
        let message = crate::test_reply::completed(&events);
        assert_eq!(message.parts.len(), 3);
        for (part, original) in message.parts.iter().zip(parts) {
            assert_eq!(
                part.replay_state.as_ref().unwrap().as_str(),
                original["thoughtSignature"].as_str().unwrap()
            );
        }
        assert!(
            matches!(&message.parts[0].semantic, MessageSemantic::Reasoning { text } if text == "Plan: check weather, then time.")
        );
        assert!(
            matches!(&message.parts[1].semantic, MessageSemantic::Text { text } if text.is_empty())
        );
        let calls: Vec<_> = message.tool_calls().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, 2);
        assert_eq!(calls[0].1.name, "get_weather");
        assert_eq!(
            calls[0].1.provider_call_id.as_deref(),
            Some("fc-2026-08-13-001")
        );
        assert_eq!(
            calls[0].1.arguments,
            serde_json::json!({"city":"Tokyo","unit":"celsius"})
        );
    }

    #[test]
    fn unknown_parts_fail_typed_in_stream_and_complete_wire() {
        for (kind, expected) in [
            ("inlineData".into(), "inlineData".into()),
            ("future".repeat(20), "future".repeat(10) + "futu"),
        ] {
            let mut unknown = serde_json::Map::new();
            unknown.insert(kind, serde_json::json!({"data":"private-payload"}));
            let wire = serde_json::json!({"candidates":[{"content":{"role":"model","parts":[
                {"text":"accepted"}, {"text":" before"}, unknown
            ]},"finishReason":"STOP"}]});
            let expected_error = crate::ReplyProtocolError::UnsupportedContentBlock {
                protocol: "generate-content".into(),
                index: 2,
                block_type: expected,
            };
            let mut parser = Parser::new(Protocol::GenerateContent);
            let events = parser.parse_chunk(None, &wire.to_string());
            assert!(matches!(&events[..], [LlmEvent::Finished {
                outcome: ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::Protocol(error)) }, ..
            }] if error == &expected_error));
            assert!(
                parser
                    .parse_chunk(None, r#"{"candidates":[{"finishReason":"STOP"}]}"#)
                    .is_empty(),
                "no complete message after the protocol error"
            );
            assert!(!format!("{events:?}").contains("private-payload"));
            assert_eq!(
                crate::test_reply::wire_to_native(Protocol::GenerateContent, &wire),
                Err(ProjectionError::Protocol(expected_error)),
            );
        }
    }

    #[test]
    fn parser_preserves_thought_state_and_optional_ids_without_synthesis() {
        for (id_field, expected_id) in [(",\"id\":\"real-id\"", Some("real-id")), ("", None)] {
            let mut parser = Parser::new(Protocol::GenerateContent);
            let chunk = format!(
                r#"{{"candidates":[{{"content":{{"role":"model","parts":[{{"text":"reason","thought":true,"thoughtSignature":"sig"}},{{"functionCall":{{"name":"observe__workspace","args":{{}}{id_field}}}}}]}},"finishReason":"STOP"}}]}}"#
            );
            let events = parser.parse_chunk(None, &chunk);
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
                expected_id
            );
            assert!(!serde_json::to_string(message).unwrap().contains("tc_"));
        }
    }

    #[test]
    fn parser_completes_usage() {
        let mut parser = Parser::new(Protocol::GenerateContent);
        let events = parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Done"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":20,"totalTokenCount":30}}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Complete { .. },
                ..
            }
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished { report: crate::AttemptReport { usage: Some(usage), .. }, .. } if usage.context_tokens() == Some(30)
        )));
        assert!(parser
            .parse_chunk(
                None,
                r#"{"candidates":[{"content":{"role":"model","parts":[]},"finishReason":"STOP"}]}"#,
            )
            .is_empty());
    }

    #[test]
    fn parser_accumulates_multichunk_reasoning_text_and_signature() {
        let mut parser = Parser::new(Protocol::GenerateContent);
        for chunk in [
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"rea","thought":true}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"son","thought":true,"thoughtSignature":"sig"}]}}]}"#,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"hel"}]}}]}"#,
        ] {
            assert!(
                parser
                    .parse_chunk(None, chunk)
                    .iter()
                    .all(|event| !matches!(
                        event,
                        LlmEvent::Finished {
                            outcome: ReplyOutcome::Complete { .. },
                            ..
                        }
                    ))
            );
        }
        let events = parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"lo"}]},"finishReason":"STOP"}]}"#,
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
        assert_eq!(message.parts.len(), 2);
        assert!(matches!(
            &message.parts[0].semantic,
            MessageSemantic::Reasoning { text } if text == "reason"
        ));
        assert_eq!(
            message.parts[0].replay_state.as_ref().unwrap().as_str(),
            "sig"
        );
        assert!(matches!(
            &message.parts[1].semantic,
            MessageSemantic::Text { text } if text == "hello"
        ));
    }

    #[test]
    fn parser_preserves_distinct_signatures_and_rejects_union_parts() {
        let mut parser = Parser::new(Protocol::GenerateContent);
        parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"a","thought":true,"thoughtSignature":"sig-a"}]}}]}"#,
        );
        let events = parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"b","thought":true,"thoughtSignature":"sig-b"}]},"finishReason":"STOP"}]}"#,
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
        assert_eq!(message.parts.len(), 2);
        assert_eq!(
            message.parts[0].replay_state.as_ref().unwrap().as_str(),
            "sig-a"
        );
        assert_eq!(
            message.parts[1].replay_state.as_ref().unwrap().as_str(),
            "sig-b"
        );

        let mut parser = Parser::new(Protocol::GenerateContent);
        let events = parser.parse_chunk(
            None,
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"x","functionCall":{"name":"observe__workspace","args":{}}}]}}]}"#,
        );
        assert!(events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished { outcome: ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::Provider(failure)) }, .. } if !failure.retryable && failure.message.contains("text and functionCall")
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, LlmEvent::Delta(_)))
        );
    }
}
