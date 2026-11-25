// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 视图推送与查询结果的测试数据：会话头、对话条目与生成中的文字。
use super::*;
use serde_json::{Value, json};

pub(super) const SESSION: &str = "11111111111141118111111111111111";
const AT: &str = "2026-01-01T00:00:00Z";
const DIGEST: &str = "5fd582703776c899a2ecb12f3d699bd7244dff1f443ef5ae3f0067bcf3ce279e";

pub(super) fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    use unicode_width::UnicodeWidthStr;
    let mut text = String::new();
    for row in buffer.content.chunks(usize::from(buffer.area.width).max(1)) {
        let mut column = 0;
        while column < row.len() {
            let symbol = row[column].symbol();
            text.push_str(symbol);
            column += symbol.width().max(1);
        }
        text.push('\n');
    }
    text
}

pub(super) fn model_candidate(
    provider: &str,
    model: &str,
    efforts: &[lattice_local::Effort],
) -> lattice_local::ModelCandidate {
    lattice_local::ModelCandidate {
        provider: lattice_local::ProviderId::parse(provider).unwrap(),
        model: lattice_local::ModelId::parse(model).unwrap(),
        name: None,
        context: None,
        output: None,
        input: None,
        supported_efforts: efforts.to_vec(),
        cost: None,
        availability: lattice_local::Availability::Ready,
    }
}

pub(super) fn sid() -> SessionId {
    SessionId::parse(SESSION).unwrap()
}

pub(super) fn settings_version(byte: u8) -> lattice_local::SettingsVersion {
    serde_json::from_value(json!(format!("{byte:02x}").repeat(32))).unwrap()
}

pub(super) fn test_app(_existing: bool) -> App {
    App::new(
        SessionContext {
            session_id: sid(),
            initial_head: head(0, None, false),
            initial_adjustment: None,
        },
        false,
        Path::new("/nonexistent-tui-test-history"),
        std::path::PathBuf::from("/nonexistent-tui-test-workspace"),
    )
}

pub(super) fn head(position: u64, turn_id: Option<TurnId>, owner: bool) -> SessionHead {
    SessionHead {
        model: Some(ModelId::parse("model-a").unwrap()),
        provider: Some(ProviderId::parse("openai").unwrap()),
        effort: None,
        settings_version: settings_version(0x11),
        session_id: sid(),
        position: NodeId(position),
        agent: "unused".into(),
        turn_id,
        owner,
        diagnostic: None,
        path_replaced: false,
        has_older: None,
    }
}

/// 新视图的开头：首条会话头之后是初始条目，到会话头当前位置的条目为止。
pub(super) fn open_view(app: &mut App, head: SessionHead, entries: Vec<ConversationEntry>) {
    app.begin_view();
    app.apply_view(LocalUpdate::Head(head));
    for entry in entries {
        app.apply_view(entry_update(entry));
    }
}

pub(super) fn entry_update(entry: ConversationEntry) -> LocalUpdate {
    LocalUpdate::Entry {
        entry: Box::new(entry),
    }
}

pub(super) fn text(
    node: u64,
    attempt: u32,
    offset: usize,
    segments: &[(&str, bool)],
) -> LocalUpdate {
    LocalUpdate::Text(TextUpdate {
        node: NodeId(node),
        attempt,
        offset,
        segments: segments
            .iter()
            .map(|(text, reasoning)| {
                let text = (*text).to_owned();
                if *reasoning {
                    TextSegment::Reasoning { text }
                } else {
                    TextSegment::Text { text }
                }
            })
            .collect(),
    })
}

pub(super) fn user(node: u64, _turn_id: TurnId, _ordinal: u64, text: &str) -> ConversationEntry {
    serde_json::from_value(json!({
        "kind": "input",
        "node": node,
        "accepted_at": AT,
        "input": {
            "channel": "tui",
            "message": {"role": "user", "parts": [{"semantic": {"text": {"text": text}}}]}
        }
    }))
    .unwrap()
}

pub(super) fn context_op(node: u64, target: u64) -> ConversationEntry {
    serde_json::from_value(json!({
        "kind": "context_op",
        "node": node,
        "accepted_at": AT,
        "op": {
            "channel": "tui",
            "edits": [{"kind": "omit_node", "node": target}]
        }
    }))
    .unwrap()
}

pub(super) fn numbered_turn(n: u64) -> TurnId {
    TurnId::parse(&format!("44444444-4444-4444-8444-{n:012}")).unwrap()
}

/// 完整的若干轮，第 n 轮的节点为 `2n - 1` 与 `2n`；每轮以失败结束，结局带 Error 级 notice。
pub(super) fn turns(range: std::ops::RangeInclusive<u64>) -> Vec<ConversationEntry> {
    range
        .flat_map(|n| {
            [
                user(2 * n - 1, numbered_turn(n), n, &format!("prompt-{n}")),
                step(2 * n, numbered_turn(n), 0)
                    .turn_ordinal(n)
                    .text(&format!("answer-{n}"))
                    .stopped("failed")
                    .entry(),
            ]
        })
        .collect()
}

pub(super) fn invocation(n: u32) -> String {
    format!("22222222-2222-4222-8222-{n:012}")
}

/// Step 条目；默认是刚预留、仍在请求中的最后一步。
pub(super) struct StepEntry {
    node: u64,
    turn_id: TurnId,
    ordinal: u32,
    progress: Value,
    parts: Option<Vec<Value>>,
    attempts: Vec<Value>,
    slots: Vec<Value>,
    results: Vec<Value>,
    retries: Vec<Value>,
    summary: Option<Value>,
}

pub(super) fn step(node: u64, turn_id: TurnId, ordinal: u32) -> StepEntry {
    StepEntry {
        node,
        turn_id,
        ordinal,
        progress: json!({"progress":"requesting"}),
        parts: None,
        attempts: Vec::new(),
        slots: Vec::new(),
        results: Vec::new(),
        retries: Vec::new(),
        summary: Some(summary("failed", json!({"progress":"requesting"}))),
    }
}

fn summary(outcome: &str, progress: Value) -> Value {
    let notice = match outcome {
        "failed" => json!({"level":"error","text":"provider failure"}),
        "cancelled" => json!({"level":"info","text":"cancelled by user"}),
        _ => Value::Null,
    };
    json!({"outcome":outcome,"notice":notice,"progress":progress})
}

fn diagnostic() -> Value {
    json!("HTTP 429")
}

impl StepEntry {
    pub(super) fn turn_ordinal(self, _ordinal: u64) -> Self {
        self
    }
    pub(super) fn text(self, text: &str) -> Self {
        self.parts(json!([{"semantic":{"text":{"text":text}}}]))
    }
    pub(super) fn parts(mut self, parts: Value) -> Self {
        self.parts = Some(parts.as_array().unwrap().clone());
        self
    }
    /// 以 Finish 关闭，这一轮完成。
    pub(super) fn finished(mut self) -> Self {
        let progress = json!({"progress":"closed","end":{"end":"finish"}});
        self.summary = Some(summary("completed", progress.clone()));
        self.progress = progress;
        self
    }
    /// 以 Stop 关闭；`outcome` 为 failed 或 cancelled。
    pub(super) fn stopped(mut self, outcome: &str) -> Self {
        let cause = if outcome == "cancelled" {
            "cancelled"
        } else {
            "process_exit"
        };
        let progress = json!({"progress":"closed","end":{"end":"stop","cause":{"cause":cause}}});
        self.summary = Some(summary(outcome, progress.clone()));
        self.progress = progress;
        self
    }
    /// 以 Continue 关闭，不是最后一步，没有结局。
    pub(super) fn continued(mut self) -> Self {
        self.progress = json!({"progress":"closed","end":{"end":"continue"}});
        self.summary = None;
        self
    }
    pub(super) fn executing(mut self) -> Self {
        self.progress = json!({"progress":"executing"});
        self.summary = Some(summary("failed", self.progress.clone()));
        self
    }
    pub(super) fn sent(mut self) -> Self {
        let attempt = self.attempts.len();
        self.attempts.push(json!({
            "attempt": attempt,
            "origin": {"protocol":"chat-completions","provider_id":"openai"},
            "usage": null,
            "usage_kind": "partial",
            "cost": null,
            "progress": {"progress":"sent"},
            "end": {"end":"stopped","cause":{"cause":"process_exit"},"diagnostic":null},
            "retry_delay_ms": null
        }));
        self
    }
    /// 一次失败并安排了重试的尝试，带后端给出的重试提示。
    pub(super) fn retried(mut self, level: &str, text: &str) -> Self {
        let attempt = self.attempts.len();
        self.attempts.push(json!({
            "attempt": attempt,
            "origin": {"protocol":"chat-completions","provider_id":"openai"},
            "usage": null,
            "usage_kind": "partial",
            "cost": null,
            "retry_delay_ms": 100,
            "progress": {"progress":"ended","end":{"end":"stopped","cause":{"cause":"provider_error","retryable":true}},"diagnostic":diagnostic()},
            "end": {"end":"stopped","cause":{"cause":"provider_error","retryable":true},"diagnostic":diagnostic()}
        }));
        self.retries
            .push(json!({"attempt":attempt,"delay_ms":100,"notice":{"level":level,"text":text}}));
        self
    }
    pub(super) fn completed_attempt(mut self, context_tokens: u64) -> Self {
        let attempt = self.attempts.len();
        self.attempts.push(json!({
            "attempt": attempt,
            "origin": {"protocol":"chat-completions","provider_id":"openai"},
            "usage_kind": "partial",
            "cost": null,
            "usage": {"input":context_tokens,"output":0,"cache_read":null,"cache_write":null,"reasoning":null},
            "retry_delay_ms": null,
            "progress": {"progress":"ended","end":{"end":"completed"},"diagnostic":diagnostic()},
            "end": {"end":"completed","step":{"session_id":SESSION,"turn_id":self.turn_id,"model_step_ordinal":self.ordinal}}
        }));
        self
    }
    /// 最近一次尝试保存的计量与判定；缺失字段保持未报告。
    pub(super) fn metering(
        mut self,
        usage: Value,
        usage_kind: lattice_local::UsageCompleteness,
        cost: Option<(f64, bool)>,
    ) -> Self {
        let attempt = self.attempts.last_mut().expect("已有尝试");
        attempt["usage"] = usage;
        attempt["usage_kind"] = json!(usage_kind);
        attempt["cost"] =
            json!(cost.map(|(usd, exact)| lattice_local::CostEstimate { usd, exact }));
        self
    }
    /// 回复末尾加一个工具调用；`terminal` 时槽位已有成功结果，否则停在 Started。
    pub(super) fn tool(self, n: u32, terminal: bool) -> Self {
        let id = invocation(n);
        let (slot_progress, outcome, reason, material, result_payload) = if terminal {
            let material = json!({
                "kind": "available",
                "digest": "4514e821d251d896aaed09c92f07abacecb5683e0714471682fa6beaca071aa2",
                "byte_length": 4
            });
            (
                json!({
                    "state": "terminal",
                    "outcome": "succeeded",
                    "reason": null,
                    "material": material
                }),
                "succeeded",
                Value::Null,
                material,
                json!({
                    "outcome": "succeeded",
                    "payload": {
                        "kind": "json",
                        "value": null,
                        "digest": "4514e821d251d896aaed09c92f07abacecb5683e0714471682fa6beaca071aa2",
                        "byte_length": 4
                    }
                }),
            )
        } else {
            (
                json!({"state": "started"}),
                "indeterminate",
                json!({"kind": "indeterminate", "value": "no_result"}),
                json!({"kind": "no_content"}),
                json!({"outcome": "indeterminate", "payload": "no_result"}),
            )
        };
        self.push_tool_slot(
            n,
            id,
            slot_progress,
            outcome,
            reason,
            material,
            result_payload,
        )
    }
    /// 回复末尾加一个以 `ToolError` 终结的失败工具调用。
    pub(super) fn failed_tool(self, n: u32, code: &str, message: &str) -> Self {
        let id = invocation(n);
        let reason = json!({"kind": "failed", "value": {"code": code}});
        let material = json!({"kind": "no_content"});
        let slot_progress = json!({
            "state": "terminal",
            "outcome": "failed",
            "reason": reason,
            "material": material
        });
        let result_payload = json!({
            "outcome": "failed",
            "payload": {"code": code, "message": message}
        });
        self.push_tool_slot(
            n,
            id,
            slot_progress,
            "failed",
            reason,
            material,
            result_payload,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn push_tool_slot(
        mut self,
        n: u32,
        id: String,
        progress: Value,
        outcome: &str,
        reason: Value,
        material: Value,
        result_payload: Value,
    ) -> Self {
        let parts = self.parts.get_or_insert_with(Vec::new);
        let part = parts.len();
        parts.push(json!({"semantic":{"tool_call":{"call":{
            "invocation_id":id,"provider_call_id":format!("call_{n}"),"name":"observe__workspace",
            "arguments":{"path":"a.txt"},"arguments_digest":DIGEST,
            "address":{"message":{"session_id":SESSION,"turn_id":self.turn_id,"model_step_ordinal":self.ordinal},"part_ordinal":part}
        }}}}));
        self.slots.push(json!({
            "invocation_id": id,
            "binding": {
                "session_id": SESSION,
                "turn_id": self.turn_id,
                "model_step_ordinal": self.ordinal,
                "part_ordinal": part,
                "method": "observe__workspace",
                "tool_identity": "observe__workspace",
                "arguments_digest": DIGEST
            },
            "progress": progress,
            "outcome": outcome,
            "reason": reason,
            "material": material
        }));
        self.results.push(json!({
            "invocation_id": id,
            "name": "observe__workspace",
            "payload": result_payload
        }));
        self
    }
    pub(super) fn entry(self) -> ConversationEntry {
        let end = match self.progress.get("end") {
            Some(end) => end.clone(),
            None => json!({"end":"stop","cause":{"cause":"process_exit"}}),
        };
        let reply = self.parts.map(|parts| {
            json!({
                "assistant": {
                    "role": "assistant",
                    "provider_origin": {"protocol":"chat-completions","provider_id":"openai"},
                    "parts": parts
                },
                "results": self.results
            })
        });
        let mut value = json!({
            "kind": "step",
            "node": self.node,
            "step": {
                "node": self.node,
                "key": {"turn_id": self.turn_id, "ordinal": self.ordinal},
                "progress": self.progress,
                "end": end,
                "attempts": self.attempts,
                "slots": self.slots,
                "updated_at": AT
            },
            "agent_name": (self.ordinal == 0).then_some("unused"),
            "reply": reply,
            "retries": self.retries,
        });
        if let Some(summary) = self.summary {
            value["summary"] = summary;
        }
        serde_json::from_value(value).unwrap()
    }
}

pub(super) fn transcript(app: &App) -> String {
    crate::view::transcript_lines(app, 200)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// 文字在对话记录中出现的行号；要求恰好出现一次。
pub(super) fn row(app: &App, marker: &str) -> usize {
    let lines = crate::view::transcript_lines(app, 200)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let rows = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(marker))
        .map(|(row, _)| row)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{marker} 应恰好出现一次：{lines:#?}");
    rows[0]
}

pub(super) fn status(level: NoticeLevel, text: &str) -> Status {
    Status {
        level,
        text: text.into(),
        closing: None,
        retry: None,
    }
}

pub(super) fn receipt(turn_id: TurnId, result: SubmissionResult) -> LocalUpdate {
    LocalUpdate::Submit {
        update: SubmissionUpdate::Receipt(SubmissionReceipt {
            turn_id,
            result,
            recovered: false,
        }),
    }
}

pub(super) fn accepted(turn_id: TurnId) -> SubmissionResult {
    accepted_at(turn_id, &[1], 2)
}

pub(super) fn accepted_at(
    turn_id: TurnId,
    input_nodes: &[u64],
    start_node: u64,
) -> SubmissionResult {
    SubmissionResult::Accepted {
        turn: turn_view_at(turn_id, input_nodes, start_node),
    }
}

pub(super) fn turn_view(turn_id: TurnId) -> lattice_local::TurnView {
    turn_view_at(turn_id, &[1], 2)
}

pub(super) fn turn_view_at(
    turn_id: TurnId,
    input_nodes: &[u64],
    start_node: u64,
) -> lattice_local::TurnView {
    serde_json::from_value(json!({
        "session_id":SESSION,"turn_id":turn_id,"start_node":start_node,"input_nodes":input_nodes,"agent_name":"unused",
        "accepted_at":AT,"ended_at":AT,"summary":summary("failed", json!({"progress":"reserved"})),
        "usage":lattice_local::UsageSummary::from_attempts([])
    }))
    .unwrap()
}

pub(super) fn turn_detail(turn_id: TurnId) -> lattice_local::TurnDetail {
    let mut value = serde_json::to_value(turn_view(turn_id)).unwrap();
    value["steps"] = json!([]);
    serde_json::from_value(value).unwrap()
}

pub(super) fn local_error(kind: lattice_local::LocalErrorKind, message: &str) -> LocalError {
    serde_json::from_value(json!({"kind":kind,"message":message,"recovery":null,"store":null}))
        .unwrap()
}
