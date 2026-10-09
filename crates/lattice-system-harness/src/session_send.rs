// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 通过公开 JSONL 驱动 Turn；业务结局、正文交付和原始进程结果分别保留。
use serde_json::Value;

use crate::driver::{CommandOutcome, DriverError, LatticeCommand, LatticeDriver, RunningCommand};
use crate::scenario::{ScenarioBudget, ScenarioSpec};
use crate::world::ScenarioWorld;

/// CLI 的 0/1/2 退出分类；2 同时用于前提拒绝与用户取消，须结合记录解释。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendExitCode {
    Committed,
    Failed,
    InputError,
}
impl SendExitCode {
    pub fn from_code(code: Option<i32>) -> Option<Self> {
        match code {
            Some(0) => Some(Self::Committed),
            Some(1) => Some(Self::Failed),
            Some(2) => Some(Self::InputError),
            _ => None,
        }
    }
    pub fn as_i32(self) -> i32 {
        match self {
            Self::Committed => 0,
            Self::Failed => 1,
            Self::InputError => 2,
        }
    }
}

/// 常用进度字段为类型化投影，其余记录保留完整 JSON；未知事件不等同于成功。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEventLine {
    TurnStarted {
        session_id: String,
        turn_id: String,
        user_node: u64,
    },
    ModelStarted {
        provider: String,
        model: String,
    },
    LlmRetry {
        key: Value,
        delay_ms: u64,
        notice: Value,
    },
    ReasoningDelta {
        content: String,
    },
    TokenDelta {
        content: String,
    },
    ToolCallStarted {
        id: String,
        name: String,
    },
    ToolCallFinished {
        id: String,
        name: String,
        ok: bool,
    },
    ModelFinished {
        finish_reason: Option<String>,
    },
    /// 包含原始 Step、summary 与身份，不从事件名重新计算持久结局。
    Terminal {
        type_name: String,
        raw: Value,
    },
    Unknown {
        type_name: String,
        raw: Value,
    },
}
impl TurnEventLine {
    pub fn type_name(&self) -> &str {
        match self {
            Self::TurnStarted { .. } => "turn_started",
            Self::ModelStarted { .. } => "model_started",
            Self::LlmRetry { .. } => "llm_retry",
            Self::ReasoningDelta { .. } => "reasoning_delta",
            Self::TokenDelta { .. } => "token_delta",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::ToolCallFinished { .. } => "tool_call_finished",
            Self::ModelFinished { .. } => "model_finished",
            Self::Terminal { type_name, .. } | Self::Unknown { type_name, .. } => type_name,
        }
    }
}
fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{field}` must be a string"))
}
fn uint(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`{field}` must be an unsigned integer"))
}
fn boolean(value: &Value, field: &str) -> Result<bool, String> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("`{field}` must be a boolean"))
}
fn array<'a>(value: &'a Value, field: &str) -> Result<&'a Vec<Value>, String> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("`{field}` must be an array"))
}
fn nullable_string(value: &Value, field: &str) -> Result<Option<String>, String> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(format!("`{field}` must be a string or null")),
    }
}
fn attempt_key(value: &Value) -> Result<(), String> {
    string(value, "turn_id")?;
    uint(value, "model_step_ordinal")?;
    uint(value, "attempt_ordinal")?;
    Ok(())
}
fn step_key(value: &Value) -> Result<(), String> {
    string(value, "turn_id")?;
    uint(value, "ordinal")?;
    Ok(())
}
fn turn_ref(value: &Value) -> Result<(), String> {
    string(value, "session_id")?;
    string(value, "turn_id")?;
    Ok(())
}
fn usage(value: &Value) -> Result<(), String> {
    if !value.is_null() {
        for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
            uint(value, key)?;
        }
    }
    Ok(())
}
fn summary(value: &Value) -> Result<&str, String> {
    let outcome = string(value, "outcome")?;
    match (outcome, value.get("notice")) {
        ("completed", Some(Value::Null)) => {}
        ("failed" | "cancelled", Some(notice)) => {
            let level = if outcome == "failed" { "error" } else { "info" };
            if string(notice, "level")? != level {
                return Err("summary notice level differs from outcome".into());
            }
            let text = string(notice, "text")?;
            if text.is_empty() || text.chars().count() > 2048 {
                return Err("invalid bounded notice".into());
            }
        }
        _ => return Err("invalid outcome or notice".into()),
    }
    Ok(outcome)
}
fn step(value: &Value) -> Result<(), String> {
    step_key(&value["key"])?;
    for attempt in array(value, "attempts")? {
        uint(attempt, "attempt")?;
        string(attempt, "origin")?;
        usage(attempt.get("usage").ok_or("missing usage")?)?;
        match string(&attempt["end"], "end")? {
            "completed" => {
                string(&attempt["end"]["step"], "turn_id")?;
                uint(&attempt["end"]["step"], "model_step_ordinal")?;
            }
            "stopped" => {
                if attempt["end"].get("cause").is_none() {
                    return Err("missing attempt cause".into());
                }
                nullable_string(&attempt["end"], "diagnostic")?;
            }
            _ => return Err("unknown attempt end".into()),
        }
    }
    let state = &value["state"];
    match string(state, "state")? {
        "requesting" => {
            uint(state, "attempt")?;
        }
        "retry_ready" => {
            uint(state, "next_attempt")?;
        }
        "executing" => {
            array(state, "slots")?;
        }
        "closed" => {
            array(state, "slots")?;
            let end = &state["end"];
            match string(end, "end")? {
                "continue" | "finish" => {
                    uint(end, "node")?;
                }
                "stop" => {
                    match end.get("node") {
                        Some(Value::Null) => {}
                        Some(n) if n.as_u64().is_some() => {}
                        _ => return Err("stop needs node or null".into()),
                    }
                    if end.get("cause").is_none_or(Value::is_null) {
                        return Err("stop needs cause".into());
                    }
                }
                _ => return Err("unknown step end".into()),
            }
        }
        _ => return Err("unknown step state".into()),
    }
    Ok(())
}
fn anchor(value: &Value) -> Result<(), String> {
    string(value, "session_id")?;
    uint(value, "head")?;
    Ok(())
}
fn parse_event_line_checked(line: &str) -> Result<Option<TurnEventLine>, String> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let kind = string(&value, "type")?.to_owned();
    let event = match kind.as_str() {
        "turn_started" => {
            turn_ref(&value["turn"])?;
            TurnEventLine::TurnStarted {
                session_id: string(&value["turn"], "session_id")?.into(),
                turn_id: string(&value["turn"], "turn_id")?.into(),
                user_node: uint(&value, "user_node")?,
            }
        }
        "model_started" => {
            attempt_key(&value["key"])?;
            TurnEventLine::ModelStarted {
                provider: string(&value, "provider")?.into(),
                model: string(&value, "model")?.into(),
            }
        }
        "token_delta" | "reasoning_delta" => {
            attempt_key(&value["key"])?;
            let content = string(&value, "content")?.into();
            if kind == "token_delta" {
                TurnEventLine::TokenDelta { content }
            } else {
                TurnEventLine::ReasoningDelta { content }
            }
        }
        "llm_retry" => {
            attempt_key(&value["key"])?;
            let delay_ms = uint(&value, "delay_ms")?;
            let notice = &value["notice"];
            if string(notice, "level")? != "warn" {
                return Err("retry notice must be warn".into());
            }
            string(notice, "text")?;
            TurnEventLine::LlmRetry {
                key: value["key"].clone(),
                delay_ms,
                notice: notice.clone(),
            }
        }
        "model_finished" => {
            attempt_key(&value["key"])?;
            usage(value.get("usage").ok_or("missing usage")?)?;
            TurnEventLine::ModelFinished {
                finish_reason: nullable_string(&value, "finish_reason")?,
            }
        }
        "tool_call_started" => TurnEventLine::ToolCallStarted {
            id: string(&value, "id")?.into(),
            name: string(&value, "name")?.into(),
        },
        "tool_call_finished" => {
            let outcome = string(&value, "outcome")?;
            if !matches!(
                outcome,
                "succeeded" | "failed" | "not_executed" | "indeterminate"
            ) {
                return Err("unknown tool outcome".into());
            }
            if value.get("material").is_none() {
                return Err("missing material".into());
            }
            nullable_string(&value, "error_summary")?;
            TurnEventLine::ToolCallFinished {
                id: string(&value, "id")?.into(),
                name: string(&value, "name")?.into(),
                ok: outcome == "succeeded",
            }
        }
        "turn_completed" | "turn_failed" | "turn_cancelled" => {
            turn_ref(&value["turn"])?;
            step(&value["step"])?;
            let outcome = summary(&value["summary"])?;
            if kind != format!("turn_{outcome}")
                || value["step"]["state"]["state"] != "closed"
                || value["step"]["key"]["turn_id"] != value["turn"]["turn_id"]
            {
                return Err("terminal identity/state/summary mismatch".into());
            }
            let end = &value["step"]["state"]["end"];
            if (outcome == "completed" && end["end"] != "finish")
                || (outcome != "completed" && end["end"] != "stop")
            {
                return Err("terminal step end differs".into());
            }
            TurnEventLine::Terminal {
                type_name: kind,
                raw: value,
            }
        }
        _ => {
            match kind.as_str() {
                "step_closed" => step(&value["step"])?,
                "calls_registered" => {
                    step_key(&value["key"])?;
                    array(&value, "calls")?;
                }
                "cep_call_started" | "cep_call_finished" => {
                    for field in ["invocation_id", "method", "semantic_stage", "guard_stage"] {
                        string(&value, field)?;
                    }
                    if kind == "cep_call_finished" {
                        boolean(&value, "ok")?;
                        nullable_string(&value, "error_code")?;
                    }
                }
                "snapshot" => {
                    let view = &value["view"];
                    string(&view["session"], "session_id")?;
                    uint(&view["session"], "current")?;
                    anchor(&view["history"])?;
                    let turn = view.get("selected_turn").ok_or("missing selected_turn")?;
                    if !turn.is_null() {
                        turn_ref(turn)?;
                        let steps = array(turn, "steps")?;
                        for s in steps {
                            step(s)?;
                        }
                        let s = turn.get("summary").ok_or("missing summary")?;
                        if !s.is_null() {
                            let outcome = summary(s)?;
                            let last = steps.last().ok_or("closed turn has no steps")?;
                            if steps.iter().any(|s| s["state"]["state"] != "closed")
                                || steps[..steps.len() - 1]
                                    .iter()
                                    .any(|s| s["state"]["end"]["end"] != "continue")
                                || last["state"]["end"]["end"]
                                    != if outcome == "completed" {
                                        "finish"
                                    } else {
                                        "stop"
                                    }
                            {
                                return Err("closed turn has invalid steps".into());
                            }
                        }
                    }
                }
                "history" => {
                    let history = &value["history"];
                    anchor(&history["anchor"])?;
                    for entry in array(history, "entries")? {
                        uint(entry, "node")?;
                        string(entry, "agent_name")?;
                        if !entry["content"].is_object() {
                            return Err("invalid node content".into());
                        }
                    }
                }
                "acceptance" => {
                    let result = &value["result"];
                    match string(result, "acceptance")? {
                        "accepted" | "existing" => turn_ref(&result["turn"])?,
                        "confirmation_unknown" => {
                            string(result, "turn_id")?;
                            nullable_string(result, "session_id")?;
                        }
                        "needs_configuration" => {
                            string(result, "agent_name")?;
                        }
                        _ => return Err("unknown acceptance".into()),
                    }
                }
                "error" => {
                    string(&value, "kind")?;
                    string(&value, "message")?;
                }
                _ => {}
            }
            TurnEventLine::Unknown {
                type_name: kind,
                raw: value,
            }
        }
    };
    Ok(Some(event))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedJsonLine {
    pub line_number: usize,
    pub raw: Vec<u8>,
    pub reason: String,
}
#[derive(Debug, Clone, Default)]
pub struct SendOutcome {
    pub events: Vec<TurnEventLine>,
    pub malformed_lines: Vec<MalformedJsonLine>,
    pub exit_code: Option<SendExitCode>,
}
#[derive(Debug)]
pub struct SendExecution {
    pub command: CommandOutcome,
    pub parsed: SendOutcome,
}
impl std::ops::Deref for SendExecution {
    type Target = SendOutcome;
    fn deref(&self) -> &Self::Target {
        &self.parsed
    }
}
impl SendOutcome {
    /// SessionId、TurnId、三态 outcome；第二项不是 Git OID。
    pub fn closed_result(&self) -> Option<(&str, &str, &str)> {
        closed_result(&self.events)
    }
    pub fn accepted_turn_id(&self) -> Option<&str> {
        self.accepted_turn().and_then(|t| t["turn_id"].as_str())
    }
    pub fn accepted_session_id(&self) -> Option<&str> {
        self.accepted_turn().and_then(|t| t["session_id"].as_str())
    }
    fn accepted_turn(&self) -> Option<&Value> {
        self.events.iter().find_map(|e| match e {
            TurnEventLine::Unknown { type_name, raw }
                if type_name == "acceptance"
                    && matches!(
                        raw["result"]["acceptance"].as_str(),
                        Some("accepted" | "existing")
                    ) =>
            {
                Some(&raw["result"]["turn"])
            }
            _ => None,
        })
    }
    pub fn has_completed(&self) -> bool {
        self.closed_result()
            .is_some_and(|(_, _, o)| o == "completed")
    }
    pub fn assistant_text(&self) -> String {
        self.events
            .iter()
            .filter_map(|e| match e {
                TurnEventLine::TokenDelta { content } => Some(content.as_str()),
                _ => None,
            })
            .collect()
    }
    pub fn type_sequence(&self) -> Vec<&str> {
        self.events.iter().map(TurnEventLine::type_name).collect()
    }
}
fn closed_result(events: &[TurnEventLine]) -> Option<(&str, &str, &str)> {
    events.iter().rev().find_map(|e| match e {
        TurnEventLine::Terminal { raw, .. } => Some((
            raw["turn"]["session_id"].as_str()?,
            raw["turn"]["turn_id"].as_str()?,
            summary(&raw["summary"]).ok()?,
        )),
        TurnEventLine::Unknown { type_name, raw } if type_name == "snapshot" => {
            let t = &raw["view"]["selected_turn"];
            Some((
                t["session_id"].as_str()?,
                t["turn_id"].as_str()?,
                summary(&t["summary"]).ok()?,
            ))
        }
        _ => None,
    })
}
/// 没有完整历史或有读取/执行错误时不能由 Completed 推导命令成功。
/// 进程退出、干预、EOF 和畸形行仍必须通过 SendExecution 另外核验。
pub fn derive_send_exit_code(events: &[TurnEventLine]) -> SendExitCode {
    let mut failure = None;
    let mut expected = None;
    let mut delivered = None;
    for event in events {
        if let TurnEventLine::Unknown { type_name, raw } = event {
            match type_name.as_str() {
                "snapshot" => expected = Some(&raw["view"]["history"]),
                "history" => delivered = Some(&raw["history"]["anchor"]),
                "acceptance"
                    if matches!(
                        raw["result"]["acceptance"].as_str(),
                        Some("accepted" | "existing")
                    ) && matches!(failure, Some("store_unavailable" | "not_found")) =>
                {
                    failure = None
                }
                "error"
                    if !matches!(
                        raw["kind"].as_str(),
                        Some("transport" | "protocol" | "not_running")
                    ) =>
                {
                    failure = raw["kind"].as_str();
                }
                _ => {}
            }
        }
    }
    if failure.is_some() {
        return SendExitCode::Failed;
    }
    let delivered = delivered.is_some_and(|anchor| expected.is_none_or(|cut| cut == anchor));
    match closed_result(events).map(|(_, _, o)| o) {
        Some("completed") if delivered => SendExitCode::Committed,
        Some("cancelled") if delivered => SendExitCode::InputError,
        _ => SendExitCode::Failed,
    }
}
pub fn parse_send_stdout(stdout: &[u8], exit_code: Option<i32>) -> SendOutcome {
    let mut result = SendOutcome {
        exit_code: SendExitCode::from_code(exit_code),
        ..Default::default()
    };
    for (index, raw) in stdout.split(|b| *b == b'\n').enumerate() {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let parsed = std::str::from_utf8(raw)
            .map_err(|e| e.to_string())
            .and_then(parse_event_line_checked);
        match parsed {
            Ok(Some(event)) => result.events.push(event),
            Ok(None) => {}
            Err(reason) => result.malformed_lines.push(MalformedJsonLine {
                line_number: index + 1,
                raw: raw.to_vec(),
                reason,
            }),
        }
    }
    result
}

/// 首次真实 prompt 被接受之前没有 SessionId；构造不调用产品、不添加 bootstrap Turn。
pub struct SessionSendDriver<'a> {
    driver: &'a LatticeDriver,
    world: &'a ScenarioWorld,
    spec: &'a ScenarioSpec,
    budget: ScenarioBudget,
    agent: String,
    session_id: std::sync::OnceLock<String>,
}
impl<'a> SessionSendDriver<'a> {
    pub fn new(
        driver: &'a LatticeDriver,
        world: &'a ScenarioWorld,
        spec: &'a ScenarioSpec,
        agent: &str,
    ) -> Self {
        Self {
            driver,
            world,
            spec,
            budget: world.budget(spec),
            agent: agent.into(),
            session_id: std::sync::OnceLock::new(),
        }
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.get().map(String::as_str)
    }
    pub fn budget(&self) -> &ScenarioBudget {
        &self.budget
    }
    fn command(&self, prompt: &str) -> LatticeCommand {
        let mut args = vec![
            "--json".into(),
            "session".into(),
            "send".into(),
            prompt.into(),
            "--agent".into(),
            self.agent.clone(),
        ];
        if let Some(id) = self.session_id() {
            args.extend(["--session".into(), id.into()]);
        }
        LatticeCommand {
            args,
            timeout: Some(self.budget.cap_timeout(self.spec.step_timeout())),
        }
    }
    fn record(&self, command: CommandOutcome) -> SendExecution {
        let parsed = parse_send_stdout(&command.stdout, command.exit_code());
        if let Some(id) = parsed.accepted_session_id() {
            if let Some(previous) = self.session_id() {
                assert_eq!(previous, id, "accepted Session changed");
            } else {
                self.session_id
                    .set(id.to_owned())
                    .expect("single scenario driver writer");
            }
        }
        SendExecution { command, parsed }
    }
    pub fn send(&self, prompt: &str) -> Result<SendExecution, DriverError> {
        Ok(self.record(self.driver.run(self.world, &self.command(prompt))?))
    }
    pub fn spawn(&self, prompt: &str) -> Result<RunningCommand, DriverError> {
        self.driver.start(self.world, &self.command(prompt))
    }
    pub fn wait_spawned(&self, running: RunningCommand) -> Result<SendExecution, DriverError> {
        Ok(self.record(running.wait(None)?))
    }
    pub fn run_command(&self, args: &[&str]) -> Result<CommandOutcome, DriverError> {
        self.driver.run(
            self.world,
            &LatticeCommand {
                args: args.iter().map(|s| (*s).into()).collect(),
                timeout: Some(self.budget.cap_timeout(self.spec.step_timeout())),
            },
        )
    }
}
pub fn assert_event_subsequence(
    outcome: &SendOutcome,
    required: &[&str],
) -> Result<Vec<usize>, String> {
    let types = outcome.type_sequence();
    let mut found = vec![];
    let mut cursor = 0;
    for need in required {
        let relative = types[cursor..]
            .iter()
            .position(|t| t == need)
            .ok_or_else(|| format!("missing '{need}' after {cursor}; observed {types:?}"))?;
        cursor += relative;
        found.push(cursor);
        cursor += 1;
    }
    Ok(found)
}
#[cfg(test)]
#[path = "session_send_tests.rs"]
mod tests;
