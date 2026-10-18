// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 视图推送与查询结果的测试数据：会话头、对话条目与生成中的文字。
use super::*;
use serde_json::{Value, json};

pub(super) const SESSION: &str = "11111111111141118111111111111111";
const AT: &str = "2026-01-01T00:00:00Z";
const DIGEST: &str = "5fd582703776c899a2ecb12f3d699bd7244dff1f443ef5ae3f0067bcf3ce279e";

pub(super) fn sid() -> SessionId {
    SessionId::parse(SESSION).unwrap()
}

pub(super) fn test_app(existing: bool) -> App {
    App::new(
        SessionContext {
            session_id: existing.then(sid),
            active_agent: "unused".into(),
        },
        false,
        Path::new("/nonexistent-tui-test-history"),
    )
}

pub(super) fn head(position: u64, turn_id: Option<TurnId>, owner: bool) -> SessionHead {
    SessionHead {
        session_id: sid(),
        position: NodeId(position),
        agent: Some("unused".into()),
        turn_id,
        owner,
        diagnostic: None,
        path_replaced: false,
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

pub(super) fn user(node: u64, turn_id: TurnId, ordinal: u64, text: &str) -> ConversationEntry {
    serde_json::from_value(json!({
        "kind": "user_message",
        "node": node,
        "ordinal": ordinal,
        "content": {
            "kind":"user_message","parent":node.saturating_sub(1),"turn_id":turn_id,"accepted_at":AT,
            "agent_name":"unused","model":"model-a","provider":"openai",
            "limits":{"timeout_secs":300,"max_model_steps":32,"max_model_attempts":3},
            "config_digest":"0".repeat(64),
            "message":{"role":"user","parts":[{"semantic":{"text":{"text":text}}}]}
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
    json!({"provider_error":"HTTP 429","partial":[],"invalid_batch":null})
}

impl StepEntry {
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
        self.attempts
            .push(json!({"usage":null,"progress":{"progress":"sent"},"retry_delay_ms":null}));
        self
    }
    /// 一次失败并安排了重试的尝试，带后端给出的重试提示。
    pub(super) fn retried(mut self, level: &str, text: &str) -> Self {
        let attempt = self.attempts.len();
        self.attempts.push(json!({
            "usage":null,"retry_delay_ms":100,
            "progress":{"progress":"ended","end":{"end":"stopped","cause":{"cause":"provider_error","retryable":true}},"diagnostic":diagnostic()}
        }));
        self.retries
            .push(json!({"attempt":attempt,"delay_ms":100,"notice":{"level":level,"text":text}}));
        self
    }
    pub(super) fn completed_attempt(mut self, total_tokens: u64) -> Self {
        self.attempts.push(json!({
            "usage":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":total_tokens},"retry_delay_ms":null,
            "progress":{"progress":"ended","end":{"end":"completed"},"diagnostic":diagnostic()}
        }));
        self
    }
    /// 回复末尾加一个工具调用；`terminal` 时槽位已有成功结果，否则停在 Started。
    pub(super) fn tool(mut self, n: u32, terminal: bool) -> Self {
        let id = invocation(n);
        let parts = self.parts.get_or_insert_with(Vec::new);
        let part = parts.len();
        parts.push(json!({"semantic":{"tool_call":{"call":{
            "invocation_id":id,"provider_call_id":format!("call_{n}"),"name":"observe.workspace",
            "arguments":{"path":"a.txt"},"arguments_digest":DIGEST,
            "address":{"message":{"session_id":SESSION,"turn_id":self.turn_id,"model_step_ordinal":self.ordinal},"part_ordinal":part}
        }}}}));
        let progress = if terminal {
            json!({"progress":"terminal","result":{
                "invocation_id":id,"name":"observe.workspace",
                "payload":{"outcome":"succeeded","payload":{"kind":"json","value":null,
                    "digest":"4514e821d251d896aaed09c92f07abacecb5683e0714471682fa6beaca071aa2","byte_length":4}}
            },"evidence":{"kind":"unavailable","value":{"reason":"no_result"}}})
        } else {
            json!({"progress":"started"})
        };
        self.slots.push(json!({
            "binding":{"invocation_id":id,"identity":{
                "session_id":SESSION,"turn_id":self.turn_id,"model_step_ordinal":self.ordinal,"part_ordinal":part,
                "method":"observe.workspace","descriptor_identity":"observe.workspace","arguments_digest":DIGEST}},
            "progress":progress
        }));
        self
    }
    pub(super) fn entry(self) -> ConversationEntry {
        let end = match self.progress.get("end") {
            Some(end) => end.clone(),
            None => json!({"end":"stop","cause":{"cause":"process_exit"}}),
        };
        let reply = self
            .parts
            .map(|parts| json!({"role":"assistant","origin":"open_ai_compatible","parts":parts}));
        let mut value = json!({
            "kind": "step",
            "node": self.node,
            "content": {
                "kind":"step","parent":self.node.saturating_sub(1),"turn_id":self.turn_id,
                "step_ordinal":self.ordinal,"progress":self.progress,"attempts":self.attempts,
                "reply":reply,"slots":self.slots,"updated_at":AT
            },
            "step": {
                "node":self.node,"key":{"turn_id":self.turn_id,"ordinal":self.ordinal},
                "progress":self.progress,"end":end,"attempts":[],"slots":[],"updated_at":AT
            },
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
    SubmissionResult::Accepted {
        turn: turn_view(turn_id),
    }
}

pub(super) fn turn_view(turn_id: TurnId) -> lattice_api::local::TurnView {
    serde_json::from_value(json!({
        "session_id":SESSION,"turn_id":turn_id,"user_node":1,"agent_name":"unused",
        "accepted_at":AT,"ended_at":AT,"summary":summary("failed", json!({"progress":"reserved"})),
        "usage":{"kind":"partial","known":{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0},"unknown_attempts":1}
    }))
    .unwrap()
}

pub(super) fn turn_detail(turn_id: TurnId) -> lattice_api::local::TurnDetail {
    let mut value = serde_json::to_value(turn_view(turn_id)).unwrap();
    value["steps"] = json!([]);
    serde_json::from_value(value).unwrap()
}

pub(super) fn local_error(kind: lattice_api::local::LocalErrorKind, message: &str) -> LocalError {
    serde_json::from_value(json!({"kind":kind,"message":message,"recovery":null,"store":null}))
        .unwrap()
}

/// 已关闭、以完成结束的一轮：会话头指向它且没有 Owner。
pub(super) fn finish(app: &mut App, turn_id: TurnId, text: &str) {
    end(app, turn_id, text, None);
}

/// 以 Stop 关闭的一轮；`outcome` 为 failed 或 cancelled。
pub(super) fn stop(app: &mut App, turn_id: TurnId, text: &str, outcome: &str) {
    end(app, turn_id, text, Some(outcome));
}

fn end(app: &mut App, turn_id: TurnId, text: &str, stopped: Option<&str>) {
    let node = app.entries.keys().last().map_or(1, |node| node.0 + 1);
    let ordinal = app
        .entries
        .values()
        .filter(|entry| matches!(entry, ConversationEntry::UserMessage { .. }))
        .count() as u64
        + 1;
    let step = step(node + 1, turn_id, 0).text("live body");
    let step = match stopped {
        Some(outcome) => step.stopped(outcome),
        None => step.finished(),
    };
    app.apply_view(LocalUpdate::Head(head(node + 1, Some(turn_id), false)));
    app.apply_view(entry_update(user(node, turn_id, ordinal, text)));
    app.apply_view(entry_update(step.entry()));
}
