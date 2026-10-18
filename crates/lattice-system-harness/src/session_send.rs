// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 通过公开 JSONL 驱动 Turn；业务结局、正文交付和原始进程结果分别保留。
//!
//! `--json` 只有七种记录（会话合同“实现模块与公开操作”）。会话头、对话条目与生成中的文字
//! 和 Local IPC 视图的 `view` 消息同形，解析函数两处共用。读者按 `node` 覆盖条目，
//! 结局取本轮最后一个 Step 条目的 `summary`。
use std::collections::BTreeMap;

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

/// 后台的结构化错误；`conflict` 只在 `kind` 为 `conflict` 时给出原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalError {
    pub kind: String,
    pub message: String,
    pub conflict: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
    Cancelled,
}
impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepEnd {
    Continue,
    Finish,
    Stop { cause: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepProgress {
    Reserved,
    Requesting,
    Executing,
    Closed(StepEnd),
}
impl StepProgress {
    pub fn is_closed(&self) -> bool {
        matches!(self, Self::Closed(_))
    }
    /// 同一节点的阶段只前进；用于核验观察者收到的值不回退。
    fn rank(&self) -> u8 {
        match self {
            Self::Reserved => 0,
            Self::Requesting => 1,
            Self::Executing => 2,
            Self::Closed(_) => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub level: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSummary {
    pub outcome: Outcome,
    pub notice: Option<Notice>,
    pub progress: StepProgress,
}

/// 安排过重试的尝试的提示；等级固定为 warn，解析时已核验。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryNotice {
    pub attempt: u64,
    pub delay_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Text,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub kind: SegmentKind,
    pub text: String,
}

/// 把段接到末尾，与末段同类时并入；生成中的文字与回复正文都按这条规则比较。
pub fn push_segment(segments: &mut Vec<Segment>, segment: Segment) {
    match segments.last_mut() {
        Some(last) if last.kind == segment.kind => last.text.push_str(&segment.text),
        _ => segments.push(segment),
    }
}

pub fn merge_segments<'a>(parts: impl IntoIterator<Item = &'a Segment>) -> Vec<Segment> {
    let mut merged = Vec::new();
    for part in parts {
        push_segment(&mut merged, part.clone());
    }
    merged
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotOutcome {
    Succeeded,
    Failed,
    NotExecuted,
    Indeterminate,
}

/// 槽位的阶段只前进：登记、开始、终态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SlotState {
    Registered,
    Started,
    Terminal,
}

/// Step 条目中一个调用槽位的结论；`error_code` 取 Failed 原因中的 CEP 错误码。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub invocation_id: String,
    pub method: String,
    pub state: SlotState,
    pub outcome: SlotOutcome,
    pub error_code: Option<String>,
}

/// 一次模型尝试的阶段：已发出的尝试可以结束，结束后不再改变。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    Sent,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepEntry {
    pub ordinal: u64,
    pub progress: StepProgress,
    /// 回复的正文与 reasoning parts，按原顺序、未合并；Tool 调用见 `slots`。
    pub reply: Vec<Segment>,
    pub attempts: Vec<AttemptState>,
    pub slots: Vec<Slot>,
    pub retries: Vec<RetryNotice>,
    /// 只有不以 `Continue` 关闭的那一步带这一轮的结局。
    pub summary: Option<TurnSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryContent {
    UserMessage {
        ordinal: u64,
        agent_name: String,
        text: String,
    },
    Step(StepEntry),
}

/// 一个对话条目；`raw` 是条目本身的 JSON（不含 `--json` 与视图记录的 `type` 标签），
/// 视图与历史查询给出的同一条目逐字段相同。原始记录由调用方另存作证据。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub node: u64,
    pub parent: u64,
    pub turn_id: String,
    pub content: EntryContent,
    pub raw: Value,
}
impl Entry {
    pub fn step(&self) -> Option<&StepEntry> {
        match &self.content {
            EntryContent::Step(step) => Some(step),
            EntryContent::UserMessage { .. } => None,
        }
    }
    pub fn user_ordinal(&self) -> Option<u64> {
        match &self.content {
            EntryContent::UserMessage { ordinal, .. } => Some(*ordinal),
            EntryContent::Step(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub session_id: String,
    pub position: u64,
    pub agent: Option<String>,
    pub turn_id: Option<String>,
    pub owner: bool,
    pub diagnostic: Option<LocalError>,
    pub path_replaced: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub node: u64,
    pub attempt: u64,
    pub offset: u64,
    pub segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptResult {
    Accepted {
        session_id: String,
        user_node: u64,
    },
    Rejected(LocalError),
    /// 按 TurnId 查询为 NotFound：确定未被接受。
    NotAccepted,
    Unknown(Option<LocalError>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub turn_id: String,
    pub result: ReceiptResult,
    /// 结果来自按 TurnId 查询，而不是提交连接上的回执。
    pub recovered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submit {
    /// 发出请求前写明身份；`new` 表示提交写明新建会话。
    Sending {
        turn_id: String,
        session_id: String,
        new: bool,
    },
    Receipt(Receipt),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelStatus {
    Taken,
    NotTaken,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelRecord {
    /// `session cancel` 时会话没有正在执行的一轮。
    NothingToCancel { session_id: String },
    Reply {
        session_id: Option<String>,
        turn_id: String,
        status: CancelStatus,
        owned: Option<bool>,
        error: Option<LocalError>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Eof,
    Idle,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SendRecord {
    Submit(Submit),
    Cancel(CancelRecord),
    Head(Head),
    Entry(Box<Entry>),
    Text(TextChunk),
    Connection {
        status: ConnectionStatus,
        error: Option<LocalError>,
    },
    Error(LocalError),
    /// 合法但不属于七种的记录；场景据此断言没有多出的记录类型。
    Unknown {
        type_name: String,
        raw: Value,
    },
}
impl SendRecord {
    pub fn type_name(&self) -> &str {
        match self {
            Self::Submit(_) => "submit",
            Self::Cancel(_) => "cancel",
            Self::Head(_) => "head",
            Self::Entry(_) => "entry",
            Self::Text(_) => "text",
            Self::Connection { .. } => "connection",
            Self::Error(_) => "error",
            Self::Unknown { type_name, .. } => type_name,
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
/// 字段必须出现，值为字符串或 null。
fn nullable_string(value: &Value, field: &str) -> Result<Option<String>, String> {
    match value.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(format!("`{field}` must be a string or null")),
    }
}
/// 字段可以缺省；出现时为字符串或 null。
fn optional_string(value: &Value, field: &str) -> Result<Option<String>, String> {
    match value.get(field) {
        None => Ok(None),
        Some(_) => nullable_string(value, field),
    }
}
fn optional_error(value: &Value, field: &str) -> Result<Option<LocalError>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(error) => local_error(error).map(Some),
    }
}
fn local_error(value: &Value) -> Result<LocalError, String> {
    let kind = string(value, "kind")?.to_owned();
    let message = string(value, "message")?.to_owned();
    let conflict = optional_string(value, "conflict")?;
    if let Some(reason) = &conflict
        && !matches!(
            reason.as_str(),
            "turn_id_used" | "session_exists" | "binding_mismatch"
        )
    {
        return Err(format!("unknown conflict reason `{reason}`"));
    }
    if (kind == "conflict") != conflict.is_some() {
        return Err("conflict reason must accompany exactly the conflict kind".into());
    }
    Ok(LocalError {
        kind,
        message,
        conflict,
    })
}
fn step_end(value: &Value) -> Result<StepEnd, String> {
    Ok(match string(value, "end")? {
        "continue" => StepEnd::Continue,
        "finish" => StepEnd::Finish,
        "stop" => StepEnd::Stop {
            cause: string(&value["cause"], "cause")?.into(),
        },
        _ => return Err("unknown step end".into()),
    })
}
fn step_progress(value: &Value) -> Result<StepProgress, String> {
    Ok(match string(value, "progress")? {
        "reserved" => StepProgress::Reserved,
        "requesting" => StepProgress::Requesting,
        "executing" => StepProgress::Executing,
        "closed" => StepProgress::Closed(step_end(&value["end"])?),
        _ => return Err("unknown step progress".into()),
    })
}
fn summary(value: &Value) -> Result<TurnSummary, String> {
    let progress = step_progress(&value["progress"])?;
    let outcome = match string(value, "outcome")? {
        "completed" => Outcome::Completed,
        "failed" => Outcome::Failed,
        "cancelled" => Outcome::Cancelled,
        _ => return Err("invalid outcome".into()),
    };
    let notice = match (outcome, value.get("notice")) {
        (Outcome::Completed, Some(Value::Null)) => None,
        (Outcome::Failed | Outcome::Cancelled, Some(notice)) => {
            let expected = if outcome == Outcome::Failed {
                "error"
            } else {
                "info"
            };
            let level = string(notice, "level")?;
            if level != expected {
                return Err("summary notice level differs from outcome".into());
            }
            let text = string(notice, "text")?;
            if text.is_empty() || text.chars().count() > 2048 {
                return Err("invalid bounded notice".into());
            }
            Some(Notice {
                level: level.into(),
                text: text.into(),
            })
        }
        _ => return Err("invalid outcome or notice".into()),
    };
    if let StepProgress::Closed(end) = &progress {
        let expected = match end {
            StepEnd::Finish => Outcome::Completed,
            StepEnd::Stop { cause } if cause == "cancelled" => Outcome::Cancelled,
            StepEnd::Stop { .. } => Outcome::Failed,
            StepEnd::Continue => return Err("a Turn summary cannot end with continue".into()),
        };
        if expected != outcome {
            return Err("summary differs from the last step conclusion".into());
        }
    }
    Ok(TurnSummary {
        outcome,
        notice,
        progress,
    })
}
fn usage(value: &Value) -> Result<(), String> {
    if !value.is_null() {
        for key in ["prompt_tokens", "completion_tokens", "total_tokens"] {
            uint(value, key)?;
        }
    }
    Ok(())
}
/// 现有 `StepDetail`：各尝试、槽位及由阶段给出的结论。
fn step_detail(value: &Value) -> Result<(Vec<AttemptState>, Vec<Slot>), String> {
    string(&value["key"], "turn_id")?;
    uint(&value["key"], "ordinal")?;
    uint(value, "node")?;
    string(value, "updated_at")?;
    let progress = step_progress(&value["progress"])?;
    let end = step_end(&value["end"])?;
    if let StepProgress::Closed(closed) = &progress
        && *closed != end
    {
        return Err("closed step end differs from progress".into());
    }
    let mut attempts = Vec::new();
    for attempt in array(value, "attempts")? {
        uint(attempt, "attempt")?;
        string(attempt, "origin")?;
        usage(attempt.get("usage").ok_or("missing usage")?)?;
        attempts.push(match string(&attempt["progress"], "progress")? {
            "sent" => AttemptState::Sent,
            "ended" => {
                if attempt["progress"]["end"]["end"] != attempt["end"]["end"]
                    || attempt["progress"]["end"]["cause"] != attempt["end"]["cause"]
                {
                    return Err("attempt progress/end mismatch".into());
                }
                AttemptState::Ended
            }
            _ => return Err("unknown attempt progress".into()),
        });
        match string(&attempt["end"], "end")? {
            "completed" => {
                string(&attempt["end"]["step"], "turn_id")?;
                uint(&attempt["end"]["step"], "model_step_ordinal")?;
            }
            "stopped" => {
                string(&attempt["end"]["cause"], "cause")?;
                nullable_string(&attempt["end"], "diagnostic")?;
            }
            _ => return Err("unknown attempt end".into()),
        }
        match attempt.get("retry_delay_ms") {
            Some(Value::Null) => {}
            Some(n) if n.as_u64().is_some() => {}
            _ => return Err("retry_delay_ms must be null or integer".into()),
        }
    }
    let mut slots = Vec::new();
    for slot in array(value, "slots")? {
        let state = match string(&slot["progress"], "state")? {
            "registered" => SlotState::Registered,
            "started" => SlotState::Started,
            "terminal" => SlotState::Terminal,
            _ => return Err("unknown slot progress".into()),
        };
        let outcome = match string(slot, "outcome")? {
            "succeeded" => SlotOutcome::Succeeded,
            "failed" => SlotOutcome::Failed,
            "not_executed" => SlotOutcome::NotExecuted,
            "indeterminate" => SlotOutcome::Indeterminate,
            _ => return Err("unknown slot outcome".into()),
        };
        let error_code = match &slot["reason"] {
            Value::Null => None,
            reason if reason["kind"] == "failed" => Some(string(&reason["value"], "code")?.into()),
            reason if reason["kind"] == "indeterminate" => None,
            _ => return Err("unknown slot reason".into()),
        };
        slots.push(Slot {
            invocation_id: string(slot, "invocation_id")?.into(),
            method: string(&slot["binding"], "method")?.into(),
            state,
            outcome,
            error_code,
        });
    }
    Ok((attempts, slots))
}
fn retry_notice(value: &Value) -> Result<RetryNotice, String> {
    let notice = &value["notice"];
    if string(notice, "level")? != "warn" {
        return Err("retry notice must be warn".into());
    }
    Ok(RetryNotice {
        attempt: uint(value, "attempt")?,
        delay_ms: uint(value, "delay_ms")?,
        text: string(notice, "text")?.into(),
    })
}
fn message_segments(message: &Value) -> Result<Vec<Segment>, String> {
    let mut segments = Vec::new();
    for part in array(message, "parts")? {
        let semantic = &part["semantic"];
        let (kind, text) = if let Some(text) = semantic.get("text") {
            (SegmentKind::Text, string(text, "text")?)
        } else if let Some(reasoning) = semantic.get("reasoning") {
            (SegmentKind::Reasoning, string(reasoning, "text")?)
        } else if semantic.get("tool_call").is_some() {
            continue;
        } else {
            return Err("unknown message part".into());
        };
        segments.push(Segment {
            kind,
            text: text.into(),
        });
    }
    Ok(segments)
}
fn segment_list(value: &Value) -> Result<Vec<Segment>, String> {
    array(value, "segments")?
        .iter()
        .map(|segment| {
            let kind = match string(segment, "kind")? {
                "text" => SegmentKind::Text,
                "reasoning" => SegmentKind::Reasoning,
                _ => return Err("unknown text segment kind".into()),
            };
            Ok(Segment {
                kind,
                text: string(segment, "text")?.into(),
            })
        })
        .collect()
}

/// 解析一个对话条目：视图、历史查询与 CLI `entry` 记录同形。
pub fn parse_entry(value: &Value) -> Result<Entry, String> {
    let node = uint(value, "node")?;
    let content = &value["content"];
    let parent = uint(content, "parent")?;
    let turn_id = string(content, "turn_id")?.to_owned();
    let kind = string(value, "kind")?;
    if kind != string(content, "kind")? {
        return Err("entry kind differs from its node".into());
    }
    let parsed = match kind {
        "user_message" => {
            for field in ["step", "retries", "summary"] {
                if value.get(field).is_some() {
                    return Err(format!("UserMessage entry must not carry `{field}`"));
                }
            }
            let ordinal = uint(value, "ordinal")?;
            if ordinal == 0 {
                return Err("UserMessage ordinal counts from one".into());
            }
            let text = message_segments(&content["message"])?
                .into_iter()
                .filter(|segment| segment.kind == SegmentKind::Text)
                .map(|segment| segment.text)
                .collect();
            EntryContent::UserMessage {
                ordinal,
                agent_name: string(content, "agent_name")?.into(),
                text,
            }
        }
        "step" => {
            if value.get("ordinal").is_some() {
                return Err("Step entry must not carry an ordinal".into());
            }
            let ordinal = uint(content, "step_ordinal")?;
            let progress = step_progress(&content["progress"])?;
            let stored_attempts = array(content, "attempts")?.len();
            let reply = match &content["reply"] {
                Value::Null => Vec::new(),
                reply => message_segments(reply)?,
            };
            let detail = &value["step"];
            let (attempts, slots) = step_detail(detail)?;
            if uint(detail, "node")? != node
                || detail["key"]["turn_id"] != content["turn_id"]
                || uint(&detail["key"], "ordinal")? != ordinal
                || detail["progress"] != content["progress"]
                || attempts.len() != stored_attempts
            {
                return Err("step detail differs from the node".into());
            }
            let retries = array(value, "retries")?
                .iter()
                .map(retry_notice)
                .collect::<Result<_, _>>()?;
            let continued = progress == StepProgress::Closed(StepEnd::Continue);
            let summary = match value.get("summary") {
                None if continued => None,
                Some(raw) if !continued => {
                    let summary = summary(raw)?;
                    if summary.progress != progress {
                        return Err("summary progress differs from the step".into());
                    }
                    Some(summary)
                }
                _ => {
                    return Err(
                        "exactly the step not closed with continue carries the Turn summary".into(),
                    );
                }
            };
            EntryContent::Step(StepEntry {
                ordinal,
                progress,
                reply,
                attempts,
                slots,
                retries,
                summary,
            })
        }
        _ => return Err("entry content must be a user message or a step".into()),
    };
    Ok(Entry {
        node,
        parent,
        turn_id,
        content: parsed,
        raw: value.clone(),
    })
}
fn head(value: &Value) -> Result<Head, String> {
    let position = uint(value, "position")?;
    let turn_id = nullable_string(value, "turn_id")?;
    if (position == 0) != turn_id.is_none() {
        return Err("only the root position has no Turn".into());
    }
    Ok(Head {
        session_id: string(value, "session_id")?.into(),
        position,
        agent: nullable_string(value, "agent")?,
        turn_id,
        owner: boolean(value, "owner")?,
        diagnostic: match value.get("diagnostic") {
            Some(Value::Null) => None,
            Some(error) => Some(local_error(error)?),
            None => return Err("`diagnostic` must be present".into()),
        },
        path_replaced: boolean(value, "path_replaced")?,
    })
}
fn submit(value: &Value) -> Result<Submit, String> {
    let turn_id = string(value, "turn_id")?.to_owned();
    Ok(match string(value, "stage")? {
        "sending" => Submit::Sending {
            turn_id,
            session_id: string(value, "session_id")?.into(),
            new: boolean(value, "new")?,
        },
        "receipt" => {
            let recovered = boolean(value, "recovered")?;
            let result = match string(value, "result")? {
                "accepted" => {
                    let turn = &value["turn"];
                    if string(turn, "turn_id")? != turn_id {
                        return Err("accepted Turn differs from the submitted identity".into());
                    }
                    ReceiptResult::Accepted {
                        session_id: string(turn, "session_id")?.into(),
                        user_node: uint(turn, "user_node")?,
                    }
                }
                "rejected" => ReceiptResult::Rejected(local_error(&value["error"])?),
                "not_accepted" => ReceiptResult::NotAccepted,
                "unknown" => ReceiptResult::Unknown(optional_error(value, "error")?),
                _ => return Err("unknown submit result".into()),
            };
            Submit::Receipt(Receipt {
                turn_id,
                result,
                recovered,
            })
        }
        _ => return Err("unknown submit stage".into()),
    })
}
fn cancel(value: &Value) -> Result<CancelRecord, String> {
    let session_id = optional_string(value, "session_id")?;
    Ok(match nullable_string(value, "turn_id")? {
        None => {
            if string(value, "status")? != "none" {
                return Err("nothing to cancel requires status none".into());
            }
            CancelRecord::NothingToCancel {
                session_id: session_id.ok_or("nothing to cancel names the session")?,
            }
        }
        Some(turn_id) => CancelRecord::Reply {
            session_id,
            turn_id,
            status: match string(value, "status")? {
                "taken" => CancelStatus::Taken,
                "not_taken" => CancelStatus::NotTaken,
                "unknown" => CancelStatus::Unknown,
                _ => return Err("unknown cancel status".into()),
            },
            owned: match value.get("owned") {
                None | Some(Value::Null) => None,
                Some(_) => Some(boolean(value, "owned")?),
            },
            error: optional_error(value, "error")?,
        },
    })
}

/// 解析一条 `--json` 记录，或 Local IPC `view` 消息的 `value`（同样以 `type` 为标签）。
pub fn parse_record(value: &Value) -> Result<SendRecord, String> {
    let kind = string(value, "type")?;
    Ok(match kind {
        "submit" => SendRecord::Submit(submit(value)?),
        "cancel" => SendRecord::Cancel(cancel(value)?),
        "head" => SendRecord::Head(head(value)?),
        "entry" => {
            // 条目本身不含传输标签；去掉 `type` 后与历史查询给出的 Entry 同形。
            let mut entry = value.clone();
            entry.as_object_mut().map(|fields| fields.remove("type"));
            SendRecord::Entry(Box::new(parse_entry(&entry)?))
        }
        "text" => SendRecord::Text(TextChunk {
            node: uint(value, "node")?,
            attempt: uint(value, "attempt")?,
            offset: uint(value, "offset")?,
            segments: segment_list(value)?,
        }),
        "connection" => SendRecord::Connection {
            status: match string(value, "status")? {
                "eof" => ConnectionStatus::Eof,
                "idle" => ConnectionStatus::Idle,
                "error" => ConnectionStatus::Error,
                _ => return Err("unknown connection status".into()),
            },
            error: optional_error(value, "error")?,
        },
        "error" => SendRecord::Error(local_error(value)?),
        _ => SendRecord::Unknown {
            type_name: kind.into(),
            raw: value.clone(),
        },
    })
}

/// 同一节点的新值只能延续旧值；同一个值重推允许。UserMessage 与已关闭的 Step 不再改变；
/// 未关闭的 Step 阶段不回退，已写入的回复、已结束的尝试与已到终态的槽位原样保留，
/// 尚未结束的尝试与槽位只前进，重试提示只在末尾追加（会话合同“已写入的节点”）。
fn continues(old: &Entry, new: &Entry) -> bool {
    if old.raw == new.raw {
        return true;
    }
    let (Some(before), Some(after)) = (old.step(), new.step()) else {
        return false;
    };
    let (held, next) = (&old.raw["step"], &new.raw["step"]);
    !before.progress.is_closed()
        && old.parent == new.parent
        && old.turn_id == new.turn_id
        && before.ordinal == after.ordinal
        && after.progress.rank() >= before.progress.rank()
        && (old.raw["content"]["reply"].is_null()
            || old.raw["content"]["reply"] == new.raw["content"]["reply"])
        && after.retries.starts_with(&before.retries)
        && before.attempts.len() <= after.attempts.len()
        && before.attempts.iter().enumerate().all(|(index, state)| {
            let (was, now) = (&held["attempts"][index], &next["attempts"][index]);
            match state {
                AttemptState::Ended => was == now,
                AttemptState::Sent => was["attempt"] == now["attempt"],
            }
        })
        && before.slots.len() <= after.slots.len()
        && before
            .slots
            .iter()
            .zip(&after.slots)
            .enumerate()
            .all(|(index, (was, now))| {
                was.invocation_id == now.invocation_id
                    && was.method == now.method
                    && now.state >= was.state
                    && (was.state != SlotState::Terminal
                        || held["slots"][index] == next["slots"][index])
            })
}

/// 一次观察连接持有的视图：最新会话头、按 NodeId 覆盖的条目与各尝试的生成中文字。
/// 每条连接用新的 `ViewState`：文字偏移按这条连接已收到的字节计算。
#[derive(Debug, Clone, Default)]
pub struct ViewState {
    pub head: Option<Head>,
    pub entries: BTreeMap<u64, Entry>,
    /// 按 (节点, 尝试) 合并相邻同类后的生成中文字。
    pub texts: BTreeMap<(u64, u64), Vec<Segment>>,
}
impl ViewState {
    /// 应用一条视图记录；同一节点不延续旧值或文字偏移不接续时报错，其余记录忽略。
    pub fn apply(&mut self, record: &SendRecord) -> Result<(), String> {
        match record {
            SendRecord::Head(head) => {
                if head.path_replaced {
                    self.entries.clear();
                    self.texts.clear();
                }
                self.head = Some(head.clone());
            }
            SendRecord::Entry(entry) => {
                if let Some(old) = self.entries.get(&entry.node)
                    && !continues(old, entry)
                {
                    let node = entry.node;
                    return Err(format!("node {node} does not continue its held value"));
                }
                self.entries.insert(entry.node, (**entry).clone());
            }
            SendRecord::Text(chunk) => {
                let TextChunk {
                    node,
                    attempt,
                    offset,
                    segments,
                } = chunk;
                let held = self.texts.entry((*node, *attempt)).or_default();
                // 偏移是这次尝试此前已发给这个观察者的字节数；已收到文字后不会再从 0 开始。
                let sent = held.iter().map(|segment| segment.text.len()).sum::<usize>() as u64;
                if *offset != sent {
                    return Err(format!(
                        "text for node {node} attempt {attempt} resumed at {offset} after {sent} bytes"
                    ));
                }
                for segment in segments {
                    push_segment(held, segment.clone());
                }
            }
            _ => {}
        }
        Ok(())
    }
    /// 条目按 NodeId 排列；同一 Session 路径上的节点按追加顺序编号，因此即路径顺序。
    pub fn raw_entries(&self) -> Vec<&Value> {
        self.entries.values().map(|entry| &entry.raw).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedJsonLine {
    pub line_number: usize,
    pub raw: Vec<u8>,
    pub reason: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SubmissionReceiptError {
    #[error("expected exactly one sending record, got {actual}")]
    SendingCount { actual: usize },
    #[error("expected exactly one submission receipt, got {actual}")]
    ReceiptCount { actual: usize },
    #[error("submission receipt does not match the sending TurnId")]
    TurnIdMismatch,
}
#[derive(Debug, Clone, Default)]
pub struct SendOutcome {
    pub records: Vec<SendRecord>,
    pub malformed_lines: Vec<MalformedJsonLine>,
    /// 正常退出时的提交回执违规；保留记录与命令证据供场景诊断。
    pub receipt_error: Option<SubmissionReceiptError>,
    pub exit_code: Option<SendExitCode>,
}
#[derive(Debug)]
pub struct SendExecution {
    pub command: CommandOutcome,
    pub parsed: SendOutcome,
}
impl SendExecution {
    pub fn clean_exit_code(&self) -> Option<i32> {
        if self.receipt_error.is_none() {
            self.command.clean_exit_code()
        } else {
            None
        }
    }
    pub fn is_clean_exit(&self, expected: i32) -> bool {
        self.clean_exit_code() == Some(expected)
    }
    /// 诊断只含进程元信息与回执规则错误；原始输出由场景另行脱敏保存。
    pub fn summary(&self) -> String {
        match &self.receipt_error {
            Some(error) => format!("{}; {error}", self.command.summary()),
            None => self.command.summary().to_string(),
        }
    }
}
impl std::ops::Deref for SendExecution {
    type Target = SendOutcome;
    fn deref(&self) -> &Self::Target {
        &self.parsed
    }
}
impl SendOutcome {
    /// `sending` 之后这次提交以一条 `rejected` 回执结束、没有别的记录时，回执中的 LocalError。
    /// 后台的拒绝与请求写出前提交端的拒绝（例如配置加载失败）都在这里，不是 `error` 记录。
    pub fn rejection(&self) -> Option<&LocalError> {
        match &self.records[..] {
            [
                SendRecord::Submit(Submit::Sending { turn_id, .. }),
                SendRecord::Submit(Submit::Receipt(Receipt {
                    turn_id: receipt,
                    result: ReceiptResult::Rejected(error),
                    ..
                })),
            ] if turn_id == receipt => Some(error),
            _ => None,
        }
    }
    /// 第一条 `submit`（发出请求前）写明的 TurnId、SessionId 与是否新建。
    pub fn sending(&self) -> Option<(&str, &str, bool)> {
        self.records.iter().find_map(|record| match record {
            SendRecord::Submit(Submit::Sending {
                turn_id,
                session_id,
                new,
            }) => Some((turn_id.as_str(), session_id.as_str(), *new)),
            _ => None,
        })
    }
    pub fn receipt(&self) -> Option<&Receipt> {
        self.records.iter().rev().find_map(|record| match record {
            SendRecord::Submit(Submit::Receipt(receipt)) => Some(receipt),
            _ => None,
        })
    }
    pub fn accepted_turn_id(&self) -> Option<&str> {
        self.receipt().and_then(|receipt| match &receipt.result {
            ReceiptResult::Accepted { .. } => Some(receipt.turn_id.as_str()),
            _ => None,
        })
    }
    pub fn accepted_session_id(&self) -> Option<&str> {
        self.receipt().and_then(|receipt| match &receipt.result {
            ReceiptResult::Accepted { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        })
    }
    pub fn heads(&self) -> impl Iterator<Item = &Head> {
        self.records.iter().filter_map(|record| match record {
            SendRecord::Head(head) => Some(head),
            _ => None,
        })
    }
    pub fn errors(&self) -> impl Iterator<Item = &LocalError> {
        self.records.iter().filter_map(|record| match record {
            SendRecord::Error(error) => Some(error),
            _ => None,
        })
    }
    pub fn cancels(&self) -> impl Iterator<Item = &CancelRecord> {
        self.records.iter().filter_map(|record| match record {
            SendRecord::Cancel(cancel) => Some(cancel),
            _ => None,
        })
    }
    /// 按 `node` 覆盖后的条目，按 NodeId 即路径顺序排列。
    pub fn entries(&self) -> Vec<&Entry> {
        latest_entries(&self.records).into_values().collect()
    }
    /// 依次应用全部视图记录得到的观察者状态；回退或文字偏移不接续时报错。
    pub fn view(&self) -> Result<ViewState, String> {
        let mut view = ViewState::default();
        for record in &self.records {
            view.apply(record)?;
        }
        Ok(view)
    }
    /// SessionId、TurnId、三态结局；本轮结束后才有。
    pub fn closed_result(&self) -> Option<(&str, &str, &str)> {
        closed_result(&self.records).map(|(sid, tid, outcome)| (sid, tid, outcome.as_str()))
    }
    pub fn has_completed(&self) -> bool {
        self.closed_result()
            .is_some_and(|(_, _, outcome)| outcome == "completed")
    }
    /// 本轮各 Step 条目最新值中的回复正文，按路径顺序连接。
    pub fn reply_text(&self) -> String {
        self.entries()
            .into_iter()
            .filter_map(Entry::step)
            .flat_map(|step| &step.reply)
            .filter(|segment| segment.kind == SegmentKind::Text)
            .map(|segment| segment.text.as_str())
            .collect()
    }
    /// 本轮各 Step 条目最新值中的调用槽位，按路径与槽位顺序。
    pub fn slots(&self) -> Vec<&Slot> {
        self.entries()
            .into_iter()
            .filter_map(Entry::step)
            .flat_map(|step| &step.slots)
            .collect()
    }
    pub fn type_sequence(&self) -> Vec<&str> {
        self.records.iter().map(SendRecord::type_name).collect()
    }
}
fn latest_entries(records: &[SendRecord]) -> BTreeMap<u64, &Entry> {
    let mut entries = BTreeMap::new();
    for record in records {
        match record {
            SendRecord::Head(head) if head.path_replaced => entries.clear(),
            SendRecord::Entry(entry) => {
                entries.insert(entry.node, &**entry);
            }
            _ => {}
        }
    }
    entries
}
/// 这一轮结束：最后一步的 `progress` 为 Closed，或未关闭且最新会话头说明这一轮没有 Owner。
fn closed_result(records: &[SendRecord]) -> Option<(&str, &str, Outcome)> {
    let last = latest_entries(records)
        .into_values()
        .filter(|entry| entry.step().is_some())
        .max_by_key(|entry| entry.step().map(|step| step.ordinal))?;
    let summary = last.step()?.summary.as_ref()?;
    let head = records.iter().rev().find_map(|record| match record {
        SendRecord::Head(head) => Some(head),
        _ => None,
    });
    let abandoned = head
        .is_some_and(|head| head.turn_id.as_deref() == Some(last.turn_id.as_str()) && !head.owner);
    if !summary.progress.is_closed() && !abandoned {
        return None;
    }
    let session_id = head.map(|head| head.session_id.as_str()).or_else(|| {
        records.iter().find_map(|record| match record {
            SendRecord::Submit(Submit::Sending { session_id, .. }) => Some(session_id.as_str()),
            _ => None,
        })
    })?;
    Some((session_id, &last.turn_id, summary.outcome))
}
fn usage_error(error: &LocalError) -> bool {
    matches!(
        error.kind.as_str(),
        "bad_request" | "not_found" | "conflict"
    )
}
/// 按记录推导退出码：先核对观察错误和已输出的回执，再使用持久结局。
/// 身份生成前的使用错误退出 2；进入流式观察后的 error 记录一律退出 1。
/// 进程退出、干预、EOF、回执规则和畸形行仍必须另外核验。
pub fn derive_send_exit_code(records: &[SendRecord]) -> SendExitCode {
    let error = records.iter().rev().find_map(|record| match record {
        SendRecord::Error(error) => Some(error),
        _ => None,
    });
    if let Some(error) = error {
        let streaming = records.iter().any(|record| {
            matches!(
                record,
                SendRecord::Submit(_)
                    | SendRecord::Head(_)
                    | SendRecord::Entry(_)
                    | SendRecord::Text(_)
                    | SendRecord::Connection { .. }
            )
        });
        return if !streaming && usage_error(error) {
            SendExitCode::InputError
        } else {
            SendExitCode::Failed
        };
    }
    let closed = closed_result(records);
    let target = records
        .iter()
        .find_map(|record| match record {
            SendRecord::Submit(Submit::Sending { turn_id, .. }) => Some(turn_id.as_str()),
            _ => None,
        })
        .or_else(|| closed.map(|(_, turn_id, _)| turn_id));
    if records.iter().any(|record| match record {
        SendRecord::Connection { .. } => true,
        SendRecord::Head(head) => {
            head.diagnostic.is_some()
                && target.is_none_or(|target| head.turn_id.as_deref() == Some(target))
        }
        _ => false,
    }) {
        return SendExitCode::Failed;
    }
    if let Some(receipt) = records.iter().find_map(|record| match record {
        SendRecord::Submit(Submit::Receipt(receipt)) => Some(receipt),
        _ => None,
    }) {
        match &receipt.result {
            ReceiptResult::Rejected(error) if usage_error(error) => {
                return SendExitCode::InputError;
            }
            ReceiptResult::Accepted { .. } => {}
            _ => return SendExitCode::Failed,
        }
    }
    match closed.map(|(_, _, outcome)| outcome) {
        Some(Outcome::Completed) => SendExitCode::Committed,
        Some(Outcome::Cancelled) => SendExitCode::InputError,
        _ => SendExitCode::Failed,
    }
}
/// 仅用于 JSON 模式的 send/run/follow 输出；正常退出的提交必须交付唯一回执。
/// `exit_code` 为 None 表示进程未正常退出，允许保留缺少回执的部分输出。
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
            .and_then(|line| serde_json::from_str::<Value>(line).map_err(|e| e.to_string()))
            .and_then(|value| parse_record(&value));
        match parsed {
            Ok(record) => result.records.push(record),
            Err(reason) => result.malformed_lines.push(MalformedJsonLine {
                line_number: index + 1,
                raw: raw.to_vec(),
                reason,
            }),
        }
    }
    // 返回违规而非 panic，让场景先保存命令证据，再判定失败。
    if exit_code.is_some() {
        result.receipt_error = submission_receipt_error(&result.records);
    }
    result
}

fn submission_receipt_error(records: &[SendRecord]) -> Option<SubmissionReceiptError> {
    let mut sending = records.iter().filter_map(|record| match record {
        SendRecord::Submit(Submit::Sending { turn_id, .. }) => Some(turn_id),
        _ => None,
    });
    let turn_id = sending.next()?;
    let actual = 1 + sending.count();
    if actual != 1 {
        return Some(SubmissionReceiptError::SendingCount { actual });
    }
    let mut receipts = records.iter().filter_map(|record| match record {
        SendRecord::Submit(Submit::Receipt(receipt)) => Some(receipt),
        _ => None,
    });
    let receipt = receipts.next();
    let actual = usize::from(receipt.is_some()) + receipts.count();
    if actual != 1 {
        return Some(SubmissionReceiptError::ReceiptCount { actual });
    }
    if receipt.is_some_and(|receipt| &receipt.turn_id != turn_id) {
        return Some(SubmissionReceiptError::TurnIdMismatch);
    }
    None
}

/// 首次提交由 CLI 生成 SessionId；驱动在接受回执之后才把它用于 `--session`。
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
        if parsed.receipt_error.is_none()
            && let Some(id) = parsed.accepted_session_id()
        {
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
#[cfg(test)]
#[path = "session_send_tests.rs"]
mod tests;
