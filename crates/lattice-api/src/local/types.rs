// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::{Deserialize, Serialize};

pub use lancet_protocol::TurnId;
pub use lattice_session::{
    CommittedTurnObservation, PendingObservation, SessionObservation, TranscriptPage,
    TurnObservation,
};

/// SelectedTurn 供单 Turn 输出；Session 供完整会话显示。两者都只读取传入的固定切点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryScope {
    SelectedTurn,
    Session,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalHistory {
    pub view: SessionObservation,
    pub entries: Vec<LocalHistoryEntry>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalHistoryEntry {
    pub turn: TurnObservation,
    pub agent_name: String,
    pub messages: Vec<super::NativeMessage<super::BoundToolCall>>,
}

/// 错误指向实际读取的固定对象，而不是错误到达时的当前 head。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryReadContext {
    /// 读取任务本身异常退出；具体切点保留在 History 更新的 view 中。
    Worker,
    Page {
        head: String,
        offset: usize,
    },
    Commit {
        object_id: String,
    },
    Transcript {
        object_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{error}")]
pub struct HistoryReadError {
    pub context: HistoryReadContext,
    pub error: LocalError,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationPhase {
    Live,
    /// 此切点没有观察 Owner；与取消资格、是否已有持久 pending 分开。
    NoOwner,
    /// 后台交付独立完成回执；可能仍有 pending 或完成诊断。
    Finished,
}

/// 只描述本客户端停止后续配置发送的位置，不断言 TurnId 全局从未执行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotSubmittedStage {
    NeedsConfiguration,
    FreezingConfiguration,
}

/// 接管事实与最终 outcome 分开；Moot 只结束未确认意图，不覆盖 Taken。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelStatus {
    Pending,
    Taken,
    NotTaken,
    Unknown,
    Moot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionConfirmation {
    Unknown,
    Confirmed,
}

/// 完成确认及诊断独立于正文加载和连接状态。
#[derive(Clone, Debug, PartialEq)]
pub struct LocalState {
    pub view: Box<SessionObservation>,
    pub phase: ObservationPhase,
    pub confirmation: CompletionConfirmation,
    pub error: Option<LocalError>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResetReason {
    Snapshot,
    Gap,
    Reconnect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Eof,
    Idle,
    Error(LocalError),
}

/// 文字段的显示动作；StartIncomplete 表示切点前的文字可能缺失，不合成模型事件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAction {
    None,
    Start,
    StartIncomplete,
    Continue,
    Finish,
}

/// 消费顺序：State → Reset → 此切点的 Event/History。History 只替换持久区域，
/// 不清除此前已经交付的同切点临时事件；旧切点成功不交付，失败带 stale=true。
#[derive(Clone, Debug, PartialEq)]
pub enum LocalUpdate {
    Submission {
        turn_id: TurnId,
        result: Result<Acceptance>,
    },
    /// 首次取消后的提交回执等待已到期；结果未知，句柄继续同身份只读观察，
    /// 并保留晚到回执及原取消意图。这不是拒绝或未执行的证明。
    /// error 仅为交接后原 Submit 的本地传输诊断，不结束观察或覆盖已知完成。
    SubmissionUnknown {
        turn_id: TurnId,
        error: Option<LocalError>,
    },
    NotSubmitted {
        turn_id: TurnId,
        stage: NotSubmittedStage,
    },
    State {
        view: Box<SessionObservation>,
        cursor: Option<EventCursor>,
        phase: ObservationPhase,
        confirmation: CompletionConfirmation,
        error: Option<LocalError>,
    },
    Reset {
        reason: ResetReason,
        cursor: Option<EventCursor>,
    },
    History {
        view: Box<SessionObservation>,
        result: std::result::Result<Box<LocalHistory>, HistoryReadError>,
        stale: bool,
    },
    Event {
        cursor: EventCursor,
        event: crate::TurnEvent,
        text: TextAction,
    },
    Cancel {
        turn_id: TurnId,
        status: CancelStatus,
        error: Option<LocalError>,
    },
    Connection(ConnectionStatus),
}

/// 普通退出只结束本地观察；已知固定结果与完成确认继续可供调用者使用。
#[derive(Clone, Debug, PartialEq)]
pub struct DetachReport {
    pub session_id: String,
    pub turn_id: Option<TurnId>,
    pub submission: Option<Result<Acceptance>>,
    /// 交接后原 Submit 的非终结传输诊断；submission 仍为 None（接受未知）。
    pub submission_diagnostic: Option<LocalError>,
    pub not_submitted: Option<NotSubmittedStage>,
    /// 尚未被后续成功正文读取消解的最近失败；完成确认和退出本身不清除它。
    /// 旧失败仍通过 History 更新携带原 view/context 交付，不作为后来成功读取的结局。
    pub history_error: Option<HistoryReadError>,
    pub cancel: Option<CancelStatus>,
    /// 原 turn_id 当前取消意图的诊断。新意图或 true/false 回执清除；
    /// 同意图补发等待、超时及 Moot 保留最近错误，不构成永久错误日志。
    pub cancel_error: Option<LocalError>,
    pub state: Option<LocalState>,
}

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
