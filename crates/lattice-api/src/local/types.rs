// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub use crate::models::ModelCandidates;
pub use crate::turn::{TextSegment, TurnRef};
pub use lancet_protocol::{InvocationId, ModelAttemptKey, SessionId, TurnId};
pub use lattice_conversation::{ResultPayload, StepContent, StoredToolResult, SuccessContent};
pub use lattice_llm::auth::{AuthState, AuthStatus, ProviderAuthStatus};
pub use lattice_session::store::{
    AttemptProgress, AuditPage, AuditReplay, AuditTailPage, ChangeSeq, ConversationEntry,
    MaterialSummary, Node, NodeId, NodeKind, NodeRef, Notice, NoticeLevel, OperatorInvocationState,
    OperatorReason, OperatorStatus, Page, PublicTurnOutcome, RetryNotice, SessionLog,
    SessionRecord, SlotProgress, StepDetail, StepEnd, StepKey, StepProgress, TurnDetail,
    TurnSummary, UsageSummary,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const WATCH_TURNS: u32 = 10;
pub const HISTORY_TURNS: u32 = 10;
pub const MAX_HISTORY_TURNS: u32 = 100;

pub use crate::session::RequestConflict as ConflictReason;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnView {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub user_node: NodeId,
    pub agent_name: String,
    pub accepted_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: chrono::DateTime<chrono::Utc>,
    pub summary: TurnSummary,
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
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TurnLookup {
    Accepted { turn: TurnDetail },
    InFlight,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionHead {
    pub session_id: SessionId,
    pub position: NodeId,
    pub agent: Option<String>,
    pub turn_id: Option<TurnId>,
    pub owner: bool,
    pub diagnostic: Option<LocalError>,
    pub path_replaced: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TextUpdate {
    pub node: NodeId,
    pub attempt: u32,
    pub offset: usize,
    pub segments: Vec<TextSegment>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ViewUpdate {
    Head(SessionHead),
    Entry {
        #[serde(flatten)]
        entry: Box<ConversationEntry>,
    },
    Text(TextUpdate),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LocalHistory {
    pub entries: Vec<ConversationEntry>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionListRow {
    #[serde(flatten)]
    pub session: lattice_session::store::SessionListRow,
    pub owner: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelStatus {
    Pending,
    Taken,
    NotTaken,
    Unknown,
    Moot,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "error", rename_all = "snake_case")]
pub enum ConnectionStatus {
    Eof,
    Idle,
    Error(LocalError),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum SubmissionResult {
    Accepted {
        turn: TurnView,
    },
    Rejected {
        error: LocalError,
    },
    NotAccepted,
    Unknown {
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<LocalError>,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SubmissionReceipt {
    pub turn_id: TurnId,
    #[serde(flatten)]
    pub result: SubmissionResult,
    pub recovered: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum SubmissionUpdate {
    Sending {
        turn_id: TurnId,
        session_id: SessionId,
        new: bool,
    },
    Receipt(SubmissionReceipt),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalUpdate {
    Submit {
        #[serde(flatten)]
        update: SubmissionUpdate,
    },
    Cancel {
        turn_id: TurnId,
        status: CancelStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        owned: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<LocalError>,
    },
    Head(SessionHead),
    Entry {
        #[serde(flatten)]
        entry: Box<ConversationEntry>,
    },
    Text(TextUpdate),
    Connection {
        #[serde(flatten)]
        status: ConnectionStatus,
    },
    Error {
        #[serde(flatten)]
        error: LocalError,
    },
}
impl From<ViewUpdate> for LocalUpdate {
    fn from(value: ViewUpdate) -> Self {
        match value {
            ViewUpdate::Head(head) => Self::Head(head),
            ViewUpdate::Entry { entry } => Self::Entry { entry },
            ViewUpdate::Text(text) => Self::Text(text),
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct DetachReport {
    pub session_id: SessionId,
    pub turn_id: Option<TurnId>,
    pub submission: Option<SubmissionReceipt>,
    pub submission_diagnostic: Option<LocalError>,
    pub cancel: Option<CancelStatus>,
    pub cancel_error: Option<LocalError>,
    pub head: Option<SessionHead>,
    pub summary: Option<TurnSummary>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LocalError {
    pub kind: LocalErrorKind,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<ConflictReason>,
    pub recovery: Option<RecoveryNeed>,
    pub store: Option<Box<StoreDiagnostic>>,
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
        path: PathBuf,
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
            conflict: None,
            recovery: None,
            store: None,
        }
    }
    pub(crate) fn recovery(cause: RecoveryCause) -> Self {
        Self {
            recovery: Some(RecoveryNeed { turn: None, cause }),
            ..Self::new(
                LocalErrorKind::StoreUnavailable,
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
            ApiError::FrontendConfiguration(reason) => {
                return Self::new(BadRequest, &reason.to_string());
            }
            ApiError::BadRequest(_) => (BadRequest, "request or frozen configuration is invalid"),
            ApiError::Conflict(reason) => {
                return Self {
                    conflict: Some(*reason),
                    ..Self::new(Conflict, "request identity or Session binding conflicts")
                };
            }
            ApiError::Busy(_) => (Busy, "session is busy"),
            ApiError::StoreUnavailable(_) => (StoreUnavailable, "store unavailable"),
            ApiError::Internal(_) => (Internal, "operation failed; inspect durable state"),
            ApiError::WorkerFailed => return Self::recovery(RecoveryCause::WorkerFailed),
            ApiError::Store(error) => {
                let mut result = Self::new(StoreUnavailable, "session store unavailable");
                result.store = (match error.as_ref() {
                    StoreError::NotInitialized { path } => {
                        Some(StoreDiagnostic::NotInitialized { path: path.clone() })
                    }
                    StoreError::LegacyStores { paths } => Some(StoreDiagnostic::LegacyStores {
                        paths: paths.clone(),
                    }),
                    StoreError::UnsupportedVersion {
                        path,
                        table,
                        key,
                        found,
                        supported,
                    } => Some(StoreDiagnostic::UnsupportedVersion {
                        path: path.clone(),
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
                    StoreError::NotFound { .. } => {
                        result.kind = NotFound;
                        None
                    }
                    StoreError::Conflict { .. } => {
                        result.kind = Conflict;
                        result.conflict = Some(ConflictReason::TurnIdUsed);
                        None
                    }
                    StoreError::SessionCollision { .. } => {
                        result.kind = Conflict;
                        result.conflict = Some(ConflictReason::SessionExists);
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
                })
                .map(Box::new);
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

/// 私有请求不实现正文 Debug；两种身份均在发送之前确定。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TurnRequest {
    pub turn_id: TurnId,
    pub session: SubmitSession,
    pub workspace: PathBuf,
    pub input: String,
    pub agent: Option<String>,
}
impl TurnRequest {
    /// 解析已有会话后再生成身份，sending 始终含完整 SessionId。
    pub fn new(
        session_id: Option<SessionId>,
        workspace: PathBuf,
        input: String,
        agent: Option<String>,
    ) -> Self {
        Self {
            turn_id: super::new_turn_id(),
            session: session_id.map_or_else(
                || SubmitSession::New(super::new_session_id()),
                SubmitSession::Existing,
            ),
            workspace,
            input,
            agent,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubmitSession {
    New(SessionId),
    Existing(SessionId),
}
impl SubmitSession {
    pub fn id(self) -> SessionId {
        match self {
            Self::New(id) | Self::Existing(id) => id,
        }
    }
    pub fn is_new(self) -> bool {
        matches!(self, Self::New(_))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "acceptance", rename_all = "snake_case")]
pub enum Acceptance {
    Accepted {
        turn: TurnView,
    },
    ConfirmationUnknown {
        turn_id: TurnId,
        session_id: SessionId,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub epoch: String,
    pub stopping: bool,
    pub owners: usize,
    pub unconfirmed_operations: usize,
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
    AuthSet {
        provider: String,
        key: super::auth::AuthKey,
    },
    AuthRemove {
        provider: String,
    },
    // 空结构让 serde 拒绝额外字段，避免静默忽略误发给查询的 key。
    AuthStatus {},
    Models {
        provider: Option<String>,
    },
    ConfigurationError {
        message: String,
    },
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
        turn_id: TurnId,
    },
    Head {
        session_id: SessionId,
    },
    History {
        session_id: SessionId,
        before: NodeId,
        turns: u32,
    },
    Rewind {
        session_id: SessionId,
        target: NodeId,
        workspace: PathBuf,
    },
    Submit {
        request: TurnRequest,
        configuration: Box<super::config::FrozenConfiguration>,
    },
    Cancel {
        turn_id: TurnId,
    },
    Watch {
        session_id: SessionId,
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
    AuthUpdated,
    AuthRemoved { removed: bool },
    AuthStatus(AuthStatus),
    Models(ModelCandidates),
    ConfigurationError { message: String },
    Session(SessionRecord),
    Sessions(Page<SessionListRow, SessionId>),
    Log(SessionLog),
    Turn(TurnLookup),
    Head(SessionHead),
    History(LocalHistory),
    Rewind(SessionRecord),
    Acceptance(Acceptance),
    View(ViewUpdate),
    Cancelled { owned: bool },
    Audit(AuditPage),
    AuditTail(AuditTailPage),
    AuditReplay(AuditReplay),
    OperatorStatus(OperatorStatus),
    Error(LocalError),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_shapes_require_configuration_and_preserve_explicit_session_identity() {
        let sid = super::super::new_session_id();
        let tid = super::super::new_turn_id();
        for session in [SubmitSession::New(sid), SubmitSession::Existing(sid)] {
            let request = TurnRequest {
                turn_id: tid,
                session,
                workspace: "/tmp/project".into(),
                input: "original".into(),
                agent: Some("local-test".into()),
            };
            let wire = serde_json::to_value(&request).unwrap();
            assert_eq!(
                wire["session"][if session.is_new() { "new" } else { "existing" }],
                sid.to_string()
            );
            assert!(
                serde_json::from_value::<Request>(
                    serde_json::json!({"operation":"submit","request":wire})
                )
                .is_err()
            );
        }
        assert!(
            matches!(serde_json::from_value::<Request>(serde_json::json!({"operation":"turn","turn_id":tid})).unwrap(), Request::Turn { turn_id } if turn_id == tid)
        );
        assert!(
            matches!(serde_json::from_value::<Request>(serde_json::json!({"operation":"cancel","turn_id":tid})).unwrap(), Request::Cancel { turn_id } if turn_id == tid)
        );
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"operation":"cancel","turn_id":tid,"session_id":sid})
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(Request::Head { session_id: sid }).unwrap(),
            serde_json::json!({"operation":"head", "session_id":sid})
        );
        assert!(
            matches!(serde_json::from_value::<Request>(serde_json::json!({"operation":"head", "session_id":sid})).unwrap(), Request::Head { session_id } if session_id == sid)
        );
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"operation":"head", "session_id":sid, "turn_id":tid})
            )
            .is_err()
        );
        assert_eq!(
            serde_json::to_value(Response::Head(SessionHead {
                session_id: sid,
                position: NodeId(0),
                agent: None,
                turn_id: None,
                owner: false,
                diagnostic: None,
                path_replaced: false,
            }))
            .unwrap(),
            serde_json::json!({"response":"head", "value":{
                "session_id":sid, "position":0, "agent":null, "turn_id":null,
                "owner":false, "diagnostic":null, "path_replaced":false,
            }})
        );
        let error = LocalError::new(LocalErrorKind::Busy, "busy");
        assert!(
            serde_json::to_value(error)
                .unwrap()
                .get("conflict")
                .is_none()
        );
        assert_eq!(
            serde_json::to_value(TurnLookup::InFlight).unwrap(),
            serde_json::json!({"status":"in_flight"})
        );
    }
    #[test]
    fn safe_store_diagnostics_preserve_path_and_never_expose_corruption_detail() {
        use lattice_session::store::StoreError;
        let error = LocalError::from(crate::ApiError::from(StoreError::UnsupportedVersion {
            path: "/tmp/old state/lattice.redb".into(),
            table: "meta".into(),
            key: "format_version".into(),
            found: 1,
            supported: 2,
        }));
        let wire = serde_json::to_value(&error).unwrap();
        assert_eq!(wire["store"]["path"], "/tmp/old state/lattice.redb");
        assert_eq!(serde_json::from_value::<LocalError>(wire).unwrap(), error);
        let error = LocalError::from(crate::ApiError::from(StoreError::LegacyStores {
            paths: vec!["/tmp/state/sessions.git".into()],
        }));
        assert!(
            matches!(error.store.as_deref(), Some(StoreDiagnostic::LegacyStores { paths }) if paths.len() == 1)
        );
        let error = LocalError::from(crate::ApiError::from(StoreError::Corruption {
            table: "nodes".into(),
            key: "1".into(),
            detail: "secret-body".into(),
        }));
        assert!(
            !serde_json::to_string(&error)
                .unwrap()
                .contains("secret-body")
        );
    }
}
