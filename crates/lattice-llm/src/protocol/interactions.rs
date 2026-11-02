// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Interactions 的无状态请求、step 路由与逐片段回放。

use std::collections::BTreeMap;

use lattice_conversation::{Protocol, ReplayOrigin, canonical_json};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::projection::{ProjectionError, request_messages_with_replay};
use crate::reasoning::ReasoningPlan;
use crate::reply::{PartDelta, PartKind, ProtocolEvent, StringPatch};
use crate::types::ChatRequest;

pub(crate) fn build_request_json(
    request: &ChatRequest,
    reasoning: Option<ReasoningPlan>,
    model_reasoning: Option<bool>,
    origin: &ReplayOrigin,
) -> Result<Value, ProjectionError> {
    let projected =
        request_messages_with_replay(origin, &request.messages, super::ReasoningReplay::None)?;
    let mut body = json!({"model":request.target.model,"input":projected.messages,"stream":true,"store":false});
    if let Some(system) = projected.system {
        body["system_instruction"] = system.into();
    }
    let mut config = json!({});
    if let Some(ReasoningPlan::Effort { effort, .. }) = reasoning {
        config["thinking_level"] = effort.as_str().into();
    }
    if model_reasoning != Some(false) {
        config["thinking_summaries"] = "auto".into();
    }
    if config.as_object().is_some_and(|fields| !fields.is_empty()) {
        body["generation_config"] = config;
    }
    if !request.tools.is_empty() {
        body["tools"] = request.tools.iter().map(|tool| json!({
            "type":"function","name":tool.name,"description":tool.description,"parameters":tool.parameters,
        })).collect();
    }
    Ok(body)
}

#[derive(Deserialize)]
struct EventType {
    event_type: String,
}

#[derive(Deserialize)]
struct Event {
    interaction: Option<Interaction>,
    interaction_id: Option<String>,
    status: Option<String>,
    index: Option<usize>,
    step: Option<Step>,
    delta: Option<Delta>,
    usage: Option<Value>,
    metadata: Option<DeltaMetadata>,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct DeltaMetadata {
    total_usage: Option<Value>,
}

#[derive(Deserialize)]
struct Interaction {
    id: Option<String>,
    status: Option<String>,
    steps: Option<Vec<StepType>>,
    usage: Option<Value>,
    #[serde(default)]
    errors: Vec<ApiError>,
}

// 终态快照只核对数量和类型，不再次解码或投递正文。
#[derive(Deserialize)]
struct StepType {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct Step {
    #[serde(rename = "type")]
    kind: String,
    content: Option<Vec<Content>>,
    summary: Option<Vec<Content>>,
    signature: Option<String>,
    name: Option<String>,
    id: Option<String>,
    arguments: Option<Value>,
}

#[derive(Deserialize)]
struct Content {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Delta {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    content: Option<Content>,
    signature: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ApiError {
    code: Option<String>,
    message: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ContentKind {
    Text,
    Summary,
    Empty,
    FunctionCall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartKey {
    pub kind: ContentKind,
    pub index: usize,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Anchor {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReplayState {
    pub step: usize,
    pub part: PartKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
}

struct StepState {
    anchor: Anchor,
    slots: Vec<(usize, PartKey)>,
    next_content: usize,
    initial_arguments: bool,
    argument_delta: bool,
    stopped: bool,
}

#[derive(Default)]
pub(crate) struct InteractionsSseParser {
    steps: BTreeMap<usize, StepState>,
    interaction_id: Option<String>,
    created: bool,
    current: Option<(usize, PartKey, usize)>,
    next_index: usize,
    usage: super::usage::UsageSnapshot,
    stopped: bool,
}

fn malformed(detail: &'static str) -> ProtocolEvent {
    ProtocolEvent::error(format!("malformed Interactions wire: {detail}"), false)
}

fn required<T>(value: Option<T>, detail: &'static str) -> Result<T, ProtocolEvent> {
    value.ok_or_else(|| malformed(detail))
}

fn supported_step(kind: &str, index: usize) -> Result<(), ProtocolEvent> {
    match kind {
        "model_output" | "thought" | "function_call" => Ok(()),
        _ => Err(ProtocolEvent::unsupported("interactions", index, kind)),
    }
}

impl InteractionsSseParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn parse_chunk(
        &mut self,
        event_name: Option<&str>,
        data: &str,
    ) -> Vec<ProtocolEvent> {
        if self.stopped || data.trim().is_empty() || data.trim() == "[DONE]" {
            return Vec::new();
        }
        let mut events = Vec::new();
        let result = serde_json::from_str::<EventType>(data)
            .map_err(|_| malformed("invalid event type"))
            .and_then(|event| {
                // 未知顶层事件不解码其余字段，也不产生用量或正文。
                if !matches!(
                    event.event_type.as_str(),
                    "interaction.created"
                        | "interaction.status_update"
                        | "step.start"
                        | "step.delta"
                        | "step.stop"
                        | "interaction.completed"
                        | "error"
                ) {
                    return Ok(());
                }
                if event_name.is_some_and(|name| name != event.event_type) {
                    return Err(malformed("event name differs from event_type"));
                }
                let fields = serde_json::from_str::<Event>(data)
                    .map_err(|_| malformed("invalid event fields"))?;
                self.event(&event.event_type, fields, &mut events)
            });
        if let Err(error) = result {
            self.stopped = true;
            events.push(error);
        }
        events
    }

    fn update_usage(
        &mut self,
        usage: Option<Value>,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        if let Some(usage) = usage
            && let Some(usage) = self
                .usage
                .update(Protocol::Interactions, &json!({"usage":usage}))
                .map_err(|_| malformed("invalid usage"))?
        {
            events.push(ProtocolEvent::Usage(usage));
        }
        Ok(())
    }

    fn identity(&mut self, id: Option<String>) -> Result<(), ProtocolEvent> {
        if let (Some(old), Some(new)) = (&self.interaction_id, &id)
            && old != new
        {
            return Err(malformed("interaction id mismatch"));
        }
        if id.is_some() {
            self.interaction_id = id;
        }
        Ok(())
    }

    fn route(&self, index: usize) -> Result<&StepState, ProtocolEvent> {
        let step = self
            .steps
            .get(&index)
            .ok_or_else(|| malformed("step has not begun"))?;
        if step.stopped {
            return Err(malformed("step already stopped"));
        }
        Ok(step)
    }

    fn replay(&self, step_index: usize, key: PartKey, anchor: bool) -> String {
        serde_json::to_string(&ReplayState {
            step: step_index,
            part: key,
            anchor: anchor.then(|| self.steps[&step_index].anchor.clone()),
        })
        .expect("replay metadata serializes")
    }

    fn begin_segment(
        &mut self,
        step_index: usize,
        key: PartKey,
        events: &mut Vec<ProtocolEvent>,
    ) -> usize {
        if key.kind != ContentKind::Empty
            && let Some((current_step, current_key, index)) = self.current
            && current_step == step_index
            && current_key.kind == ContentKind::Empty
        {
            self.steps.get_mut(&step_index).unwrap().slots[0].1 = key;
            events.push(ProtocolEvent::Part(PartDelta::ReplayState {
                index,
                patch: StringPatch::Set(self.replay(step_index, key, true)),
            }));
            self.current = Some((step_index, key, index));
            return index;
        }
        let index = self.next_index;
        self.next_index += 1;
        let state = self.steps.get_mut(&step_index).unwrap();
        let anchor = state.slots.is_empty();
        let kind = match key.kind {
            ContentKind::FunctionCall => PartKind::ToolCall,
            ContentKind::Summary => PartKind::Reasoning,
            ContentKind::Empty if state.anchor.kind == "thought" => PartKind::Reasoning,
            _ => PartKind::Text,
        };
        state.slots.push((index, key));
        events.push(ProtocolEvent::Part(PartDelta::Begin { index, kind }));
        events.push(ProtocolEvent::Part(PartDelta::ReplayState {
            index,
            patch: StringPatch::Set(self.replay(step_index, key, anchor)),
        }));
        self.current = (kind != PartKind::ToolCall).then_some((step_index, key, index));
        index
    }

    fn text(&mut self, step: usize, key: PartKey, text: String, events: &mut Vec<ProtocolEvent>) {
        let index = match self.current {
            Some((current_step, current_key, index))
                if current_step == step && current_key == key =>
            {
                index
            }
            _ => self.begin_segment(step, key, events),
        };
        events.push(ProtocolEvent::Part(if key.kind == ContentKind::Summary {
            PartDelta::Reasoning { index, delta: text }
        } else {
            PartDelta::Text { index, delta: text }
        }));
    }

    fn content(
        &mut self,
        index: usize,
        content: Content,
        kind: ContentKind,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        if content.kind != "text" {
            return Err(ProtocolEvent::unsupported(
                "interactions",
                index,
                &content.kind,
            ));
        }
        let text = required(content.text, "text content missing text")?;
        let step = self.steps.get_mut(&index).unwrap();
        let key = PartKey {
            kind,
            index: step.next_content,
        };
        step.next_content += 1;
        self.text(index, key, text, events);
        Ok(())
    }

    fn start_step(
        &mut self,
        index: usize,
        step: Step,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        supported_step(&step.kind, index)?;
        if self.steps.contains_key(&index) {
            return Err(malformed("duplicate step start"));
        }
        if (step.kind != "thought" && (step.signature.is_some() || step.summary.is_some()))
            || (step.kind != "model_output" && step.content.is_some())
            || (step.kind != "function_call"
                && (step.name.is_some() || step.id.is_some() || step.arguments.is_some()))
        {
            return Err(malformed("step fields do not match type"));
        }
        self.steps.insert(
            index,
            StepState {
                anchor: Anchor {
                    kind: step.kind.clone(),
                    signature: step.signature,
                },
                slots: Vec::new(),
                next_content: 0,
                initial_arguments: false,
                argument_delta: false,
                stopped: false,
            },
        );
        let key = PartKey {
            kind: if step.kind == "function_call" {
                ContentKind::FunctionCall
            } else {
                ContentKind::Empty
            },
            index: 0,
        };
        let slot = self.begin_segment(index, key, events);
        if step.kind == "function_call" {
            let arguments = required(step.arguments, "function call missing arguments")?;
            let object = arguments
                .as_object()
                .ok_or_else(|| malformed("function arguments must be an object"))?;
            events.push(ProtocolEvent::Part(PartDelta::ToolMetadata {
                index: slot,
                name: Some(StringPatch::Set(required(
                    step.name,
                    "function call missing name",
                )?)),
                provider_call_id: Some(StringPatch::Set(required(
                    step.id,
                    "function call missing id",
                )?)),
            }));
            if !object.is_empty() {
                self.steps.get_mut(&index).unwrap().initial_arguments = true;
                events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                    index: slot,
                    delta: canonical_json(&arguments),
                }));
            }
        } else {
            let (content, kind) = if step.kind == "thought" {
                (step.summary, ContentKind::Summary)
            } else {
                (step.content, ContentKind::Text)
            };
            for content in content.into_iter().flatten() {
                self.content(index, content, kind, events)?;
            }
        }
        Ok(())
    }

    fn delta(
        &mut self,
        index: usize,
        delta: Delta,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        let step = self.route(index)?;
        match (step.anchor.kind.as_str(), delta.kind.as_str()) {
            ("model_output", "text") => {
                let key = PartKey {
                    kind: ContentKind::Text,
                    index: step.next_content.saturating_sub(1),
                };
                self.steps.get_mut(&index).unwrap().next_content = key.index + 1;
                self.text(
                    index,
                    key,
                    required(delta.text, "text delta missing text")?,
                    events,
                );
            }
            ("thought", "thought_summary") => self.content(
                index,
                required(delta.content, "summary delta missing content")?,
                ContentKind::Summary,
                events,
            )?,
            ("thought", "thought_signature") => {
                self.steps.get_mut(&index).unwrap().anchor.signature = Some(required(
                    delta.signature,
                    "signature delta missing signature",
                )?);
                let (slot, key) = self.steps[&index].slots[0];
                events.push(ProtocolEvent::Part(PartDelta::ReplayState {
                    index: slot,
                    patch: StringPatch::Set(self.replay(index, key, true)),
                }));
            }
            ("function_call", "arguments_delta") => {
                if step.initial_arguments {
                    return Err(malformed(
                        "arguments delta conflicts with initial arguments",
                    ));
                }
                let slot = step.slots[0].0;
                self.steps.get_mut(&index).unwrap().argument_delta = true;
                events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                    index: slot,
                    delta: required(delta.arguments, "arguments delta missing arguments")?,
                }));
            }
            (_, "text" | "thought_summary" | "thought_signature" | "arguments_delta") => {
                return Err(malformed("delta does not match step type"));
            }
            _ => {
                return Err(ProtocolEvent::unsupported(
                    "interactions",
                    index,
                    &delta.kind,
                ));
            }
        }
        Ok(())
    }

    fn stop_step(
        &mut self,
        index: usize,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        let step = self.route(index)?;
        let (slot, key) = step.slots[0];
        if step.anchor.kind == "function_call" && !step.initial_arguments && !step.argument_delta {
            events.push(ProtocolEvent::Part(PartDelta::ToolArguments {
                index: slot,
                delta: "{}".into(),
            }));
        }
        events.push(ProtocolEvent::Part(PartDelta::ReplayState {
            index: slot,
            patch: StringPatch::Set(self.replay(index, key, true)),
        }));
        let step = self.steps.get_mut(&index).unwrap();
        for &(index, _) in &step.slots {
            events.push(ProtocolEvent::Part(PartDelta::End { index }));
        }
        step.stopped = true;
        if self.current.is_some_and(|(current, _, _)| current == index) {
            self.current = None;
        }
        Ok(())
    }

    fn completed(
        &mut self,
        interaction: Interaction,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        self.update_usage(interaction.usage, events)?;
        self.identity(interaction.id)?;
        if let Some(steps) = interaction.steps.filter(|steps| !steps.is_empty()) {
            for (index, step) in steps.iter().enumerate() {
                supported_step(&step.kind, index)?;
            }
            if steps.len() != self.steps.len()
                || steps
                    .iter()
                    .zip(self.steps.values())
                    .any(|(snapshot, step)| snapshot.kind != step.anchor.kind)
            {
                return Err(malformed("completed steps differ from streamed steps"));
            }
        }
        let status = required(interaction.status, "interaction missing status")?;
        match status.as_str() {
            "completed" | "requires_action" | "incomplete" => {
                if status == "incomplete" {
                    let open: Vec<_> = self
                        .steps
                        .iter()
                        .filter_map(|(&index, step)| (!step.stopped).then_some(index))
                        .collect();
                    for index in open {
                        self.stop_step(index, events)?;
                    }
                } else {
                    if self.steps.values().any(|step| !step.stopped) {
                        return Err(malformed("completed interaction has open steps"));
                    }
                    let has_calls = self
                        .steps
                        .values()
                        .any(|step| step.anchor.kind == "function_call");
                    if has_calls != (status == "requires_action") {
                        return Err(malformed(
                            "interaction status does not match function calls",
                        ));
                    }
                }
                self.stopped = true;
                events.push(ProtocolEvent::Done {
                    finish_reason: match status.as_str() {
                        "requires_action" => "tool_calls",
                        "incomplete" => "incomplete",
                        _ => "stop",
                    }
                    .into(),
                });
            }
            "failed" => return Err(api_errors(&interaction.errors)),
            "cancelled" | "requires_continuation" => {
                return Err(ProtocolEvent::error(
                    format!("Interactions status: {status}"),
                    false,
                ));
            }
            _ => return Err(malformed("invalid terminal status")),
        }
        Ok(())
    }

    fn event(
        &mut self,
        kind: &str,
        event: Event,
        events: &mut Vec<ProtocolEvent>,
    ) -> Result<(), ProtocolEvent> {
        match kind {
            "interaction.created" => {
                let interaction = required(event.interaction, "missing interaction")?;
                self.update_usage(interaction.usage, events)?;
                self.identity(interaction.id)?;
                if self.created {
                    return Err(malformed("duplicate interaction created"));
                }
                self.created = true;
                if interaction.status.is_some() {
                    check_status(interaction.status.as_deref())?;
                }
                for (index, step) in interaction.steps.iter().flatten().enumerate() {
                    supported_step(&step.kind, index)?;
                }
            }
            "interaction.status_update" => {
                self.update_usage(event.usage, events)?;
                self.identity(event.interaction_id)?;
                check_status(event.status.as_deref())?;
            }
            "step.start" => self.start_step(
                required(event.index, "missing step index")?,
                required(event.step, "missing step")?,
                events,
            )?,
            "step.delta" => {
                if let Some(metadata) = event.metadata {
                    self.update_usage(metadata.total_usage, events)?;
                }
                self.delta(
                    required(event.index, "missing step index")?,
                    required(event.delta, "missing delta")?,
                    events,
                )?;
            }
            "step.stop" => {
                self.update_usage(event.usage, events)?;
                self.stop_step(required(event.index, "missing step index")?, events)?;
            }
            "interaction.completed" => {
                self.completed(required(event.interaction, "missing interaction")?, events)?
            }
            "error" => {
                self.update_usage(event.usage, events)?;
                return Err(api_errors(&[required(event.error, "missing error")?]));
            }
            _ => unreachable!("only known events are decoded"),
        }
        Ok(())
    }

    pub(super) fn full_reply(wire: &Value) -> Result<Vec<ProtocolEvent>, ProtocolEvent> {
        #[derive(Deserialize)]
        struct FullInteraction {
            steps: Vec<Step>,
        }
        let full: FullInteraction = serde_json::from_value(wire.clone())
            .map_err(|_| malformed("invalid interaction fields"))?;
        let interaction: Interaction = serde_json::from_value(wire.clone())
            .map_err(|_| malformed("invalid interaction fields"))?;
        let mut parser = Self::new();
        let mut events = Vec::new();
        for (index, step) in full.steps.into_iter().enumerate() {
            parser.start_step(index, step, &mut events)?;
            parser.stop_step(index, &mut events)?;
        }
        parser.completed(interaction, &mut events)?;
        Ok(events)
    }
}

fn check_status(status: Option<&str>) -> Result<(), ProtocolEvent> {
    match status {
        Some(
            "in_progress"
            | "requires_action"
            | "completed"
            | "failed"
            | "cancelled"
            | "incomplete"
            | "requires_continuation",
        ) => Ok(()),
        _ => Err(malformed("invalid interaction status")),
    }
}

fn retryable(code: Option<&str>) -> bool {
    matches!(
        code,
        Some(
            "rate_limit_exceeded"
                | "too_many_requests"
                | "api_error"
                | "service_unavailable"
                | "aborted"
                | "deadline_exceeded"
        )
    )
}

fn api_errors(errors: &[ApiError]) -> ProtocolEvent {
    let retryable =
        !errors.is_empty() && errors.iter().all(|error| retryable(error.code.as_deref()));
    let error = errors.first();
    ProtocolEvent::error(
        format!(
            "Interactions error {}: {}",
            error
                .and_then(|error| error.code.as_deref())
                .unwrap_or("unknown"),
            error
                .and_then(|error| error.message.as_deref())
                .unwrap_or("interaction failed")
        ),
        retryable,
    )
}

#[cfg(test)]
mod tests;
