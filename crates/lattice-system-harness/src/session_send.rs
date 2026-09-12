// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `SessionSendDriver`: drive a deterministic full turn through the public
//! `lattice --json session send <id> <prompt>` entrypoint.
//!
//! This module is deliberately split into two halves:
//!
//! - **Pure parsing / exit-code derivation** (`TurnEventLine`,
//!   `derive_send_exit_code`, `EventSequence`): no I/O, no binary, fast unit
//!   tests. These mirror the public contract documented in
//!   `crates/lattice-cli/src/commands/run.rs` and
//!   `crates/lattice-api/src/turn/types.rs` but never import the product
//!   `TurnEvent` enum — the harness boundary forbids it.
//! - **Driver** (`SessionSendDriver`): creates a session via the public
//!   `session new --json` command, runs `--json session send`, captures
//!   stdout JSONL + exit code, and exposes the parsed event sequence. The
//!   Direct-Local turn itself runs through the real product path.
//!
//! The parser is **tolerant of unknown variants**: the public `TurnEvent`
//! contract is explicitly incremental (new variants may appear), so each
//! JSONL line is decoded into a generic JSON value plus a typed `type`
//! discriminator, and only the fields this driver asserts on are surfaced.
//! An unknown `type` becomes [`TurnEventLine::Unknown`] carrying the raw
//! value, never a parse error.

use serde_json::Value;
use thiserror::Error;

use crate::driver::{CommandOutcome, DriverError, LatticeCommand, LatticeDriver, RunningCommand};
use crate::scenario::{ScenarioBudget, ScenarioSpec};
use crate::world::ScenarioWorld;

/// Exit code contract for `lattice --json session send` (public, documented
/// in `run.rs` and the issue #41 (Codeberg #19) RFC decision three).
///
/// - `0` — turn committed (`TurnOutcome::Completed`).
/// - `1` — provider/tool/commit/runtime failure, or stream ended without a
///   terminal/commit signal.
/// - `2` — input/config error: session not found, profile load failure, or
///   any pre-stream `ApiError` mapped to exit 2 in `run_inner`.
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

/// One decoded JSONL line from `lattice --json session send` stdout.
///
/// The shape mirrors the public `TurnEvent` serde contract
/// (`#[serde(tag = "type", rename_all = "snake_case")]`), but only the
/// variants this driver asserts on are typed; everything else lands in
/// [`TurnEventLine::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEventLine {
    TurnStarted {
        session_id: String,
        turn_id: String,
        branch: Option<String>,
    },
    ModelStarted {
        provider: Option<String>,
        model: String,
    },
    LlmRequestError {
        error_kind: String,
        will_retry: bool,
        message: String,
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
        finish_reason: String,
    },
    TurnFailed {
        reason: String,
    },
    TurnCancelled {
        reason: Option<String>,
    },
    TurnCommitting,
    TurnCommitted {
        session_id: String,
        commit_id: String,
        outcome: String,
    },
    CommitFailed {
        reason: String,
    },
    /// A line that decoded as JSON and carried a `type` field, but the
    /// discriminator is not one this driver knows. The raw value is kept
    /// so a scenario can inspect it without re-parsing stdout.
    Unknown {
        type_name: String,
        raw: Value,
    },
}

impl TurnEventLine {
    /// The `type` discriminator as it appears on the wire (snake_case).
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::TurnStarted { .. } => "turn_started",
            Self::ModelStarted { .. } => "model_started",
            Self::LlmRequestError { .. } => "llm_request_error",
            Self::ReasoningDelta { .. } => "reasoning_delta",
            Self::TokenDelta { .. } => "token_delta",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::ToolCallFinished { .. } => "tool_call_finished",
            Self::ModelFinished { .. } => "model_finished",
            Self::TurnFailed { .. } => "turn_failed",
            Self::TurnCancelled { .. } => "turn_cancelled",
            Self::TurnCommitting => "turn_committing",
            Self::TurnCommitted { .. } => "turn_committed",
            Self::CommitFailed { .. } => "commit_failed",
            Self::Unknown { .. } => "unknown",
        }
    }
}

/// Decode one JSONL line into a [`TurnEventLine`]. Unknown discriminators are
/// tolerated, while known variants must satisfy their documented field types.
pub fn parse_event_line(line: &str) -> Option<TurnEventLine> {
    parse_event_line_checked(line).ok().flatten()
}

fn parse_event_line_checked(line: &str) -> Result<Option<TurnEventLine>, String> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let type_name = required_string(&value, "type")?;
    let event = match type_name.as_str() {
        "turn_started" => {
            required_nullable_string(&value, "parent_commit_id")?;
            TurnEventLine::TurnStarted {
                session_id: required_string(&value, "session_id")?,
                turn_id: required_string(&value, "turn_id")?,
                branch: required_nullable_string(&value, "branch")?,
            }
        }
        "model_started" => TurnEventLine::ModelStarted {
            provider: required_nullable_string(&value, "provider")?,
            model: required_string(&value, "model")?,
        },
        "llm_request_error" => {
            required_nullable_string(&value, "provider")?;
            required_string(&value, "model")?;
            required_u64(&value, "attempt")?;
            required_u64(&value, "max_attempts")?;
            required_bool(&value, "retryable")?;
            required_nullable_u64(&value, "delay_ms")?;
            required_nullable_u64(&value, "status_code")?;
            TurnEventLine::LlmRequestError {
                error_kind: required_string(&value, "error_kind")?,
                will_retry: required_bool(&value, "will_retry")?,
                message: required_string(&value, "message")?,
            }
        }
        "reasoning_delta" => TurnEventLine::ReasoningDelta {
            content: required_string(&value, "content")?,
        },
        "token_delta" => TurnEventLine::TokenDelta {
            content: required_string(&value, "content")?,
        },
        "tool_call_started" => TurnEventLine::ToolCallStarted {
            id: required_string(&value, "id")?,
            name: required_string(&value, "name")?,
        },
        "tool_call_finished" => {
            required_u64(&value, "output_len")?;
            required_nullable_string(&value, "error_summary")?;
            TurnEventLine::ToolCallFinished {
                id: required_string(&value, "id")?,
                name: required_string(&value, "name")?,
                ok: required_bool(&value, "ok")?,
            }
        }
        "model_finished" => {
            required_nullable_usage(&value, "usage")?;
            TurnEventLine::ModelFinished {
                finish_reason: required_string(&value, "finish_reason")?,
            }
        }
        "turn_failed" => TurnEventLine::TurnFailed {
            reason: required_string(&value, "reason")?,
        },
        "turn_cancelled" => TurnEventLine::TurnCancelled {
            reason: required_nullable_string(&value, "reason")?,
        },
        "turn_committing" => TurnEventLine::TurnCommitting,
        "turn_committed" => {
            let outcome = tagged_outcome(
                value
                    .get("outcome")
                    .ok_or_else(|| "field 'outcome' is required".to_string())?,
            )
            .map_err(|reason| format!("field 'outcome': {reason}"))?
            .to_string();
            required_string(&value, "origin")?;
            if value.get("workspace_history_checkpoint").is_none() {
                return Err("missing field 'workspace_history_checkpoint'".to_string());
            }
            TurnEventLine::TurnCommitted {
                session_id: required_string(&value, "session_id")?,
                commit_id: required_string(&value, "commit_id")?,
                outcome,
            }
        }
        "commit_failed" => TurnEventLine::CommitFailed {
            reason: required_string(&value, "reason")?,
        },
        "cep_call_started" => {
            validate_cep_fields(&value, false)?;
            TurnEventLine::Unknown {
                type_name,
                raw: value,
            }
        }
        "cep_call_finished" => {
            validate_cep_fields(&value, true)?;
            TurnEventLine::Unknown {
                type_name,
                raw: value,
            }
        }
        "snapshot" => {
            // 固定快照中已提交 Turn 的 outcome 与直播终态共用同一严格判定；畸形不得充当结果。
            let turn = &value["view"]["requested_turn"];
            if turn["state"] == "committed" {
                tagged_outcome(&turn["outcome"])
                    .map_err(|reason| format!("committed snapshot outcome: {reason}"))?;
            }
            TurnEventLine::Unknown {
                type_name,
                raw: value,
            }
        }
        other => TurnEventLine::Unknown {
            type_name: other.to_string(),
            raw: value,
        },
    };
    Ok(Some(event))
}

fn required_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("`{field}` must be a string"))
}

fn required_nullable_string(value: &Value, field: &str) -> Result<Option<String>, String> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("`{field}` must be a string or null")),
        None => Err(format!("`{field}` is required")),
    }
}

fn required_bool(value: &Value, field: &str) -> Result<bool, String> {
    value
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("`{field}` must be a boolean"))
}

fn required_u64(value: &Value, field: &str) -> Result<u64, String> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("`{field}` must be an unsigned integer"))
}

fn required_nullable_u64(value: &Value, field: &str) -> Result<Option<u64>, String> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(number) => number
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{field}` must be an unsigned integer or null")),
        None => Err(format!("`{field}` is required")),
    }
}

fn required_nullable_usage(value: &Value, field: &str) -> Result<(), String> {
    match value.get(field) {
        Some(Value::Null) => Ok(()),
        Some(usage @ Value::Object(_)) => {
            required_u64(usage, "prompt_tokens")?;
            required_u64(usage, "completion_tokens")?;
            required_u64(usage, "total_tokens")?;
            Ok(())
        }
        Some(_) => Err(format!("`{field}` must be an object or null")),
        None => Err(format!("`{field}` is required")),
    }
}

fn validate_cep_fields(value: &Value, finished: bool) -> Result<(), String> {
    required_string(value, "invocation_id")?;
    required_string(value, "method")?;
    required_string(value, "semantic_stage")?;
    required_string(value, "guard_stage")?;
    if finished {
        required_bool(value, "ok")?;
        required_nullable_string(value, "error_code")?;
    }
    Ok(())
}

/// Derived outcome of a `session send` run, parsed purely from the public
/// JSONL + exit code (no product types).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedJsonLine {
    pub line_number: usize,
    pub raw: Vec<u8>,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct SendOutcome {
    /// Every decoded event line, in arrival order. Unknown variants are
    /// kept as [`TurnEventLine::Unknown`].
    pub events: Vec<TurnEventLine>,
    /// Every nonblank stdout line that was not a valid JSON event.
    pub malformed_lines: Vec<MalformedJsonLine>,
    /// The exit code the binary returned, classified into the public
    /// 0/1/2 contract. `None` if the process was killed by a signal (no
    /// exit code) — the scenario should treat that as a failure.
    pub exit_code: Option<SendExitCode>,
}

/// One lossless `session send` execution: raw process bytes and termination
/// evidence alongside the tolerant JSONL projection.
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
    /// 最终固定快照可补齐丢失的终态事件，但不被伪装成补播事件。
    pub fn committed_result(&self) -> Option<(&str, &str, &str)> {
        committed_result(&self.events)
    }

    pub fn accepted_turn_id(&self) -> Option<&str> {
        self.events.iter().find_map(|event| match event {
            TurnEventLine::Unknown { type_name, raw }
                if type_name == "acceptance" && raw["result"]["acceptance"] == "accepted" =>
            {
                raw["result"]["turn_id"].as_str()
            }
            _ => None,
        })
    }

    /// 终态事件可能丢失；固定快照与命令结果仍须分别核验。
    pub fn has_committed_completed(&self) -> bool {
        self.committed_result()
            .is_some_and(|(_, _, outcome)| outcome == "completed")
    }

    /// Concatenated `token_delta` content, in arrival order. Lets a scenario
    /// assert the visible assistant text without re-implementing the
    /// renderer.
    pub fn assistant_text(&self) -> String {
        let mut out = String::new();
        for e in &self.events {
            if let TurnEventLine::TokenDelta { content } = e {
                out.push_str(content);
            }
        }
        out
    }

    /// The subsequence of event type names, in arrival order. Useful for
    /// asserting a required event subsequence (RFC #41 decision five: no
    /// full golden, only required sub-sequences).
    pub fn type_sequence(&self) -> Vec<&'static str> {
        self.events.iter().map(|e| e.type_name()).collect()
    }
}

fn committed_result(events: &[TurnEventLine]) -> Option<(&str, &str, &str)> {
    events.iter().rev().find_map(|event| match event {
        TurnEventLine::TurnCommitted {
            session_id,
            commit_id,
            outcome,
        } => Some((session_id.as_str(), commit_id.as_str(), outcome.as_str())),
        TurnEventLine::Unknown { type_name, raw } if type_name == "snapshot" => {
            let view = &raw["view"];
            let turn = &view["requested_turn"];
            if turn["state"] != "committed" {
                return None;
            }
            Some((
                view["session"]["session_id"].as_str()?,
                turn["commit_id"].as_str()?,
                tagged_outcome(&turn["outcome"]).ok()?,
            ))
        }
        _ => None,
    })
}

/// 按公开 `TurnOutcome`、`ToolCallValidation`、`AttemptInterruption` 与 `InvocationId`
/// 的序列化形式核验标签、原因值域和嵌套字段；不导入产品类型，也不接受自造的兼容形式。
/// 直播 `turn_committed` 与固定快照共用此入口，畸形终态不得进入成功推导。
fn tagged_outcome(value: &Value) -> Result<&str, String> {
    let (tag, payload) =
        single_tag(value).ok_or_else(|| "outcome must carry exactly one tag".to_string())?;
    if !payload.is_object() {
        return Err(format!("outcome `{tag}` payload must be an object"));
    }
    let valid = match tag {
        "completed" => payload["finish_reason"].as_str() == Some("stop"),
        "failed" => valid_failure_reason(&payload["reason"]),
        "cancelled" => payload["reason"].as_str() == Some("user_cancelled"),
        "interrupted" => valid_interruption_reason(&payload["reason"]),
        other => return Err(format!("unknown outcome tag `{other}`")),
    };
    if !valid {
        return Err(format!(
            "outcome `{tag}` has an invalid finish_reason or reason"
        ));
    }
    Ok(tag)
}

fn single_tag(value: &Value) -> Option<(&str, &Value)> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object
        .iter()
        .next()
        .map(|(tag, payload)| (tag.as_str(), payload))
}

fn valid_failure_reason(value: &Value) -> bool {
    if let Some(reason) = value.as_str() {
        return matches!(reason, "model_request_failed" | "model_step_limit_exceeded");
    }
    let Some(("invalid_tool_call", payload)) = single_tag(value) else {
        return false;
    };
    let Some((tag, fields)) = single_tag(&payload["validation"]) else {
        return false;
    };
    let required: &[&str] = match tag {
        "too_many_calls" => &["limit", "actual"],
        "malformed_arguments"
        | "arguments_not_object"
        | "missing_tool_name"
        | "empty_tool_call_id" => &["call_ordinal"],
        "arguments_too_large" => &["call_ordinal", "limit", "actual"],
        "duplicate_tool_call_id" => &["first_ordinal", "second_ordinal"],
        _ => return false,
    };
    valid_usize_fields(fields, required)
}

fn valid_interruption_reason(value: &Value) -> bool {
    if let Some(reason) = value.as_str() {
        return matches!(
            reason,
            "process_interrupted_before_model_attempt"
                | "process_interrupted_during_model_attempt"
                | "process_interrupted_during_model_retry"
                | "process_interrupted_during_tool_binding"
                | "while_awaiting_model_continuation"
                | "process_interrupted_during_step_closing"
        );
    }
    match single_tag(value) {
        Some(("model_attempt", reason)) => valid_attempt_interruption(reason),
        Some(("invocation_may_have_occurred", payload)) => {
            payload["invocation"].as_str().is_some_and(|id| {
                id.len() == 36
                    && id.bytes().enumerate().all(|(index, byte)| match index {
                        8 | 13 | 18 | 23 => byte == b'-',
                        14 => byte == b'4',
                        19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
                        _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
                    })
            })
        }
        _ => false,
    }
}

fn valid_attempt_interruption(value: &Value) -> bool {
    if let Some(reason) = value.as_str() {
        return matches!(
            reason,
            "stream_error" | "timeout" | "cancelled" | "process_interrupted"
        );
    }
    let Some(("output_limit_exceeded", payload)) = single_tag(value) else {
        return false;
    };
    matches!(
        payload["kind"].as_str(),
        Some(
            "tool_call_arguments"
                | "assistant_content"
                | "reasoning_content"
                | "model_attempt_total"
        )
    ) && valid_usize_fields(payload, &["limit", "actual"])
}

fn valid_usize_fields(value: &Value, fields: &[&str]) -> bool {
    fields.iter().all(|field| {
        value[*field]
            .as_u64()
            .is_some_and(|number| usize::try_from(number).is_ok())
    })
}

/// Classify a `session send` exit code into the public contract. A missing
/// exit code (signal kill) maps to [`SendExitCode::Failed`]: the scenario
/// treats signal death as a failure, never as success.
pub fn classify_exit(code: Option<i32>) -> SendExitCode {
    SendExitCode::from_code(code).unwrap_or(SendExitCode::Failed)
}

/// 从公开终态或固定快照交叉检查退出码；显式发布错误优先，不能用可见 pin 掩盖失败。
pub fn derive_send_exit_code(events: &[TurnEventLine]) -> SendExitCode {
    if events.iter().any(|e| {
        matches!(e, TurnEventLine::CommitFailed { .. })
            || matches!(e, TurnEventLine::Unknown { type_name, .. } if type_name == "error")
    }) {
        return SendExitCode::Failed;
    }
    if events.iter().any(|e| {
        matches!(
            e,
            TurnEventLine::TurnFailed { .. } | TurnEventLine::TurnCancelled { .. }
        )
    }) {
        return SendExitCode::Failed;
    }
    if committed_result(events).is_some_and(|(_, _, outcome)| outcome == "completed") {
        SendExitCode::Committed
    } else {
        SendExitCode::Failed
    }
}

/// Parse a full `session send` stdout buffer into a [`SendOutcome`]. Blank
/// lines are ignored; every other invalid line is retained with its raw bytes.
pub fn parse_send_stdout(stdout: &[u8], exit_code: Option<i32>) -> SendOutcome {
    let mut events = Vec::new();
    let mut malformed_lines = Vec::new();
    for (index, raw) in stdout.split(|byte| *byte == b'\n').enumerate() {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        if raw.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match std::str::from_utf8(raw) {
            Ok(line) => match parse_event_line_checked(line) {
                Ok(Some(event)) => events.push(event),
                Ok(None) => {}
                Err(reason) => malformed_lines.push(MalformedJsonLine {
                    line_number: index + 1,
                    raw: raw.to_vec(),
                    reason,
                }),
            },
            Err(error) => malformed_lines.push(MalformedJsonLine {
                line_number: index + 1,
                raw: raw.to_vec(),
                reason: error.to_string(),
            }),
        }
    }
    SendOutcome {
        events,
        malformed_lines,
        exit_code: SendExitCode::from_code(exit_code),
    }
}

/// Errors surfaced by `SessionSendDriver`. A driver failure is distinct from
/// a product-side turn failure: a `Failed`/`InputError` exit code is a
/// successful *drive* of an unsuccessful *turn*, and is returned in
/// [`SendOutcome`], not as a `SessionSendError`.
#[derive(Debug, Error)]
pub enum SessionSendError {
    #[error(transparent)]
    Driver(#[from] DriverError),
    #[error("`session new --json` did not emit a session_id; stdout: {stdout}")]
    NoSessionId { stdout: String },
    #[error("`session new --json` failed: {outcome:?}")]
    NewSessionFailed { outcome: Box<CommandOutcome> },
}

fn create_session_with_timeout(
    driver: &LatticeDriver,
    world: &ScenarioWorld,
    timeout: std::time::Duration,
    agent: &str,
    project_branch: &str,
) -> Result<String, SessionSendError> {
    let output = driver.run(
        world,
        &LatticeCommand {
            args: vec![
                "--json".into(),
                "session".into(),
                "new".into(),
                "--agent".into(),
                agent.into(),
                "--project-branch".into(),
                project_branch.into(),
            ],
            timeout: Some(timeout),
        },
    )?;
    if !output.is_clean_exit(0) {
        return Err(SessionSendError::NewSessionFailed {
            outcome: Box::new(output),
        });
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let value: Value =
        serde_json::from_str(stdout.trim()).map_err(|_| SessionSendError::NoSessionId {
            stdout: stdout.to_string(),
        })?;
    let id = value
        .get("session_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| SessionSendError::NoSessionId {
            stdout: stdout.to_string(),
        })?;
    Ok(id.to_string())
}

fn send_prompt_with_timeout(
    driver: &LatticeDriver,
    world: &ScenarioWorld,
    timeout: std::time::Duration,
    session_id: &str,
    prompt: &str,
) -> Result<SendExecution, DriverError> {
    let command = driver.run(
        world,
        &LatticeCommand {
            args: vec![
                "--json".into(),
                "session".into(),
                "send".into(),
                session_id.into(),
                prompt.into(),
            ],
            timeout: Some(timeout),
        },
    )?;
    let parsed = parse_send_stdout(&command.stdout, command.exit_code());
    Ok(SendExecution { command, parsed })
}

/// 保存已解析的会话 ID，供场景多次 `send`，无需重复传入身份。
pub struct SessionSendDriver<'a> {
    driver: &'a LatticeDriver,
    world: &'a ScenarioWorld,
    spec: &'a ScenarioSpec,
    budget: ScenarioBudget,
    session_id: String,
}

impl<'a> SessionSendDriver<'a> {
    /// 创建会话并保存其 ID；会话创建与后续发送都计入场景唯一预算，不在此重新起算。
    pub fn new(
        driver: &'a LatticeDriver,
        world: &'a ScenarioWorld,
        spec: &'a ScenarioSpec,
        agent: &str,
        project_branch: &str,
    ) -> Result<Self, SessionSendError> {
        let budget = world.budget(spec);
        let session_id = create_session_with_timeout(
            driver,
            world,
            budget.cap_timeout(spec.step_timeout()),
            agent,
            project_branch,
        )?;
        Ok(Self {
            driver,
            world,
            spec,
            budget,
            session_id,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// 场景唯一预算的副本（与 [`ScenarioWorld::budget`] 同一起点）；辅助启动、查询及屏障等待沿用它。
    pub fn budget(&self) -> &ScenarioBudget {
        &self.budget
    }

    /// Send one prompt and return raw and parsed execution evidence.
    pub fn send(&self, prompt: &str) -> Result<SendExecution, DriverError> {
        send_prompt_with_timeout(
            self.driver,
            self.world,
            self.budget.cap_timeout(self.spec.step_timeout()),
            &self.session_id,
            prompt,
        )
    }

    /// Spawn `session send` without waiting. Used for concurrent races where
    /// the scenario must mutate Local state or release a fake-LLM gate while
    /// the turn is in flight.
    pub fn spawn(&self, prompt: &str) -> Result<RunningCommand, DriverError> {
        self.driver.start(
            self.world,
            &LatticeCommand {
                args: vec![
                    "--json".into(),
                    "session".into(),
                    "send".into(),
                    self.session_id.clone(),
                    prompt.into(),
                ],
                timeout: Some(self.budget.cap_timeout(self.spec.step_timeout())),
            },
        )
    }

    /// Wait for a previously [`spawn`](Self::spawn)ed send and parse its
    /// JSONL the same way [`send`](Self::send) does.
    pub fn wait_spawned(&self, running: RunningCommand) -> Result<SendExecution, DriverError> {
        let command = running.wait(None)?;
        let parsed = parse_send_stdout(&command.stdout, command.exit_code());
        Ok(SendExecution { command, parsed })
    }

    /// Run another public CLI command under this scenario's shared total budget.
    pub fn run_command(&self, args: &[&str]) -> Result<CommandOutcome, DriverError> {
        self.driver.run(
            self.world,
            &LatticeCommand {
                args: args.iter().map(|arg| (*arg).to_string()).collect(),
                timeout: Some(self.budget.cap_timeout(self.spec.step_timeout())),
            },
        )
    }
}

/// Helper for scenarios: assert that a required event-type subsequence
/// appears in the outcome's event stream, in order (RFC #41 decision five:
/// no full golden, only required sub-sequences). Returns the index of the
/// first event of each required type, or `None` if the subsequence is not
/// satisfied.
pub fn assert_event_subsequence(
    outcome: &SendOutcome,
    required: &[&str],
) -> Result<Vec<usize>, String> {
    let types: Vec<&str> = outcome.events.iter().map(|e| e.type_name()).collect();
    let mut found = Vec::new();
    let mut cursor = 0usize;
    for need in required {
        match types[cursor..].iter().position(|t| t == need) {
            Some(rel) => {
                let abs = cursor + rel;
                found.push(abs);
                cursor = abs + 1;
            }
            None => {
                return Err(format!(
                    "required event subsequence not satisfied: missing '{need}' after position {cursor}; observed types = {types:?}"
                ));
            }
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(json: &str) -> TurnEventLine {
        parse_event_line(json).expect("parsed line")
    }

    #[test]
    fn fixed_commit_after_gap_does_not_hide_publication_error() {
        let snapshot = line(
            r#"{"type":"snapshot","view":{"session":{"session_id":"s1"},"requested_turn":{"state":"committed","commit_id":"c1","outcome":{"completed":{"finish_reason":"stop"}}}}}"#,
        );
        let mut events = vec![line(r#"{"type":"gap"}"#), snapshot];
        assert_eq!(committed_result(&events), Some(("s1", "c1", "completed")));
        assert_eq!(derive_send_exit_code(&events), SendExitCode::Committed);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, TurnEventLine::TurnCommitted { .. }))
        );
        events.push(line(r#"{"type":"error","kind":"store_unavailable"}"#));
        assert_eq!(derive_send_exit_code(&events), SendExitCode::Failed);
    }

    #[test]
    fn parse_turn_started() {
        let e = line(
            r#"{"type":"turn_started","session_id":"s1","turn_id":"s:root","parent_commit_id":null,"branch":"main"}"#,
        );
        assert_eq!(
            e,
            TurnEventLine::TurnStarted {
                session_id: "s1".into(),
                turn_id: "s:root".into(),
                branch: Some("main".into())
            }
        );
    }

    #[test]
    fn parse_token_delta() {
        let e = line(r#"{"type":"token_delta","content":"Hi"}"#);
        assert_eq!(
            e,
            TurnEventLine::TokenDelta {
                content: "Hi".into()
            }
        );
    }

    #[test]
    fn parse_turn_committed_completed() {
        let e = line(
            r#"{"type":"turn_committed","session_id":"s1","commit_id":"c1","outcome":{"completed":{"finish_reason":"stop"}},"origin":"live","workspace_history_checkpoint":"empty"}"#,
        );
        assert_eq!(
            e,
            TurnEventLine::TurnCommitted {
                session_id: "s1".into(),
                commit_id: "c1".into(),
                outcome: "completed".into()
            }
        );
    }

    #[test]
    fn parse_unknown_variant_is_tolerated() {
        let e = line(r#"{"type":"some_future_event","foo":42}"#);
        match e {
            TurnEventLine::Unknown { type_name, raw } => {
                assert_eq!(type_name, "some_future_event");
                assert_eq!(raw.get("foo").and_then(|v| v.as_i64()), Some(42));
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }

    #[test]
    fn known_variant_with_wrong_field_type_is_malformed() {
        let outcome = parse_send_stdout(b"{\"type\":\"token_delta\",\"content\":7}\n", Some(1));
        assert!(outcome.events.is_empty());
        assert_eq!(outcome.malformed_lines.len(), 1);
        assert!(outcome.malformed_lines[0].reason.contains("content"));
    }

    #[test]
    fn known_variants_validate_required_discarded_fields() {
        let lines = [
            r#"{"type":"turn_started","session_id":"s","turn_id":"s:root","branch":null}"#,
            r#"{"type":"tool_call_finished","id":"c","name":"x","ok":true,"error_summary":null}"#,
            r#"{"type":"model_finished","finish_reason":"stop"}"#,
            r#"{"type":"turn_committed","session_id":"s","commit_id":"c","outcome":{"completed":{"finish_reason":"stop"}},"workspace_history_checkpoint":"empty"}"#,
            r#"{"type":"cep_call_started","call_id":1,"method":"x"}"#,
        ];
        for line in lines {
            let outcome = parse_send_stdout(format!("{line}\n").as_bytes(), Some(1));
            assert!(outcome.events.is_empty(), "accepted malformed line: {line}");
            assert_eq!(outcome.malformed_lines.len(), 1, "line: {line}");
        }
    }

    #[test]
    fn exit_code_classification() {
        assert_eq!(
            SendExitCode::from_code(Some(0)),
            Some(SendExitCode::Committed)
        );
        assert_eq!(SendExitCode::from_code(Some(1)), Some(SendExitCode::Failed));
        assert_eq!(
            SendExitCode::from_code(Some(2)),
            Some(SendExitCode::InputError)
        );
        assert_eq!(SendExitCode::from_code(Some(7)), None);
        assert_eq!(SendExitCode::from_code(None), None);
        assert_eq!(classify_exit(None), SendExitCode::Failed);
        assert_eq!(classify_exit(Some(2)), SendExitCode::InputError);
    }

    #[test]
    fn new_session_failure_retains_complete_command_outcome() {
        let outcome = CommandOutcome {
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_complete: true,
            stderr_complete: true,
            termination: Some(crate::driver::ProcessTermination::Signaled(libc::SIGTERM)),
            interventions: vec![crate::driver::HarnessIntervention::InterruptRequested],
            cleanup: crate::driver::ProcessGroupCleanup::default(),
        };

        let error = SessionSendError::NewSessionFailed {
            outcome: Box::new(outcome),
        };
        let SessionSendError::NewSessionFailed { outcome } = error else {
            panic!("wrong error variant");
        };
        assert_eq!(
            outcome.termination,
            Some(crate::driver::ProcessTermination::Signaled(libc::SIGTERM))
        );
        assert_eq!(
            outcome.interventions,
            vec![crate::driver::HarnessIntervention::InterruptRequested]
        );
    }

    #[test]
    fn derive_exit_committed_completed_is_zero() {
        let events = vec![
            line(
                r#"{"type":"turn_started","session_id":"s","turn_id":"s:root","parent_commit_id":null,"branch":null}"#,
            ),
            line(r#"{"type":"model_started","provider":null,"model":"compat:test"}"#),
            line(r#"{"type":"token_delta","content":"hi"}"#),
            line(r#"{"type":"model_finished","finish_reason":"stop","usage":null}"#),
            line(r#"{"type":"turn_committing"}"#),
            line(
                r#"{"type":"turn_committed","session_id":"s","commit_id":"c","outcome":{"completed":{"finish_reason":"stop"}},"origin":"live","workspace_history_checkpoint":"empty"}"#,
            ),
        ];
        assert_eq!(derive_send_exit_code(&events), SendExitCode::Committed);
    }

    #[test]
    fn derive_exit_commit_failed_is_one() {
        let events = vec![
            line(
                r#"{"type":"turn_started","session_id":"s","turn_id":"s:root","parent_commit_id":null,"branch":null}"#,
            ),
            line(r#"{"type":"commit_failed","reason":"boom"}"#),
        ];
        assert_eq!(derive_send_exit_code(&events), SendExitCode::Failed);
    }

    #[test]
    fn derive_exit_turn_failed_is_one() {
        let events = vec![
            line(
                r#"{"type":"turn_started","session_id":"s","turn_id":"s:root","parent_commit_id":null,"branch":null}"#,
            ),
            line(r#"{"type":"turn_failed","reason":"provider boom"}"#),
        ];
        assert_eq!(derive_send_exit_code(&events), SendExitCode::Failed);
    }

    #[test]
    fn derive_exit_committed_error_status_is_one() {
        let events = vec![line(
            r#"{"type":"turn_committed","session_id":"s","commit_id":"c","outcome":{"failed":{"reason":"model_request_failed"}},"origin":"live","workspace_history_checkpoint":"empty"}"#,
        )];
        assert_eq!(derive_send_exit_code(&events), SendExitCode::Failed);
    }

    #[test]
    fn derive_exit_no_terminal_is_one() {
        let events = vec![
            line(
                r#"{"type":"turn_started","session_id":"s","turn_id":"s:root","parent_commit_id":null,"branch":null}"#,
            ),
            line(r#"{"type":"token_delta","content":"hi"}"#),
        ];
        assert_eq!(derive_send_exit_code(&events), SendExitCode::Failed);
    }

    #[test]
    fn parse_send_stdout_concatenates_text_and_classifies() {
        let stdout = b"\
{\"type\":\"turn_started\",\"session_id\":\"s\",\"turn_id\":\"s:root\",\"parent_commit_id\":null,\"branch\":null}\n\
{\"type\":\"model_started\",\"provider\":null,\"model\":\"compat:test\"}\n\
{\"type\":\"token_delta\",\"content\":\"Hello\"}\n\
{\"type\":\"token_delta\",\"content\":\" world\"}\n\
{\"type\":\"model_finished\",\"finish_reason\":\"stop\",\"usage\":null}\n\
{\"type\":\"turn_committed\",\"session_id\":\"s\",\"commit_id\":\"c\",\"outcome\":{\"completed\":{\"finish_reason\":\"stop\"}},\"origin\":\"live\",\"workspace_history_checkpoint\":\"empty\"}\n";
        let outcome = parse_send_stdout(stdout, Some(0));
        assert_eq!(outcome.exit_code, Some(SendExitCode::Committed));
        assert!(outcome.has_committed_completed());
        assert_eq!(outcome.assistant_text(), "Hello world");
        assert_eq!(
            outcome.type_sequence(),
            vec![
                "turn_started",
                "model_started",
                "token_delta",
                "token_delta",
                "model_finished",
                "turn_committed",
            ]
        );
        // Cross-check: derived exit agrees with the binary's exit.
        assert_eq!(
            derive_send_exit_code(&outcome.events),
            SendExitCode::Committed
        );
    }

    #[test]
    fn parse_send_stdout_with_unknown_variant_tolerated() {
        let stdout = b"\
{\"type\":\"turn_started\",\"session_id\":\"s\",\"turn_id\":\"s:root\",\"parent_commit_id\":null,\"branch\":null}\n\
{\"type\":\"some_future_event\",\"x\":1}\n\
{\"type\":\"turn_committed\",\"session_id\":\"s\",\"commit_id\":\"c\",\"outcome\":{\"completed\":{\"finish_reason\":\"stop\"}},\"origin\":\"live\",\"workspace_history_checkpoint\":\"empty\"}\n";
        let outcome = parse_send_stdout(stdout, Some(0));
        assert_eq!(outcome.events.len(), 3);
        assert!(matches!(
            outcome.events[1],
            TurnEventLine::Unknown { ref type_name, .. } if type_name == "some_future_event"
        ));
    }

    #[test]
    fn assert_event_subsequence_satisfied_in_order() {
        let outcome = parse_send_stdout(
            b"\
{\"type\":\"turn_started\",\"session_id\":\"s\",\"turn_id\":\"s:root\",\"parent_commit_id\":null,\"branch\":null}\n\
{\"type\":\"model_started\",\"provider\":null,\"model\":\"m\"}\n\
{\"type\":\"token_delta\",\"content\":\"x\"}\n\
{\"type\":\"model_finished\",\"finish_reason\":\"stop\",\"usage\":null}\n\
{\"type\":\"turn_committed\",\"session_id\":\"s\",\"commit_id\":\"c\",\"outcome\":{\"completed\":{\"finish_reason\":\"stop\"}},\"origin\":\"live\",\"workspace_history_checkpoint\":\"empty\"}\n",
            Some(0),
        );
        let required = [
            "turn_started",
            "model_started",
            "token_delta",
            "model_finished",
            "turn_committed",
        ];
        let idxs = assert_event_subsequence(&outcome, &required).expect("subseq");
        assert_eq!(idxs, vec![0, 1, 2, 3, 4]);
    }

    /// 独立期望夹具：合法标签对象与各类畸形对象；直播 `turn_committed` 与固定快照共用同一判定。
    fn outcome_tag_fixture() -> Value {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/turn_outcome_tags.json"
        )))
        .expect("fixture JSON")
    }

    fn committed_line(outcome: &Value) -> String {
        format!(
            r#"{{"type":"turn_committed","session_id":"s","commit_id":"c","outcome":{outcome},"origin":"live","workspace_history_checkpoint":"empty"}}"#
        )
    }

    fn committed_snapshot_line(outcome: &Value) -> String {
        format!(
            r#"{{"type":"snapshot","view":{{"session":{{"session_id":"s"}},"requested_turn":{{"state":"committed","commit_id":"c","outcome":{outcome}}}}}}}"#
        )
    }

    #[test]
    fn live_turn_committed_accepts_exactly_one_legal_outcome_tag() {
        let fixture = outcome_tag_fixture();
        for case in fixture["accept"].as_array().expect("accept cases") {
            let parsed = parse_send_stdout(
                format!("{}\n", committed_line(&case["outcome"])).as_bytes(),
                Some(0),
            );
            assert!(
                parsed.malformed_lines.is_empty(),
                "rejected legal outcome {}: {:?}",
                case["outcome"],
                parsed.malformed_lines
            );
            assert_eq!(
                parsed.committed_result().map(|(_, _, outcome)| outcome),
                case["tag"].as_str(),
                "outcome {}",
                case["outcome"]
            );
        }
        for case in fixture["reject"].as_array().expect("reject cases") {
            let parsed = parse_send_stdout(
                format!("{}\n", committed_line(&case["outcome"])).as_bytes(),
                Some(0),
            );
            assert!(
                parsed.events.is_empty(),
                "accepted malformed outcome ({}): {} -> {:?}",
                case["why"],
                case["outcome"],
                parsed.events
            );
            assert_eq!(parsed.malformed_lines.len(), 1, "{}", case["why"]);
            assert_eq!(
                derive_send_exit_code(&parsed.events),
                SendExitCode::Failed,
                "{}",
                case["why"]
            );
        }
    }

    #[test]
    fn fixed_snapshot_outcome_uses_the_same_strict_tag_parser() {
        let fixture = outcome_tag_fixture();
        for case in fixture["accept"].as_array().expect("accept cases") {
            let parsed = parse_send_stdout(
                format!("{}\n", committed_snapshot_line(&case["outcome"])).as_bytes(),
                Some(0),
            );
            assert!(
                parsed.malformed_lines.is_empty(),
                "rejected legal snapshot outcome {}: {:?}",
                case["outcome"],
                parsed.malformed_lines
            );
            assert_eq!(
                parsed.committed_result().map(|(_, _, outcome)| outcome),
                case["tag"].as_str(),
                "snapshot outcome {}",
                case["outcome"]
            );
            let expected = if case["tag"] == "completed" {
                SendExitCode::Committed
            } else {
                SendExitCode::Failed
            };
            assert_eq!(
                derive_send_exit_code(&parsed.events),
                expected,
                "snapshot outcome {}",
                case["outcome"]
            );
        }
        for case in fixture["reject"].as_array().expect("reject cases") {
            let parsed = parse_send_stdout(
                format!("{}\n", committed_snapshot_line(&case["outcome"])).as_bytes(),
                Some(0),
            );
            assert!(
                parsed.committed_result().is_none(),
                "malformed snapshot outcome derived a commit ({}): {} -> {:?}",
                case["why"],
                case["outcome"],
                parsed.committed_result()
            );
            assert!(!parsed.has_committed_completed(), "{}", case["why"]);
            assert_eq!(
                derive_send_exit_code(&parsed.events),
                SendExitCode::Failed,
                "{}",
                case["why"]
            );
            assert_eq!(
                parsed.malformed_lines.len(),
                1,
                "malformed committed snapshot must be retained as a malformed line ({}): {}",
                case["why"],
                case["outcome"]
            );
        }
    }

    #[test]
    fn assert_event_subsequence_missing_event_reports() {
        let outcome = parse_send_stdout(
            b"{\"type\":\"turn_started\",\"session_id\":\"s\",\"turn_id\":\"s:root\",\"parent_commit_id\":null,\"branch\":null}\n",
            Some(0),
        );
        let err = assert_event_subsequence(&outcome, &["turn_started", "turn_committed"])
            .expect_err("should fail");
        assert!(err.contains("missing 'turn_committed'"));
    }
}
