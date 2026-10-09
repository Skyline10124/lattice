// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub use crate::turn::{AttemptSegments, TextSegment, TurnRef};
pub use lancet_protocol::{InvocationId, ModelAttemptKey, SessionId, TurnId};
pub use lattice_conversation::{ResultPayload, StepContent, StoredToolResult, SuccessContent};
pub use lattice_session::store::{
    AuditPage, AuditReplay, AuditTailPage, ChangeSeq, HistoryAnchor, HistoryCursor,
    MaterialSummary, Node, NodeFragment, NodeId, NodeKind, NodeRef, NoticeLevel,
    OperatorInvocationState, OperatorStatus, Page, PublicTurnOutcome, SessionListRow, SessionLog,
    SessionRecord, StepDetail, StepDetailState, StepEnd, StepKey, TurnDetail, TurnSummary,
    UsageSummary,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnView {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub user_node: NodeId,
    pub agent_name: String,
    pub accepted_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    pub summary: Option<TurnSummary>,
    pub usage: UsageSummary,
}
impl From<&TurnDetail> for TurnView {
    fn from(turn: &TurnDetail) -> Self {
        Self {
            session_id: turn.session_id,
            turn_id: turn.turn_id,
            user_node: turn.user_node,
            agent_name: turn.agent_name.clone(),
            accepted_at: turn.accepted_at,
            ended_at: turn.ended_at,
            summary: turn.summary.clone(),
            usage: turn.usage.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActiveStepView {
    pub key: lattice_session::store::StepKey,
    pub state: ActiveStateView,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ActiveStateView {
    Requesting {
        attempt: u32,
    },
    RetryReady {
        next_attempt: u32,
    },
    Executing {
        reply: lattice_conversation::NativeMessage<lattice_conversation::BoundToolCall>,
        slots: Vec<SlotView>,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SlotView {
    pub invocation: lattice_session::store::OperatorInvocation,
    pub result: Option<lattice_session::store::StoredToolResult>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ObservationSnapshot {
    pub session: SessionRecord,
    pub current_agent: Option<String>,
    pub selected_turn: Option<TurnDetail>,
    pub active: Option<ActiveStepView>,
    pub history: HistoryAnchor,
    pub attempt: Option<AttemptSegments>,
    pub cursor: EventCursor,
    pub owned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HistoryEntry {
    pub node: NodeId,
    pub agent_name: String,
    pub content: Node,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LocalHistory {
    pub anchor: HistoryAnchor,
    pub entries: Vec<HistoryEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HistoryFragmentView {
    pub fragment: NodeFragment,
    pub agent_name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HistoryPageView {
    pub anchor: HistoryAnchor,
    pub fragments: Vec<HistoryFragmentView>,
    pub next: Option<HistoryCursor>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum HistoryReadContext {
    Worker,
    Page {
        anchor: HistoryAnchor,
        cursor: Option<HistoryCursor>,
    },
    Node {
        node: NodeRef,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{error}")]
pub struct HistoryReadError {
    pub context: HistoryReadContext,
    pub error: LocalError,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationPhase {
    Live,
    NoOwner,
    Finished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotSubmittedStage {
    NeedsConfiguration,
    FreezingConfiguration,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelStatus {
    Pending,
    Taken,
    NotTaken,
    Unknown,
    Moot,
}
#[derive(Clone, Debug, PartialEq)]
pub struct LocalState {
    pub view: Box<ObservationSnapshot>,
    pub phase: ObservationPhase,
    pub error: Option<LocalError>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectionStatus {
    Eof,
    Idle,
    Error(LocalError),
}

/// 状态先交付；历史真正装入后才发布 Finished，旧代读取错误仍保留为 stale。
#[derive(Clone, Debug, PartialEq)]
pub enum LocalUpdate {
    Submission {
        turn_id: TurnId,
        result: Result<Acceptance>,
    },
    SubmissionUnknown {
        turn_id: TurnId,
        error: Option<LocalError>,
    },
    NotSubmitted {
        turn_id: TurnId,
        stage: NotSubmittedStage,
    },
    State {
        view: Box<ObservationSnapshot>,
        phase: ObservationPhase,
        error: Option<LocalError>,
    },
    Reset {
        view: Box<ObservationSnapshot>,
        generation: u64,
    },
    History {
        anchor: HistoryAnchor,
        generation: u64,
        result: std::result::Result<Box<LocalHistory>, HistoryReadError>,
        stale: bool,
    },
    Event {
        cursor: EventCursor,
        event: crate::TurnEvent,
    },
    Finished {
        turn: Option<TurnRef>,
        anchor: HistoryAnchor,
        generation: u64,
    },
    Cancel {
        turn_id: TurnId,
        status: CancelStatus,
        error: Option<LocalError>,
    },
    Connection(ConnectionStatus),
}
#[derive(Clone, Debug, PartialEq)]
pub struct DetachReport {
    pub session_id: Option<SessionId>,
    pub turn_id: Option<TurnId>,
    pub submission: Option<Result<Acceptance>>,
    pub submission_diagnostic: Option<LocalError>,
    pub not_submitted: Option<NotSubmittedStage>,
    pub history_error: Option<HistoryReadError>,
    pub cancel: Option<CancelStatus>,
    pub cancel_error: Option<LocalError>,
    pub state: Option<LocalState>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LocalError {
    pub kind: LocalErrorKind,
    pub message: String,
    pub recovery: Option<RecoveryNeed>,
    pub store: Option<StoreDiagnostic>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryNeed {
    pub turn: Option<TurnRef>,
    pub cause: RecoveryCause,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCause {
    CommitFailed,
    WriteDisabled,
    WorkerFailed,
    ReopenFailed,
    ClosureFailed,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoreDiagnostic {
    NotInitialized {
        path: PathBuf,
    },
    LegacyStores {
        paths: Vec<PathBuf>,
    },
    UnsupportedVersion {
        table: String,
        key: String,
        found: u32,
        supported: u32,
    },
    Corruption {
        table: String,
        key: String,
    },
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
            store: None,
        }
    }
    pub(crate) fn recovery(cause: RecoveryCause) -> Self {
        Self {
            recovery: Some(RecoveryNeed { turn: None, cause }),
            ..Self::new(
                LocalErrorKind::RecoveryRequired,
                "session store recovery required",
            )
        }
    }
}
impl From<crate::ApiError> for LocalError {
    fn from(error: crate::ApiError) -> Self {
        use crate::ApiError;
        use LocalErrorKind::*;
        use lattice_session::store::StoreError;
        let (kind, message) = match &error {
            ApiError::NotFound(_) => (NotFound, "session or Turn not found"),
            ApiError::Configuration(reason) => return Self::new(BadRequest, &reason.to_string()),
            ApiError::BadRequest(_) => (BadRequest, "request or frozen configuration is invalid"),
            ApiError::Conflict(_) => (Conflict, "request identity or Session binding conflicts"),
            ApiError::Busy(_) => (Busy, "session is busy"),
            ApiError::StoreUnavailable(_) => (StoreUnavailable, "store unavailable"),
            ApiError::Internal(_) => (Internal, "operation failed; inspect durable state"),
            ApiError::WorkerFailed => return Self::recovery(RecoveryCause::WorkerFailed),
            ApiError::Store(error) => {
                let mut result = Self::new(StoreUnavailable, "session store unavailable");
                result.store = match error.as_ref() {
                    StoreError::NotInitialized { path } => {
                        Some(StoreDiagnostic::NotInitialized { path: path.clone() })
                    }
                    StoreError::LegacyStores { paths } => Some(StoreDiagnostic::LegacyStores {
                        paths: paths.clone(),
                    }),
                    StoreError::UnsupportedVersion {
                        table,
                        key,
                        found,
                        supported,
                    } => Some(StoreDiagnostic::UnsupportedVersion {
                        table: table.clone(),
                        key: key.clone(),
                        found: *found,
                        supported: *supported,
                    }),
                    StoreError::Corruption { table, key, .. } => {
                        Some(StoreDiagnostic::Corruption {
                            table: table.clone(),
                            key: key.clone(),
                        })
                    }
                    StoreError::CommitFailed { .. }
                    | StoreError::CreationDurabilityUnknown { .. } => {
                        return Self::recovery(RecoveryCause::CommitFailed);
                    }
                    StoreError::WriteDisabled => {
                        return Self::recovery(RecoveryCause::WriteDisabled);
                    }
                    StoreError::RecoveryRequired { .. } => {
                        return Self::recovery(RecoveryCause::ClosureFailed);
                    }
                    StoreError::NotFound { .. } => {
                        result.kind = NotFound;
                        None
                    }
                    StoreError::Conflict { .. } | StoreError::SessionCollision { .. } => {
                        result.kind = Conflict;
                        None
                    }
                    StoreError::Busy { .. } => {
                        result.kind = Busy;
                        None
                    }
                    StoreError::InvalidInput { .. } | StoreError::InvalidCursor => {
                        result.kind = BadRequest;
                        None
                    }
                    _ => None,
                };
                return result;
            }
        };
        Self::new(kind, message)
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

/// 私有请求不实现正文 Debug。SID 只在首次接受后确定。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TurnRequest {
    pub session_id: Option<SessionId>,
    pub turn_id: TurnId,
    pub workspace: PathBuf,
    pub input: String,
    pub agent: Option<String>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnTarget {
    pub turn_id: TurnId,
    pub session_id: Option<SessionId>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "acceptance", rename_all = "snake_case")]
pub enum Acceptance {
    Accepted {
        turn: TurnView,
    },
    Existing {
        turn: TurnView,
    },
    ConfirmationUnknown {
        turn_id: TurnId,
        session_id: Option<SessionId>,
    },
    NeedsConfiguration {
        agent_name: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub epoch: String,
    pub stopping: bool,
    pub owners: usize,
    pub unconfirmed_operations: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventCursor {
    pub epoch: String,
    pub sequence: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Observation {
    Snapshot {
        view: Box<ObservationSnapshot>,
    },
    Event {
        cursor: EventCursor,
        event: crate::TurnEvent,
    },
    Finished {
        turn: Option<TurnRef>,
        cursor: EventCursor,
        history: LocalHistory,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct AuditSelection {
    pub session_id: Option<SessionId>,
    pub turn_id: Option<TurnId>,
    pub method: Option<String>,
    pub outcome: Option<lattice_conversation::ToolResultOutcome>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    Status,
    Stop,
    ResolveSession {
        prefix: String,
    },
    List {
        after: Option<SessionId>,
        limit: u32,
    },
    Log {
        session_id: SessionId,
    },
    Turn {
        target: TurnTarget,
    },
    History {
        anchor: HistoryAnchor,
        cursor: Option<HistoryCursor>,
        max_bytes: u32,
    },
    Rewind {
        session_id: SessionId,
        target: NodeId,
        workspace: PathBuf,
    },
    Submit {
        request: TurnRequest,
        configuration: Option<Box<super::config::FrozenConfiguration>>,
    },
    Query {
        session_id: Option<SessionId>,
        turn_id: Option<TurnId>,
    },
    Cancel {
        target: TurnTarget,
    },
    Subscribe {
        target: TurnTarget,
        cursor: Option<EventCursor>,
    },
    AuditQuery {
        workspace: PathBuf,
        selection: AuditSelection,
        after: Option<InvocationId>,
        limit: u32,
    },
    AuditTail {
        workspace: PathBuf,
        selection: AuditSelection,
        after: ChangeSeq,
        limit: u32,
    },
    AuditReplay {
        workspace: PathBuf,
        session_id: SessionId,
        turn_id: TurnId,
    },
    OperatorStatus {
        workspace: PathBuf,
    },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "response", content = "value", rename_all = "snake_case")]
pub(super) enum Response {
    Status(DaemonStatus),
    Session(SessionRecord),
    Sessions(Page<SessionListRow, SessionId>),
    Log(SessionLog),
    Turn(TurnDetail),
    History(HistoryPageView),
    Rewind(SessionRecord),
    Acceptance(Acceptance),
    View(Box<ObservationSnapshot>),
    Observation(Observation),
    Cancelled { owned: bool },
    Audit(AuditPage),
    AuditTail(AuditTailPage),
    AuditReplay(AuditReplay),
    OperatorStatus(OperatorStatus),
    Error(LocalError),
    HistoryError(HistoryReadError),
}

#[cfg(test)]
mod v3_tests {
    use super::*;
    #[test]
    fn first_submission_and_safe_store_diagnostics_have_v3_shapes() {
        let request: Request = serde_json::from_value(serde_json::json!({
            "operation":"submit", "request": {"session_id":null,
            "turn_id":"00000000-0000-4000-8000-000000000001", "workspace":"/tmp/project", "input":" original ", "agent":"alpha"},
            "configuration":null
        })).expect("first submission has no SessionId");
        assert!(matches!(
            request,
            Request::Submit {
                request: TurnRequest {
                    session_id: None,
                    agent: Some(_),
                    ..
                },
                ..
            }
        ));
        let error = LocalError::from(crate::ApiError::from(
            lattice_session::store::StoreError::LegacyStores {
                paths: vec![
                    "/tmp/state/sessions.git".into(),
                    "/tmp/state/projects/p/invocations.git".into(),
                ],
            },
        ));
        assert!(
            matches!(error.store, Some(StoreDiagnostic::LegacyStores { ref paths }) if paths.len() == 2)
        );
        let error = LocalError::from(crate::ApiError::from(
            lattice_session::store::StoreError::Corruption {
                table: "nodes".into(),
                key: "1".into(),
                detail: "secret-body".into(),
            },
        ));
        let wire = serde_json::to_string(&error).unwrap();
        assert!(!wire.contains("secret-body"));
        assert!(matches!(
            error.store,
            Some(StoreDiagnostic::Corruption { .. })
        ));
    }
}
