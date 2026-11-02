// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Responses 的无状态请求、事件路由与逐片段回放材料。

use std::collections::BTreeMap;

use lattice_conversation::{Effort, Protocol, ReplayOrigin};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::projection::{ProjectionError, request_messages_with_replay};
use super::{ReasoningSummary, ResponsesFeatures};
use crate::reasoning::ReasoningPlan;
use crate::reply::{PartDelta, PartKind, ProtocolEvent, StringPatch};
use crate::types::ChatRequest;

pub(crate) fn build_request_json(
    request: &ChatRequest,
    features: ResponsesFeatures,
    reasoning: Option<ReasoningPlan>,
    model_reasoning: Option<bool>,
    origin: &ReplayOrigin,
) -> Result<Value, ProjectionError> {
    let projected =
        request_messages_with_replay(origin, &request.messages, super::ReasoningReplay::None)?;
    let mut body = json!({
        "model":request.target.model, "input":projected.messages,
        "stream":true, "store":false,
    });
    if let Some(system) = projected.system {
        body["instructions"] = system.into();
    }
    if let Some(session) = &request.client_request_id {
        body["prompt_cache_key"] = session.clone().into();
    }
    if model_reasoning != Some(false) {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    let effort = match reasoning {
        Some(ReasoningPlan::Effort { effort, .. }) => Some(effort),
        _ => None,
    };
    if let Some(effort) = effort {
        body["reasoning"] =
            json!({"effort":if effort == Effort::Off {"none"} else {effort.as_str()}});
    }
    if features.reasoning_summary == ReasoningSummary::Auto
        && model_reasoning != Some(false)
        && effort != Some(Effort::Off)
    {
        body["reasoning"]["summary"] = "auto".into();
    }
    if !request.tools.is_empty() {
        body["tools"] = request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type":"function", "name":tool.name, "description":tool.description,
                    "parameters":tool.parameters, "strict":false,
                })
            })
            .collect();
    }
    Ok(body)
}

// 仅解码参与协议转换的字段；回显的 instructions、tools 等由 serde 跳过。
#[derive(Deserialize)]
struct EventType {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Event {
    response: Option<Response>,
    item: Option<OutputItem>,
    part: Option<Content>,
    output_index: Option<usize>,
    item_id: Option<String>,
    content_index: Option<usize>,
    summary_index: Option<usize>,
    delta: Option<String>,
    text: Option<String>,
    refusal: Option<String>,
    arguments: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct Response {
    status: Option<String>,
    output: Option<Vec<OutputItem>>,
    usage: Option<Value>,
    error: Option<ApiError>,
    incomplete_details: Option<IncompleteDetails>,
}

#[derive(Deserialize)]
struct ApiError {
    code: Option<String>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct IncompleteDetails {
    reason: String,
}

#[derive(Deserialize)]
struct OutputItem {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    role: Option<String>,
    phase: Option<String>,
    status: Option<String>,
    #[serde(default)]
    content: Vec<Content>,
    #[serde(default)]
    summary: Vec<Content>,
    encrypted_content: Option<String>,
    name: Option<String>,
    call_id: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    refusal: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ContentKind {
    OutputText,
    Refusal,
    SummaryText,
    ReasoningText,
    FunctionCall,
    Empty,
}

impl ContentKind {
    fn part_kind(self, item_kind: &str) -> PartKind {
        match self {
            Self::SummaryText | Self::ReasoningText => PartKind::Reasoning,
            Self::FunctionCall => PartKind::ToolCall,
            Self::Empty if item_kind == "reasoning" => PartKind::Reasoning,
            _ => PartKind::Text,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartKey {
    pub kind: ContentKind,
    pub index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Anchor {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encrypted_content: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplayState {
    pub item: usize,
    pub part: PartKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

#[derive(Default)]
struct ContentState {
    seen: bool,
    done: bool,
}

struct ItemState {
    anchor: Anchor,
    name: Option<String>,
    call_id: Option<String>,
    contents: BTreeMap<PartKey, ContentState>,
    slots: Vec<(usize, PartKey)>,
    done: bool,
}

#[derive(Default)]
pub(crate) struct ResponsesSseParser {
    items: BTreeMap<usize, ItemState>,
    current: Option<(usize, PartKey, usize)>,
    next_index: usize,
    usage: super::usage::UsageSnapshot,
    stopped: bool,
}

fn malformed(detail: &'static str) -> ProtocolEvent {
    ProtocolEvent::error(format!("malformed Responses wire: {detail}"), false)
}

fn required<T>(value: Option<T>, detail: &'static str) -> Result<T, ProtocolEvent> {
    value.ok_or_else(|| malformed(detail))
}

fn unsupported(index: usize, kind: &str) -> ProtocolEvent {
    ProtocolEvent::unsupported("responses", index, kind)
}

fn content_key(
    content: &Content,
    index: usize,
    reasoning: bool,
    summary: bool,
) -> Result<PartKey, ProtocolEvent> {
    let kind = match (content.kind.as_str(), reasoning, summary) {
        ("output_text", false, false) => ContentKind::OutputText,
        ("refusal", false, false) => ContentKind::Refusal,
        ("summary_text", true, true) => ContentKind::SummaryText,
        ("reasoning_text", true, false) => ContentKind::ReasoningText,
        _ => return Err(unsupported(index, &content.kind)),
    };
    Ok(PartKey { kind, index })
}

fn content_text(content: &Content) -> Result<&str, ProtocolEvent> {
    required(
        if content.kind == "refusal" {
            content.refusal.as_deref()
        } else {
            content.text.as_deref()
        },
        "content missing text",
    )
}

impl ResponsesSseParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn parse_chunk(
        &mut self,
        event_type: Option<&str>,
        data: &str,
    ) -> Vec<ProtocolEvent> {
        if self.stopped || data.trim().is_empty() || data.trim() == "[DONE]" {
            return Vec::new();
        }
        let mut events = Vec::new();
        let result = serde_json::from_str::<EventType>(data)
            .map_err(|_| malformed("invalid event fields"))
            .and_then(|event| self.event(event_type, &event.kind, data, &mut events));
        if let Err(error) = result {
            self.stopped = true;
            events.push(error);
        }
        events
    }

    fn update_usage(
        &mut self,
        response: &mut Response,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        if let Some(usage) = response.usage.take()
            && let Some(usage) = self
                .usage
                .update(Protocol::Responses, &json!({"usage":usage}))
                .map_err(|_| malformed("invalid usage"))?
        {
            events.push(ProtocolEvent::Usage(usage));
        }
        Ok(())
    }

    fn check_item(item: &OutputItem, index: usize) -> Result<(), ProtocolEvent> {
        match item.kind.as_str() {
            "message" if item.role.as_deref() == Some("assistant") => {
                for (i, content) in item.content.iter().enumerate() {
                    content_key(content, i, false, false)?;
                    content_text(content)?;
                }
                if !item.summary.is_empty() || item.encrypted_content.is_some() {
                    return Err(malformed("message carries reasoning fields"));
                }
            }
            "reasoning" => {
                for (i, content) in item.summary.iter().enumerate() {
                    content_key(content, i, true, true)?;
                    content_text(content)?;
                }
                for (i, content) in item.content.iter().enumerate() {
                    content_key(content, i, true, false)?;
                    content_text(content)?;
                }
            }
            "function_call" => {
                if !item.content.is_empty() || !item.summary.is_empty() {
                    return Err(malformed("function call carries content"));
                }
            }
            "message" => return Err(malformed("message is not assistant")),
            _ => return Err(unsupported(index, &item.kind)),
        }
        if item.id.is_none() {
            return Err(malformed("output item missing id"));
        }
        Ok(())
    }

    fn add_item(
        &mut self,
        index: usize,
        item: &OutputItem,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        Self::check_item(item, index)?;
        if self.items.contains_key(&index) {
            return Err(malformed("duplicate output item"));
        }
        self.items.insert(
            index,
            ItemState {
                name: item.name.clone(),
                call_id: item.call_id.clone(),
                anchor: Anchor {
                    kind: item.kind.clone(),
                    id: item.id.clone(),
                    phase: item.phase.clone(),
                    encrypted_content: None,
                },
                contents: BTreeMap::new(),
                slots: Vec::new(),
                done: false,
            },
        );
        if item.kind == "function_call" {
            let key = PartKey {
                kind: ContentKind::FunctionCall,
                index: 0,
            };
            self.begin_segment(index, key, events)?;
            self.tool_metadata(index, item, events)?;
        } else {
            self.begin_segment(
                index,
                PartKey {
                    kind: ContentKind::Empty,
                    index: 0,
                },
                events,
            )?;
        }
        Ok(())
    }

    fn route(&self, index: usize, id: Option<&str>) -> Result<(), ProtocolEvent> {
        let item = self
            .items
            .get(&index)
            .ok_or_else(|| malformed("item has not begun"))?;
        if item.done {
            return Err(malformed("item already ended"));
        }
        if id.is_none() || item.anchor.id.as_deref() != id {
            return Err(malformed("item id mismatch"));
        }
        Ok(())
    }

    fn begin_segment(
        &mut self,
        item_index: usize,
        key: PartKey,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<usize, ProtocolEvent> {
        let item = self
            .items
            .get_mut(&item_index)
            .ok_or_else(|| malformed("item has not begun"))?;
        // 首个内容紧跟 item 时沿用空边界；已有其他片段到达后必须另建位置。
        if key.kind != ContentKind::Empty
            && let Some((current_item, current_key, index)) = self.current
            && current_item == item_index
            && current_key.kind == ContentKind::Empty
        {
            let state = item.contents.remove(&current_key).unwrap();
            item.contents.insert(key, state);
            item.slots[0].1 = key;
            events.push(ProtocolEvent::Part(PartDelta::ReplayState {
                index,
                patch: StringPatch::Set(
                    serde_json::to_string(&ReplayState {
                        item: item_index,
                        part: key,
                        anchor: Some(item.anchor.clone()),
                    })
                    .expect("replay metadata serializes"),
                ),
            }));
            self.current = Some((item_index, key, index));
            return Ok(index);
        }
        let index = self.next_index;
        self.next_index += 1;
        let anchor = item.slots.is_empty().then(|| item.anchor.clone());
        let kind = key.kind.part_kind(&item.anchor.kind);
        item.slots.push((index, key));
        item.contents.entry(key).or_default();
        events.push(ProtocolEvent::Part(PartDelta::Begin { index, kind }));
        events.push(ProtocolEvent::Part(PartDelta::ReplayState {
            index,
            patch: StringPatch::Set(
                serde_json::to_string(&ReplayState {
                    item: item_index,
                    part: key,
                    anchor,
                })
                .expect("replay metadata serializes"),
            ),
        }));
        self.current = (kind != PartKind::ToolCall).then_some((item_index, key, index));
        Ok(index)
    }

    fn text(
        &mut self,
        item_index: usize,
        key: PartKey,
        text: &str,
        final_value: bool,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        let item = self
            .items
            .get(&item_index)
            .ok_or_else(|| malformed("item has not begun"))?;
        let state = item
            .contents
            .get(&key)
            .ok_or_else(|| malformed("content has not begun"))?;
        if final_value && (state.seen || state.done) {
            self.items
                .get_mut(&item_index)
                .unwrap()
                .contents
                .get_mut(&key)
                .unwrap()
                .done = true;
            return Ok(());
        }
        if state.done {
            return Err(malformed("content already ended"));
        }
        let index = match self.current {
            Some((item, part, index)) if item == item_index && part == key => index,
            _ => self.begin_segment(item_index, key, events)?,
        };
        let state = self
            .items
            .get_mut(&item_index)
            .unwrap()
            .contents
            .get_mut(&key)
            .unwrap();
        state.seen = true;
        state.done = final_value;
        events.push(ProtocolEvent::Part(
            if matches!(
                key.kind,
                ContentKind::SummaryText | ContentKind::ReasoningText
            ) {
                PartDelta::Reasoning {
                    index,
                    delta: text.into(),
                }
            } else {
                PartDelta::Text {
                    index,
                    delta: text.into(),
                }
            },
        ));
        Ok(())
    }

    fn tool_metadata(
        &mut self,
        index: usize,
        item: &OutputItem,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        let state = &self.items[&index];
        if state.name != item.name || state.call_id != item.call_id {
            return Err(malformed("function metadata changed"));
        }
        let slot = self.items[&index].slots[0].0;
        events.push(ProtocolEvent::Part(PartDelta::ToolMetadata {
            index: slot,
            name: Some(StringPatch::Set(required(
                item.name.clone(),
                "function call missing name",
            )?)),
            provider_call_id: item.call_id.clone().map(StringPatch::Set),
        }));
        Ok(())
    }

    fn arguments(
        &mut self,
        index: usize,
        arguments: String,
        final_value: bool,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        let item = self
            .items
            .get_mut(&index)
            .ok_or_else(|| malformed("item has not begun"))?;
        if item.anchor.kind != "function_call" {
            return Err(malformed("arguments on non-function item"));
        }
        let key = PartKey {
            kind: ContentKind::FunctionCall,
            index: 0,
        };
        let state = item.contents.get_mut(&key).unwrap();
        if final_value && (state.seen || state.done) {
            state.done = true;
            return Ok(());
        }
        if state.done {
            return Err(malformed("arguments already ended"));
        }
        state.seen = true;
        state.done = final_value;
        events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
            index: item.slots[0].0,
            delta: arguments,
        }));
        Ok(())
    }

    fn finish_item(
        &mut self,
        index: usize,
        item: OutputItem,
        allow_new: bool,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        Self::check_item(&item, index)?;
        if item
            .status
            .as_deref()
            .is_some_and(|status| status != "completed")
        {
            return Err(malformed("output item is not complete"));
        }
        if !self.items.contains_key(&index) {
            if !allow_new {
                return Err(malformed("item has not begun"));
            }
            self.add_item(index, &item, events)?;
        }
        let previous = &self.items[&index];
        if previous.anchor.kind != item.kind || previous.anchor.id != item.id {
            return Err(malformed("final item identity mismatch"));
        }
        if previous.done {
            if !allow_new {
                return Err(malformed("duplicate item end"));
            }
            if item.phase != previous.anchor.phase
                || item.encrypted_content != previous.anchor.encrypted_content
                || item.name != previous.name
                || item.call_id != previous.call_id
            {
                return Err(malformed("completed replay metadata changed"));
            }
            return Ok(());
        }
        let mut final_keys = std::collections::BTreeSet::new();
        for (summary, contents) in [(false, &item.content), (true, &item.summary)] {
            for (i, content) in contents.iter().enumerate() {
                final_keys.insert(content_key(content, i, item.kind == "reasoning", summary)?);
            }
        }
        if item.kind == "function_call" {
            final_keys.insert(PartKey {
                kind: ContentKind::FunctionCall,
                index: 0,
            });
        }
        if previous
            .contents
            .keys()
            .any(|key| key.kind != ContentKind::Empty && !final_keys.contains(key))
        {
            return Err(malformed("final item omits content"));
        }
        for (summary, contents) in [(false, &item.content), (true, &item.summary)] {
            for (i, content) in contents.iter().enumerate() {
                let key = content_key(content, i, item.kind == "reasoning", summary)?;
                if !self.items[&index].contents.contains_key(&key) {
                    self.begin_segment(index, key, events)?;
                }
                self.text(index, key, content_text(content)?, true, events)?;
            }
        }
        if item.kind == "function_call" {
            self.tool_metadata(index, &item, events)?;
            self.arguments(
                index,
                required(item.arguments, "function call missing arguments")?,
                true,
                events,
            )?;
        }
        let state = self.items.get_mut(&index).unwrap();
        state.anchor.phase = item.phase.or(state.anchor.phase.take());
        state.anchor.encrypted_content = item.encrypted_content;
        let (anchor_index, key) = state.slots[0];
        events.push(ProtocolEvent::Part(PartDelta::ReplayState {
            index: anchor_index,
            patch: StringPatch::Set(
                serde_json::to_string(&ReplayState {
                    item: index,
                    part: key,
                    anchor: Some(state.anchor.clone()),
                })
                .expect("replay metadata serializes"),
            ),
        }));
        for &(index, _) in &state.slots {
            events.push(ProtocolEvent::Part(PartDelta::End { index }));
        }
        state.done = true;
        if self.current.is_some_and(|(item, _, _)| item == index) {
            self.current = None;
        }
        Ok(())
    }

    pub(super) fn completed(
        &mut self,
        mut response: Response,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        self.update_usage(&mut response, events)?;
        if response.status.as_deref() != Some("completed") {
            return Err(malformed("response status is not completed"));
        }
        self.finish_output(
            required(response.output, "response missing output")?,
            false,
            events,
        )?;
        let calls = self
            .items
            .values()
            .any(|item| item.anchor.kind == "function_call");
        events.push(ProtocolEvent::Done {
            finish_reason: if calls { "tool_calls" } else { "stop" }.into(),
        });
        self.stopped = true;
        Ok(())
    }

    fn finish_output(
        &mut self,
        output: Vec<OutputItem>,
        incomplete: bool,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        if self.items.keys().any(|&index| index >= output.len()) {
            return Err(malformed("response omits output item"));
        }
        for (index, mut item) in output.into_iter().enumerate() {
            if incomplete && matches!(item.status.as_deref(), Some("incomplete" | "in_progress")) {
                item.status = None;
            }
            self.finish_item(index, item, true, events)?;
        }
        Ok(())
    }

    fn incomplete(
        &mut self,
        mut response: Response,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        self.update_usage(&mut response, events)?;
        let detail = required(
            response.incomplete_details,
            "incomplete response missing reason",
        )?;
        let finish_reason = match detail.reason.as_str() {
            "max_output_tokens" => "length",
            "content_filter" => "content_filter",
            _ => {
                return Err(ProtocolEvent::error(
                    format!("Responses incomplete: {}", detail.reason),
                    false,
                ));
            }
        };
        if response.status.as_deref() != Some("incomplete") {
            return Err(malformed("response status is not incomplete"));
        }
        if let Some(output) = response.output {
            self.finish_output(output, true, events)?;
        }
        // 截断可能省略 item 的结束事件；结束片段后由共同累积器校验整批工具参数。
        for item in self.items.values_mut().filter(|item| !item.done) {
            for &(index, _) in &item.slots {
                events.push(ProtocolEvent::Part(PartDelta::End { index }));
            }
            item.done = true;
        }
        self.current = None;
        events.push(ProtocolEvent::Done {
            finish_reason: finish_reason.into(),
        });
        self.stopped = true;
        Ok(())
    }

    fn event(
        &mut self,
        event_type: Option<&str>,
        kind: &str,
        data: &str,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        // 先按类型分派，未知的非增量事件不解码或校验其余字段。
        let decode = || {
            if event_type.is_some_and(|name| name != kind) {
                return Err(malformed("event name differs from type"));
            }
            serde_json::from_str::<Event>(data).map_err(|_| malformed("invalid event fields"))
        };
        match kind {
            "response.created" | "response.in_progress" | "response.queued" => {
                let event = decode()?;
                let mut response = required(event.response, "event missing response")?;
                self.update_usage(&mut response, events)?;
                for (i, item) in response.output.iter().flatten().enumerate() {
                    Self::check_item(item, i)?;
                    let state = self
                        .items
                        .get(&i)
                        .ok_or_else(|| malformed("snapshot contains unbegun item"))?;
                    if state.anchor.id != item.id || state.anchor.kind != item.kind {
                        return Err(malformed("snapshot item identity mismatch"));
                    }
                }
            }
            "response.completed" => {
                let event = decode()?;
                self.completed(required(event.response, "event missing response")?, events)?
            }
            "response.incomplete" => {
                let event = decode()?;
                self.incomplete(required(event.response, "event missing response")?, events)?;
            }
            "response.failed" => {
                let event = decode()?;
                let mut response = required(event.response, "event missing response")?;
                self.update_usage(&mut response, events)?;
                let error = required(response.error, "failed response missing error")?;
                return Err(api_error(error.code.as_deref(), error.message.as_deref()));
            }
            "error" => {
                let event = decode()?;
                return Err(api_error(event.code.as_deref(), event.message.as_deref()));
            }
            "response.output_item.added" | "response.output_item.done" => {
                let event = decode()?;
                let index = required(event.output_index, "missing output_index")?;
                let item = required(event.item, "missing item")?;
                if kind.ends_with("added") {
                    self.add_item(index, &item, events)?;
                } else {
                    self.finish_item(index, item, false, events)?;
                }
            }
            "response.content_part.added"
            | "response.content_part.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done" => {
                let event = decode()?;
                let index = required(event.output_index, "missing output_index")?;
                self.route(index, event.item_id.as_deref())?;
                let summary = kind.contains("summary");
                let part_index = required(
                    if summary {
                        event.summary_index
                    } else {
                        event.content_index
                    },
                    "missing content index",
                )?;
                let content = required(event.part, "missing content part")?;
                if self.items[&index].anchor.kind == "function_call" {
                    return Err(malformed("content on function item"));
                }
                let key = content_key(
                    &content,
                    part_index,
                    self.items[&index].anchor.kind == "reasoning",
                    summary,
                )?;
                if kind.ends_with("added") {
                    if self.items[&index].contents.contains_key(&key) {
                        return Err(malformed("duplicate content begin"));
                    }
                    self.begin_segment(index, key, events)?;
                    let text = content_text(&content)?;
                    if !text.is_empty() {
                        self.text(index, key, text, false, events)?;
                    }
                } else {
                    self.text(index, key, content_text(&content)?, true, events)?;
                }
            }
            "response.output_text.delta"
            | "response.output_text.done"
            | "response.refusal.delta"
            | "response.refusal.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.delta"
            | "response.reasoning_text.done" => {
                let event = decode()?;
                let index = required(event.output_index, "missing output_index")?;
                self.route(index, event.item_id.as_deref())?;
                let (content_kind, part_index) = if kind.contains("reasoning_summary") {
                    (ContentKind::SummaryText, event.summary_index)
                } else if kind.contains("reasoning_text") {
                    (ContentKind::ReasoningText, event.content_index)
                } else if kind.contains("refusal") {
                    (ContentKind::Refusal, event.content_index)
                } else {
                    (ContentKind::OutputText, event.content_index)
                };
                let key = PartKey {
                    kind: content_kind,
                    index: required(part_index, "missing content index")?,
                };
                let done = kind.ends_with("done");
                let text = required(
                    if !done {
                        event.delta
                    } else if content_kind == ContentKind::Refusal {
                        event.refusal
                    } else {
                        event.text
                    },
                    "missing text",
                )?;
                self.text(index, key, &text, done, events)?;
            }
            "response.function_call_arguments.delta" | "response.function_call_arguments.done" => {
                let event = decode()?;
                let index = required(event.output_index, "missing output_index")?;
                self.route(index, event.item_id.as_deref())?;
                let done = kind.ends_with("done");
                self.arguments(
                    index,
                    required(
                        if done { event.arguments } else { event.delta },
                        "missing arguments",
                    )?,
                    done,
                    events,
                )?;
            }
            "response.output_text.annotation.added" => {
                let event = decode()?;
                let index = required(event.output_index, "missing output_index")?;
                self.route(index, event.item_id.as_deref())?;
                let key = PartKey {
                    kind: ContentKind::OutputText,
                    index: required(event.content_index, "missing content index")?,
                };
                if !self.items[&index].contents.contains_key(&key) {
                    return Err(malformed("annotation without content"));
                }
            }
            _ if kind.ends_with(".delta") => {
                let event = decode()?;
                return Err(unsupported(event.output_index.unwrap_or(0), kind));
            }
            _ => {}
        }
        Ok(())
    }
}

fn api_error(code: Option<&str>, message: Option<&str>) -> ProtocolEvent {
    let retryable = matches!(
        code,
        Some(
            "server_error" | "rate_limit_exceeded" | "rate_limit_error" | "temporarily_unavailable"
        )
    );
    ProtocolEvent::error(
        format!(
            "Responses {}: {}",
            code.unwrap_or("error"),
            message.unwrap_or("provider error")
        ),
        retryable,
    )
}

#[cfg(test)]
mod tests;
