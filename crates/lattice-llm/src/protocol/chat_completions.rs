// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Chat Completions 的请求构造与 SSE 解析。

use std::collections::BTreeMap;

use lattice_conversation::{Protocol, ReplayOrigin};

use super::{EffortParam, ThinkingBudget, ThinkingToggle};
use crate::protocol::ChatCompletionsFeatures;
use crate::protocol::projection::{ProjectionError, request_messages_with_replay};
use crate::reasoning::ReasoningPlan;
use crate::reply::{PartDelta, PartKind, ProtocolEvent, StringPatch};
use crate::types::ChatRequest;
use lattice_conversation::Effort;

/// 按模型名构造流式请求，并要求服务端报告用量。
/// 响应路径只解析 SSE、不解码非流式 JSON，因此固定发送 stream: true。
pub fn build_request_json(
    request: &ChatRequest,
    model: &str,
    features: ChatCompletionsFeatures,
    reasoning: Option<ReasoningPlan>,
    origin: &ReplayOrigin,
) -> Result<serde_json::Value, ProjectionError> {
    let projected =
        request_messages_with_replay(origin, &request.messages, features.reasoning_replay)?;
    let mut messages = Vec::new();
    if let Some(system) = projected.system {
        messages.push(serde_json::json!({"role": "system", "content": system}));
    }
    let system_count = messages.len();
    messages.extend(projected.messages);
    if features.cache_markers {
        if system_count != 0 {
            mark_last_content(&mut messages[0]);
        }
        if messages.len() > system_count {
            mark_last_content(messages.last_mut().expect("projected messages exist"));
        }
    }

    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });

    let mut reasoning_object = serde_json::Map::new();
    if let Some(plan) = reasoning {
        let toggle = match plan {
            ReasoningPlan::Effort {
                effort,
                enable_toggle,
            } => {
                let name = if effort == Effort::Off {
                    "none"
                } else {
                    effort.as_str()
                };
                match features.effort_param {
                    EffortParam::ReasoningEffort => body["reasoning_effort"] = name.into(),
                    EffortParam::ReasoningObject => {
                        reasoning_object.insert("effort".into(), name.into());
                    }
                }
                enable_toggle.then_some(true)
            }
            ReasoningPlan::Budget {
                tokens,
                enable_toggle,
                ..
            } => {
                match features.thinking_budget {
                    ThinkingBudget::ThinkingBudget => body["thinking_budget"] = tokens.into(),
                    ThinkingBudget::ReasoningMaxTokens => {
                        reasoning_object.insert("max_tokens".into(), tokens.into());
                    }
                    ThinkingBudget::None => {
                        unreachable!("resolved budget requires a supported feature")
                    }
                }
                enable_toggle.then_some(true)
            }
            ReasoningPlan::Toggle { enabled } => Some(enabled),
        };
        if let Some(enabled) = toggle {
            match features.thinking_toggle {
                ThinkingToggle::ThinkingType => {
                    body["thinking"] =
                        serde_json::json!({"type": if enabled { "enabled" } else { "disabled" }})
                }
                ThinkingToggle::EnableThinking => body["enable_thinking"] = enabled.into(),
                ThinkingToggle::None => {
                    unreachable!("resolved toggle requires a supported feature")
                }
            }
        }
    }
    if !reasoning_object.is_empty() {
        body["reasoning"] = reasoning_object.into();
    }

    if !request.tools.is_empty() {
        let tools_array: Vec<serde_json::Value> = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect();
        body["tools"] = serde_json::Value::Array(tools_array);
    }

    Ok(body)
}

/// 只标记已有的合法末块；无文本的工具调用不产生缓存块。
fn mark_last_content(message: &mut serde_json::Value) {
    let content = &mut message["content"];
    if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
        *content = serde_json::json!([{"type":"text","text":text}]);
    }
    if let Some(last) = content.as_array_mut().and_then(|blocks| blocks.last_mut())
        && last["type"] == "text"
    {
        last["cache_control"] = serde_json::json!({"type":"ephemeral"});
    }
}

pub(super) enum ContentError {
    Unsupported { index: usize, block_type: String },
    Malformed(&'static str),
}

impl From<ContentError> for ProtocolEvent {
    fn from(error: ContentError) -> Self {
        match error {
            ContentError::Unsupported { index, block_type } => {
                Self::unsupported("chat-completions", index, &block_type)
            }
            ContentError::Malformed(detail) => {
                Self::error(format!("malformed Chat Completions wire: {detail}"), false)
            }
        }
    }
}

fn chunk_text(chunk: &serde_json::Value) -> Result<&str, ContentError> {
    chunk["text"]
        .as_str()
        .ok_or(ContentError::Malformed("text chunk missing string text"))
}

/// 字符串与分块 content 共用翻译；thinking 内只接收 TextChunk，直接交付文字，不另存缓冲。
/// 格式依据 https://docs.mistral.ai/studio/conversations/reasoning#handling-thinking-chunks 。
pub(super) fn visit_content<E: From<ContentError>>(
    content: &serde_json::Value,
    mut emit: impl FnMut(PartKind, &str) -> Result<(), E>,
) -> Result<(), E> {
    let chunks = match content {
        serde_json::Value::Null => return Ok(()),
        serde_json::Value::String(text) => return emit(PartKind::Text, text),
        serde_json::Value::Array(chunks) => chunks,
        _ => {
            return Err(ContentError::Unsupported {
                index: 0,
                block_type: "content".into(),
            }
            .into());
        }
    };
    for (index, chunk) in chunks.iter().enumerate() {
        match chunk["type"].as_str() {
            Some("text") => emit(PartKind::Text, chunk_text(chunk)?)?,
            Some("thinking") => {
                let inner = chunk["thinking"].as_array().ok_or(ContentError::Malformed(
                    "thinking chunk missing text chunk list",
                ))?;
                for text in inner {
                    if text["type"].as_str() != Some("text") {
                        return Err(ContentError::Unsupported {
                            index,
                            block_type: text["type"].as_str().unwrap_or("<missing>").into(),
                        }
                        .into());
                    }
                    emit(PartKind::Reasoning, chunk_text(text)?)?;
                }
            }
            other => {
                return Err(ContentError::Unsupported {
                    index,
                    block_type: other.unwrap_or("<missing>").into(),
                }
                .into());
            }
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct OpenAiSseParser {
    usage: super::usage::UsageSnapshot,
    tool_calls: BTreeMap<u32, usize>,
    current: Option<(usize, PartKind)>,
    next_index: usize,
    pending_finish_reason: Option<String>,
    completed: bool,
}

impl OpenAiSseParser {
    pub fn new() -> Self {
        Self::default()
    }

    fn close_text(&mut self, events: &mut Vec<ProtocolEvent>) {
        if let Some((index, _)) = self.current.take() {
            events.push(ProtocolEvent::Part(PartDelta::End { index }));
        }
    }

    fn text(&mut self, kind: PartKind, text: &str, events: &mut Vec<ProtocolEvent>) {
        if text.is_empty() {
            return;
        }
        let index = if let Some((index, _)) = self.current.filter(|(_, current)| *current == kind) {
            index
        } else {
            self.close_text(events);
            let index = self.next_index;
            self.next_index += 1;
            self.current = Some((index, kind));
            events.push(ProtocolEvent::Part(PartDelta::Begin { index, kind }));
            index
        };
        events.push(ProtocolEvent::Part(match kind {
            PartKind::Text => PartDelta::Text {
                index,
                delta: text.into(),
            },
            PartKind::Reasoning => PartDelta::Reasoning {
                index,
                delta: text.into(),
            },
            PartKind::ToolCall => unreachable!(),
        }));
    }

    fn finish(&mut self, events: &mut Vec<ProtocolEvent>) {
        self.completed = true;
        self.close_text(events);
        for index in self.tool_calls.values() {
            events.push(ProtocolEvent::Part(PartDelta::End { index: *index }));
        }
        events.push(ProtocolEvent::Done {
            finish_reason: self
                .pending_finish_reason
                .take()
                .unwrap_or_else(|| "stop".into()),
        });
    }

    /// 只维护线上键映射与协议控制状态，内容直接交给共同累积器。
    pub fn parse_chunk(&mut self, _event_type: Option<&str>, data: &str) -> Vec<ProtocolEvent> {
        if self.completed {
            return Vec::new();
        }
        let mut events = Vec::new();
        if data.trim() == "[DONE]" {
            if self.pending_finish_reason.is_some() {
                self.finish(&mut events);
            }
            return events;
        }
        let root = match super::common::parse_json_data("Chat Completions", data) {
            Ok(Some(root)) => root,
            Ok(None) => return events,
            Err(error) => return vec![error],
        };
        if let Some(error) = super::common::extract_api_error(Protocol::ChatCompletions, &root) {
            return vec![error];
        }
        if let Some(choices) = root.get("choices").and_then(|value| value.as_array()) {
            for choice in choices {
                let delta = &choice["delta"];
                if let Some(reasoning) = ["reasoning_content", "reasoning"]
                    .into_iter()
                    .find_map(|key| delta[key].as_str().filter(|text| !text.is_empty()))
                {
                    self.text(PartKind::Reasoning, reasoning, &mut events);
                }
                if let Some(content) = delta.get("content")
                    && let Err(error) = visit_content(content, |kind, text| {
                        self.text(kind, text, &mut events);
                        Ok::<_, ProtocolEvent>(())
                    })
                {
                    return vec![error];
                }
                if let Some(calls) = delta.get("tool_calls").and_then(|value| value.as_array()) {
                    for call in calls {
                        let Some(wire_index) = call
                            .get("index")
                            .and_then(|value| value.as_u64())
                            .and_then(|value| u32::try_from(value).ok())
                        else {
                            return vec![ProtocolEvent::error(
                                "malformed Chat Completions wire: tool call index is missing or too large",
                                false,
                            )];
                        };
                        if let Some(kind) = call.get("type")
                            && kind.as_str() != Some("function")
                        {
                            return vec![ProtocolEvent::unsupported(
                                "chat-completions",
                                wire_index as usize,
                                kind.as_str().unwrap_or("<non-string>"),
                            )];
                        }
                        let index = if let Some(index) = self.tool_calls.get(&wire_index) {
                            *index
                        } else {
                            self.close_text(&mut events);
                            let index = self.next_index;
                            self.next_index += 1;
                            self.tool_calls.insert(wire_index, index);
                            events.push(ProtocolEvent::Part(PartDelta::Begin {
                                index,
                                kind: PartKind::ToolCall,
                            }));
                            index
                        };
                        let name = call["function"]["name"]
                            .as_str()
                            .filter(|name| !name.is_empty())
                            .map(|name| StringPatch::Set(name.into()));
                        let provider_call_id =
                            call["id"].as_str().map(|id| StringPatch::Set(id.into()));
                        if name.is_some() || provider_call_id.is_some() {
                            events.push(ProtocolEvent::Part(PartDelta::ToolMetadata {
                                index,
                                name,
                                provider_call_id,
                            }));
                        }
                        if let Some(arguments) = call["function"]["arguments"]
                            .as_str()
                            .filter(|text| !text.is_empty())
                        {
                            events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                                index,
                                delta: arguments.into(),
                            }));
                        }
                    }
                }
                if let Some(reason) = choice["finish_reason"]
                    .as_str()
                    .filter(|reason| !reason.is_empty())
                {
                    self.pending_finish_reason = Some(reason.into());
                }
            }
        }
        if root.get("usage").is_some_and(|usage| !usage.is_null()) {
            match self.usage.update(Protocol::ChatCompletions, &root) {
                Ok(Some(usage)) => events.push(ProtocolEvent::Usage(usage)),
                Ok(None) => {}
                Err(error) => {
                    self.completed = true;
                    events.push(ProtocolEvent::error(error.to_string(), false));
                    return events;
                }
            }
            if self.pending_finish_reason.is_some() {
                self.finish(&mut events);
            }
        }
        events
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_reply::{Parser, completed, failure};
    use crate::{LiveDelta, LlmEvent, ReplyOutcome, ReplyProtocolError, ReplyStop, StreamStop};
    use lattice_conversation::{
        MessagePart, MessageRole, MessageSemantic, NativeMessage, TokenUsage,
    };
    use serde_json::json;

    fn parser() -> Parser {
        Parser::new(Protocol::ChatCompletions)
    }
    fn delta(parser: &mut Parser, value: serde_json::Value) -> Vec<LlmEvent> {
        parser.parse_chunk(None, &json!({"choices": [{"delta": value}]}).to_string())
    }
    fn finish(parser: &mut Parser) -> Vec<LlmEvent> {
        let mut events =
            parser.parse_chunk(None, r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        events.extend(parser.parse_chunk(None, "[DONE]"));
        events
    }
    fn labels(message: &NativeMessage<lattice_conversation::UnboundToolCall>) -> Vec<String> {
        message
            .parts
            .iter()
            .map(|part| match &part.semantic {
                MessageSemantic::Text { text } => format!("T:{text}"),
                MessageSemantic::Reasoning { text } => format!("R:{text}"),
                MessageSemantic::ToolCall { call } => format!("C:{}", call.name),
            })
            .collect()
    }
    fn call_delta(index: u32, name: &str, arguments: &str) -> serde_json::Value {
        json!({"tool_calls": [{"index": index, "function": {"name": name, "arguments": arguments}}]})
    }
    fn assert_limit(events: &[LlmEvent], kind: lattice_conversation::LimitKind) {
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, LlmEvent::Finished { .. }))
                .count(),
            1
        );
        assert!(events.iter().any(|event| matches!(event,
            LlmEvent::Finished { outcome: ReplyOutcome::Interrupted {
                reason: ReplyStop::OutputLimit(lattice_conversation::AttemptInterruption::OutputLimitExceeded { kind: actual, .. })
            }, .. } if *actual == kind
        )), "{events:?}");
        assert!(!events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Complete { .. },
                ..
            }
        )));
    }

    #[tokio::test]
    async fn mistral_content_chunks_preserve_reasoning_text_and_transition_order() {
        use futures::{StreamExt, stream};
        let frames = vec![
            json!({"choices":[{"delta":{"content":[{"type":"thinking","thinking":[{"type":"text","text":"R"}]}]}}]}).to_string(),
            json!({"choices":[{"delta":{"content":[{"type":"thinking","thinking":[{"type":"text","text":"S"}]},{"type":"text","text":"A"}]}}]}).to_string(),
            json!({"choices":[{"delta":{"content":"B"},"finish_reason":"stop"}]}).to_string(),
            "[DONE]".into(),
        ];
        let mut parser = OpenAiSseParser::new();
        let input = stream::iter(frames)
            .flat_map(move |frame| stream::iter(parser.parse_chunk(None, &frame)));
        let events = crate::reply_stream::assemble_reply_stream(
            Some(crate::test_reply::replay_origin(Protocol::ChatCompletions)),
            Box::pin(input),
            crate::StreamControl::default(),
        )
        .collect::<Vec<_>>()
        .await;
        let message = completed(&events);
        assert_eq!(labels(message), ["R:RS", "T:AB"]);
        let live: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                LlmEvent::Delta(delta) => Some(delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            live,
            [
                LiveDelta::Reasoning("R".into()),
                LiveDelta::Reasoning("S".into()),
                LiveDelta::Text("A".into()),
                LiveDelta::Text("B".into())
            ]
        );
        let wire = json!({"choices":[{"message":{"content":[
            {"type":"thinking","thinking":[{"type":"text","text":"R"},{"type":"text","text":"S"}]},
            {"type":"text","text":"A"}, {"type":"text","text":"B"}
        ]}}]});
        assert_eq!(
            crate::test_reply::wire_to_native(Protocol::ChatCompletions, &wire).unwrap(),
            *message
        );
    }

    #[test]
    fn mistral_unknown_content_chunks_fail_typed_in_both_paths() {
        for (chunk, expected) in [
            (
                json!({"type":"image_url","image_url":"private-payload"}),
                "image_url",
            ),
            (
                json!({"type":"thinking","thinking":[{"type":"future_inner","data":"private-payload"}]}),
                "future_inner",
            ),
        ] {
            let content = json!([{"type":"text","text":"prefix"},chunk]);
            let error = ReplyProtocolError::UnsupportedContentBlock {
                protocol: "chat-completions".into(),
                index: 1,
                block_type: expected.into(),
            };
            let mut parser = parser();
            let events = delta(&mut parser, json!({"content":content}));
            assert!(matches!(events.as_slice(), [LlmEvent::Finished {
                outcome: ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::Protocol(actual)) }, ..
            }] if actual == &error));
            assert!(
                finish(&mut parser).is_empty(),
                "unknown chunks cannot publish a complete reply"
            );
            assert!(!format!("{events:?}").contains("private-payload"));
            let wire = json!({"choices":[{"message":{"content":content}}]});
            assert_eq!(
                crate::test_reply::wire_to_native(Protocol::ChatCompletions, &wire),
                Err(ProjectionError::Protocol(error))
            );
        }
    }

    #[test]
    fn unknown_tool_types_fail_in_stream_and_complete_wire() {
        for (kind, expected) in [
            (json!("future_tool"), "future_tool".into()),
            (json!("x".repeat(80)), "x".repeat(64)),
            (json!(null), "<non-string>".into()),
        ] {
            let error = ReplyProtocolError::UnsupportedContentBlock {
                protocol: "chat-completions".into(),
                index: 9,
                block_type: expected.clone(),
            };
            for late in [false, true] {
                let mut parser = parser();
                if late {
                    assert!(delta(&mut parser, json!({"tool_calls":[{"index":9,"type":"function","function":{"name":"f","arguments":"{"}}]})).is_empty());
                }
                let events = delta(
                    &mut parser,
                    json!({"tool_calls":[{"index":9,"type":kind,"function":{"name":"f","arguments":"{}"},"payload":"private-payload"}]}),
                );
                assert!(matches!(&events[..], [LlmEvent::Finished {
                    outcome: ReplyOutcome::Interrupted { reason: ReplyStop::Stream(StreamStop::Protocol(actual)) }, ..
                }] if actual == &error));
                assert!(
                    finish(&mut parser).is_empty(),
                    "no complete message after rejection"
                );
                assert!(!format!("{events:?}").contains("private-payload"));
            }
            assert_eq!(
                crate::test_reply::wire_to_native(
                    Protocol::ChatCompletions,
                    &json!({"choices":[{"message":{"tool_calls":[{"type":kind,"function":{"name":"f","arguments":"{}"}}]}}]}),
                ),
                Err(ProjectionError::Protocol(
                    ReplyProtocolError::UnsupportedContentBlock {
                        protocol: "chat-completions".into(),
                        index: 0,
                        block_type: expected,
                    }
                )),
            );
        }
    }

    #[test]
    fn function_tool_type_can_be_omitted_from_following_deltas() {
        let mut parser = parser();
        delta(
            &mut parser,
            json!({"tool_calls":[{"index":9,"type":"function","function":{"name":"f","arguments":"{"}}]}),
        );
        delta(
            &mut parser,
            json!({"tool_calls":[{"index":9,"function":{"arguments":"}"}}]}),
        );
        let events = finish(&mut parser);
        let message = completed(&events);
        let calls: Vec<_> = message.tool_calls().collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1.name, "f");
        assert_eq!(calls[0].1.arguments, json!({}));
    }

    #[test]
    fn parser_preserves_real_and_missing_ids_without_synthesis() {
        for id in [Some("call_abc"), None] {
            let mut parser = parser();
            let mut call = call_delta(0, "observe__workspace", "{}");
            if let Some(id) = id {
                call["tool_calls"][0]["id"] = json!(id);
            }
            delta(&mut parser, call);
            assert!(parser.raw.iter().any(|event| matches!(event, ProtocolEvent::Part(PartDelta::ToolMetadata { name: Some(StringPatch::Set(name)), provider_call_id, .. })
                if name == "observe__workspace" && provider_call_id.as_ref() == id.map(|id| StringPatch::Set(id.into())).as_ref())));
            let events = finish(&mut parser);
            let message = completed(&events);
            assert_eq!(
                message
                    .tool_calls()
                    .next()
                    .unwrap()
                    .1
                    .provider_call_id
                    .as_deref(),
                id
            );
            assert!(!serde_json::to_string(message).unwrap().contains("tc_"));
        }
    }
    #[test]
    fn parser_completes_text_and_usage() {
        let mut parser = parser();
        let events = parser.parse_chunk(
            None,
            r#"{"choices":[{"delta":{"content":"hello"},"finish_reason":"stop"}],"usage":null}"#,
        );
        assert_eq!(events, [LlmEvent::Delta(LiveDelta::Text("hello".into()))]);
        let events = parser.parse_chunk(
            None,
            r#"{"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}"#,
        );
        assert_eq!(labels(completed(&events)), ["T:hello"]);
        assert!(
            matches!(&events[..], [LlmEvent::Finished { report, .. }] if report.usage.unwrap().context_tokens() == Some(3) && report.finish_reason.as_deref() == Some("stop"))
        );
        assert!(parser.parse_chunk(None, "[DONE]").is_empty());
    }
    #[test]
    fn parser_accumulates_multichunk_name_id_and_arguments() {
        let mut parser = parser();
        let first = parser.parse_chunk(None, r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"observe__workspace","arguments":"{\"path\""}}]}}]}"#);
        assert!(first.is_empty());
        assert!(parser.raw.iter().any(|event| matches!(event, ProtocolEvent::Part(PartDelta::ToolMetadata { name: Some(StringPatch::Set(name)), provider_call_id: Some(StringPatch::Set(id)), .. }) if name == "observe__workspace" && id == "call_1")));
        delta(&mut parser, call_delta(0, "", ":\"a\"}"));
        let events = finish(&mut parser);
        let call = completed(&events).tool_calls().next().unwrap().1;
        assert_eq!(call.name, "observe__workspace");
        assert_eq!(call.provider_call_id.as_deref(), Some("call_1"));
        assert_eq!(call.arguments, json!({"path":"a"}));
    }
    #[test]
    fn parser_rejects_malformed_usage_and_missing_tool_index() {
        let mut parser = parser();
        let events = parser.parse_chunk(
            None,
            r#"{"choices":[{"delta":{"content":"x"},"finish_reason":"stop"}],"usage":null}"#,
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, LlmEvent::Finished { .. }))
        );
        let events = parser.parse_chunk(None, r#"{"choices":[],"usage":{"prompt_tokens":"bad","completion_tokens":2,"total_tokens":3}}"#);
        assert!(failure(&events).unwrap().contains("usage"));
        assert!(!events.iter().any(|event| matches!(
            event,
            LlmEvent::Finished {
                outcome: ReplyOutcome::Complete { .. },
                ..
            }
        )));
        let mut parser = self::parser();
        let events = delta(
            &mut parser,
            json!({"tool_calls":[{"function":{"name":"observe__workspace","arguments":"{}"}}]}),
        );
        assert!(failure(&events).unwrap().contains("index"));
    }
    #[test]
    fn reasoning_alias_keeps_order_and_is_not_double_counted() {
        let mut parser = parser();
        let mut live = vec![];
        for value in [
            json!({"reasoning":"想"}),
            json!({"content":"答"}),
            json!({"reasoning_content":"再想", "reasoning":"再想"}),
        ] {
            live.extend(delta(&mut parser, value));
        }
        let live: Vec<_> = live
            .into_iter()
            .map(|event| match event {
                LlmEvent::Delta(LiveDelta::Text(text)) => format!("T:{text}"),
                LlmEvent::Delta(LiveDelta::Reasoning(text)) => format!("R:{text}"),
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        assert_eq!(live, ["R:想", "T:答", "R:再想"]);
        assert_eq!(labels(completed(&finish(&mut parser))), live);
    }
    #[test]
    fn order_f02_same_chunk_follows_emitted_order() {
        let mut parser = parser();
        delta(&mut parser, json!({"content":"A"}));
        let events = delta(
            &mut parser,
            json!({"content":"B","reasoning_content":"R","tool_calls":[{"index":8,"function":{"name":"t","arguments":"{}"}}]}),
        );
        assert_eq!(
            events,
            [
                LlmEvent::Delta(LiveDelta::Reasoning("R".into())),
                LlmEvent::Delta(LiveDelta::Text("B".into()))
            ]
        );
        assert!(matches!(
            &parser.raw[parser.raw.len() - 3..],
            [
                ProtocolEvent::Part(PartDelta::Begin {
                    kind: PartKind::ToolCall,
                    ..
                }),
                ProtocolEvent::Part(PartDelta::ToolMetadata { .. }),
                ProtocolEvent::Part(PartDelta::ToolArguments { .. })
            ]
        ));
        assert_eq!(
            labels(completed(&finish(&mut parser))),
            ["T:A", "R:R", "T:B", "C:t"]
        );
    }
    #[test]
    fn ordered_parts_preserve_tool_positions_and_ignore_empty_fragments() {
        for (deltas, expected) in [
            (
                vec![
                    json!({"content":"A"}),
                    call_delta(0, "t", "{}"),
                    json!({"content":"B"}),
                ],
                vec!["T:A", "C:t", "T:B"],
            ),
            (
                vec![
                    call_delta(u32::MAX, "first", "{}"),
                    call_delta(7, "second", "{}"),
                    call_delta(1, "third", "{}"),
                ],
                vec!["C:first", "C:second", "C:third"],
            ),
            (
                vec![
                    json!({"content":"","reasoning_content":""}),
                    json!({"content":"A"}),
                    json!({"reasoning_content":""}),
                    json!({"content":"B"}),
                    json!({"reasoning_content":"R"}),
                    json!({"content":""}),
                    json!({"reasoning_content":"S"}),
                ],
                vec!["T:AB", "R:RS"],
            ),
            (vec![], vec![]),
        ] {
            let mut parser = parser();
            for value in deltas {
                let empty = value == json!({"content":"","reasoning_content":""});
                let events = delta(&mut parser, value);
                if empty {
                    assert!(events.is_empty());
                    assert!(parser.raw.is_empty());
                }
            }
            assert_eq!(labels(completed(&finish(&mut parser))), expected);
        }
    }
    #[test]
    fn order_f05_late_metadata_and_fragments_fill_original_slot() {
        let mut parser = parser();
        let events = delta(
            &mut parser,
            json!({"tool_calls":[{"index":9,"function":{"arguments":"{\"path\":"}}]}),
        );
        assert!(events.is_empty());
        assert!(matches!(
            &parser.raw[..],
            [
                ProtocolEvent::Part(PartDelta::Begin {
                    index: 0,
                    kind: PartKind::ToolCall
                }),
                ProtocolEvent::Part(PartDelta::ToolArguments { index: 0, .. })
            ]
        ));
        delta(&mut parser, json!({"content":"between"}));
        delta(&mut parser, call_delta(2, "second", "{}"));
        delta(
            &mut parser,
            json!({"tool_calls":[{"index":9,"id":"real-id","function":{"name":"observe__workspace","arguments":"\"a\"}"}}]}),
        );
        assert!(matches!(
            &parser.raw[..],
            [
                ProtocolEvent::Part(PartDelta::ToolMetadata { index: 0, .. }),
                ProtocolEvent::Part(PartDelta::ToolArguments { index: 0, .. })
            ]
        ));
        let events = finish(&mut parser);
        let message = completed(&events);
        assert_eq!(
            labels(message),
            ["C:observe__workspace", "T:between", "C:second"]
        );
        let calls: Vec<_> = message.tool_calls().map(|(_, call)| call).collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].provider_call_id.as_deref(), Some("real-id"));
        assert_eq!(calls[0].arguments, json!({"path":"a"}));
        assert_eq!(calls[1].arguments, json!({}));
    }
    #[test]
    fn order_f06_usage_and_done_complete_once() {
        for mode in 0..3 {
            let mut parser = parser();
            let usage = json!({"prompt_tokens":1,"completion_tokens":2,"total_tokens":3});
            let mut chunk = json!({"choices":[{"delta":{"content":"A"},"finish_reason":"stop"}]});
            if mode == 0 {
                chunk["usage"] = usage.clone();
            }
            let mut events = parser.parse_chunk(None, &chunk.to_string());
            if mode != 0 {
                assert!(
                    !events
                        .iter()
                        .any(|event| matches!(event, LlmEvent::Finished { .. }))
                );
            }
            if mode == 1 {
                events.extend(
                    parser.parse_chunk(None, &json!({"choices":[],"usage":usage}).to_string()),
                );
            }
            events.extend(parser.parse_chunk(None, "[DONE]"));
            assert_eq!(labels(completed(&events)), ["T:A"]);
            let reports: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    LlmEvent::Finished { report, .. } => Some(report),
                    _ => None,
                })
                .collect();
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].finish_reason.as_deref(), Some("stop"));
            assert_eq!(
                reports[0].usage,
                if mode == 2 {
                    None
                } else {
                    Some(TokenUsage {
                        input: Some(1),
                        output: Some(2),
                        ..Default::default()
                    })
                }
            );
            assert!(parser.parse_chunk(None, "[DONE]").is_empty());
            assert!(delta(&mut parser, json!({"content":"late"})).is_empty());
        }
    }
    #[test]
    fn order_f09_interleaved_wire_indexes_keep_separate_budgets() {
        use lattice_conversation::{LimitKind, MAX_TOOL_CALL_ARGUMENTS_BYTES};
        let maximum = MAX_TOOL_CALL_ARGUMENTS_BYTES;
        let arguments = format!("{{\"x\":\"{}\"}}", "z".repeat(maximum - 8));
        for extra in ["", " "] {
            let mut parser = parser();
            delta(
                &mut parser,
                call_delta(9, "first", &arguments[..maximum / 2]),
            );
            delta(&mut parser, call_delta(2, "second", "{}"));
            let mut events = delta(
                &mut parser,
                call_delta(9, "", &format!("{}{extra}", &arguments[maximum / 2..])),
            );
            events.extend(finish(&mut parser));
            if extra.is_empty() {
                completed(&events);
            } else {
                assert_limit(&events, LimitKind::ToolCallArguments);
            }
        }
    }
    fn ordered_native() -> NativeMessage<lattice_conversation::UnboundToolCall> {
        NativeMessage::new(
            MessageRole::Assistant,
            Some(crate::test_reply::replay_origin(Protocol::ChatCompletions)),
            vec![
                MessagePart::semantic_only(MessageSemantic::Text { text: "A".into() }),
                MessagePart::semantic_only(MessageSemantic::Reasoning { text: "R".into() }),
                MessagePart::semantic_only(MessageSemantic::ToolCall {
                    call: lattice_conversation::UnboundToolCall {
                        provider_call_id: Some("real".into()),
                        name: "observe__workspace".into(),
                        arguments: json!({"z":1,"a":2}),
                    },
                }),
                MessagePart::semantic_only(MessageSemantic::Text { text: "B".into() }),
                MessagePart::semantic_only(MessageSemantic::Reasoning { text: "S".into() }),
            ],
        )
    }
    #[test]
    fn order_f11_request_merges_without_changing_wire_shape() {
        let wire = crate::protocol::projection::native_to_wire(
            &crate::test_reply::replay_origin(Protocol::ChatCompletions),
            &ordered_native(),
        )
        .unwrap();
        assert_eq!(
            wire,
            json!({"role":"assistant","content":"AB","reasoning_content":"RS","tool_calls":[{"id":"real","type":"function","function":{"name":"observe__workspace","arguments":"{\"a\":2,\"z\":1}"}}]})
        );
    }
    #[test]
    fn order_f12_cross_provider_preserves_remaining_order() {
        let source = ordered_native();
        let original = source.clone();
        let projected = crate::protocol::projection::project_cross_provider(
            &source,
            &crate::test_reply::replay_origin(Protocol::GenerateContent),
        );
        assert_eq!(labels(&projected), ["T:A", "C:observe__workspace", "T:B"]);
        assert!(
            projected
                .tool_calls()
                .next()
                .unwrap()
                .1
                .provider_call_id
                .is_none()
        );
        assert_eq!(source, original);
    }
}
