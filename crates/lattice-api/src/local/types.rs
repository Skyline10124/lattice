// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

pub use lancet_protocol::TurnId;
pub use lattice_session::{
    CommittedTurnObservation, PendingObservation, SessionObservation, TranscriptPage,
    TurnObservation,
};

/// 连接状态与持久 Turn 状态分别表达；任何传输错误都不证明请求未被接受。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LocalError {
    pub kind: LocalErrorKind,
    pub message: String,
    pub recovery: Option<crate::RecoveryRequired>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalErrorKind {
    BadRequest,
    NotFound,
    Conflict,
    Busy,
    Stopping,
    StoreUnavailable,
    RecoveryRequired,
    Internal,
    NotRunning,
    Transport,
    Protocol,
    Permission,
    AlreadyRunning,
}

pub type Result<T> = std::result::Result<T, LocalError>;

impl LocalError {
    pub(super) fn new(kind: LocalErrorKind, message: &str) -> Self {
        Self {
            kind,
            message: message.into(),
            recovery: None,
        }
    }
}

impl From<crate::ApiError> for LocalError {
    fn from(error: crate::ApiError) -> Self {
        use crate::ApiError;
        use LocalErrorKind::*;
        // 错误分类可以跨 IPC，底层字符串可能包含输入或秘密，不能原样公开。
        let (kind, message, recovery) = match error {
            ApiError::NotFound(_) => (NotFound, "session or Turn not found", None),
            ApiError::BadRequest(_) => (
                BadRequest,
                "request or frozen configuration is invalid",
                None,
            ),
            ApiError::Conflict(_) => (
                Conflict,
                "request identity or session binding conflicts",
                None,
            ),
            ApiError::Busy(_) => (Busy, "session is busy", None),
            ApiError::StoreUnavailable(_) => (
                StoreUnavailable,
                "store unavailable; inspect durable state",
                None,
            ),
            ApiError::Internal(_) => (
                Internal,
                "operation failed; inspect durable state before recovery",
                None,
            ),
            ApiError::RecoveryRequired(reason) => (
                RecoveryRequired,
                "durable reconciliation required",
                Some(reason),
            ),
            ApiError::EvidenceDurabilityFailed(reason) => (
                RecoveryRequired,
                "evidence durability not confirmed",
                Some(crate::RecoveryRequired::EvidenceDurabilityFailed(reason)),
            ),
        };
        Self {
            kind,
            message: message.into(),
            recovery,
        }
    }
}

impl From<std::io::Error> for LocalError {
    fn from(error: std::io::Error) -> Self {
        let kind = match error.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                LocalErrorKind::NotRunning
            }
            std::io::ErrorKind::PermissionDenied => LocalErrorKind::Permission,
            _ => LocalErrorKind::Transport,
        };
        Self::new(
            kind,
            "local connection failed; acknowledgement may be unknown",
        )
    }
}

/// 客户端在第一次提交前保留此身份；重试不能另分配 TurnId。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TurnRequest {
    pub session_id: String,
    pub turn_id: TurnId,
    pub workspace: std::path::PathBuf,
    pub input: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "acceptance", rename_all = "snake_case")]
pub enum Acceptance {
    Accepted { turn_id: TurnId },
    Existing { turn: TurnObservation },
    Recovered { turn: Box<CommittedTurnObservation> },
    ConfirmationUnknown { turn_id: TurnId },
    NeedsConfiguration,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub epoch: String,
    pub stopping: bool,
    pub owners: usize,
    /// 本次后台生命周期内未确认成功的所有者操作数；不是持久 pending 计数。
    pub unconfirmed_operations: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventCursor {
    pub epoch: String,
    pub sequence: u64,
}

/// Snapshot/Gap 后替换持久视图并清空临时增量；结果正文经固定 commit 字节页重载。
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Observation {
    Snapshot {
        view: Box<SessionObservation>,
        cursor: EventCursor,
        owned: bool,
    },
    Event {
        cursor: EventCursor,
        event: crate::TurnEvent,
    },
    Gap {
        cursor: EventCursor,
    },
    Finished {
        view: Box<SessionObservation>,
        cursor: EventCursor,
        error: Option<LocalError>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum SessionWrite {
    Checkout { target: String },
    Branch { name: String, from: Option<String> },
    Reset { target: String },
    SetAgent { name: String },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "value", rename_all = "snake_case")]
pub enum SessionWriteResult {
    Checkout(crate::GraphHead),
    Branch(crate::GraphRef),
    Reset(crate::ResetResult),
    SetAgent(crate::GraphSession),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", content = "value", rename_all = "snake_case")]
pub enum LocalRewindResult {
    Success(crate::RewindSuccess),
    Conflict(crate::RewindConflict),
    AuditPublicationFailed(Box<crate::RewindPartialState>),
}

// 请求与秘密材料刻意不实现 Debug，协议处理不记录正文。
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    Status,
    Stop,
    List,
    History {
        session_id: String,
        head: String,
        offset: usize,
        limit: usize,
    },
    SessionStatus {
        session_id: String,
    },
    Log {
        session_id: String,
    },
    Diff {
        session_id: String,
        from: String,
        to: String,
    },
    Rewind {
        session_id: String,
        target: String,
        workspace: std::path::PathBuf,
    },
    Open {
        session_id: String,
    },
    Create {
        request: crate::CreateSessionRequest,
    },
    Write {
        session_id: String,
        change: SessionWrite,
    },
    Submit {
        request: TurnRequest,
        configuration: Option<Box<super::config::FrozenConfiguration>>,
    },
    Recover {
        session_id: String,
    },
    Query {
        session_id: String,
        turn_id: Option<TurnId>,
    },
    Commit {
        session_id: String,
        commit_id: String,
    },
    Transcript {
        session_id: String,
        commit_id: String,
        offset: usize,
        limit: usize,
    },
    Cancel {
        session_id: String,
        turn_id: TurnId,
    },
    Subscribe {
        session_id: String,
        turn_id: TurnId,
        cursor: Option<EventCursor>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "response", content = "value", rename_all = "snake_case")]
pub(super) enum Response {
    Status(DaemonStatus),
    Sessions(Vec<crate::SessionGraphSummary>),
    History(Vec<String>),
    SessionStatus(crate::SessionStatus),
    Log(Vec<crate::TurnLogEntry>),
    Diff(crate::GraphDiff),
    Write(SessionWriteResult),
    Rewind(LocalRewindResult),
    Session(crate::GraphSession),
    View(Box<SessionObservation>),
    Commit(Box<CommittedTurnObservation>),
    Transcript(TranscriptPage),
    Acceptance(Acceptance),
    Observation(Observation),
    Cancelled { owned: bool },
    Error(LocalError),
}
