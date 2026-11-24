// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! 本地 IPC 共享请求、响应、视图与错误类型。

use std::path::PathBuf;

use lattice_conversation::{Effort, ImageRef, ModelId, ModelRef, ProviderId, SessionId, TurnId};
use lattice_session::{
    AgentName,
    store::{
        ConversationEntry, NodeId, Page, SessionLog, SessionRecord, SessionUsage, SettingsVersion,
        TurnDetail, TurnSummary, UsageSummary,
    },
};
use serde::{Deserialize, Serialize};

use crate::{
    auth::{AuthKey, AuthStatus, RevocationStatus},
    models::{ModelCandidateQuery, ModelCandidates},
};

pub const WATCH_TURNS: u32 = 10;
pub const HISTORY_TURNS: u32 = 10;
pub const MAX_HISTORY_TURNS: u32 = 100;

/// 客户端对立即可纠正的请求冲突给出明确提示，不与 Busy 或存储错误混同。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestConflict {
    TurnIdUsed,
    SessionExists,
    BindingMismatch,
    SettingsChanged,
}

impl std::fmt::Display for RequestConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TurnIdUsed => "TurnId is already in use",
            Self::SessionExists => "session already exists",
            Self::BindingMismatch => "session is bound to another workspace",
            Self::SettingsChanged => "session settings changed",
        })
    }
}

pub use RequestConflict as ConflictReason;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnRef {
    pub session_id: SessionId,
    pub turn_id: TurnId,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TextSegment {
    Text { text: String },
    Reasoning { text: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthRemoval {
    pub removed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation: Option<RevocationStatus>,
}

/// Response 的结构体变体不受严格解析约束，载荷单独成结构体才能拒绝未知字段。
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthLoggedOut {
    pub revocation: RevocationStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TurnView {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub start_node: NodeId,
    pub input_nodes: Vec<NodeId>,
    pub agent_name: String,
    pub accepted_at: chrono::DateTime<chrono::Utc>,
    pub ended_at: chrono::DateTime<chrono::Utc>,
    pub summary: TurnSummary,
    pub usage: Box<UsageSummary>,
}

impl From<&TurnDetail> for TurnView {
    fn from(turn: &TurnDetail) -> Self {
        Self {
            session_id: turn.session_id,
            turn_id: turn.turn_id,
            start_node: turn.start_node,
            input_nodes: turn.input_nodes.clone(),
            agent_name: turn.agent_name.as_str().to_owned(),
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
    pub agent: String,
    pub model: Option<ModelId>,
    pub provider: Option<ProviderId>,
    pub effort: Option<Effort>,
    pub settings_version: SettingsVersion,
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
    },
    Receipt(SubmissionReceipt),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ConfigureResult {
    Configured {
        head: SessionHead,
        adjustment: Option<EffortAdjustment>,
    },
    Rejected {
        error: LocalError,
    },
    Unknown {
        error: Option<LocalError>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalUpdate {
    Submit {
        #[serde(flatten)]
        update: SubmissionUpdate,
    },
    Configure {
        session_id: SessionId,
        #[serde(flatten)]
        result: ConfigureResult,
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
    /// 已有后台的 IPC 魔数版本与本端不同（更旧或更新）。客户端不替换它，也不兼容其他版本的协议。
    IncompatibleBackend,
}

pub type Result<T> = std::result::Result<T, LocalError>;

impl LocalError {
    pub fn new(kind: LocalErrorKind, message: &str) -> Self {
        Self {
            kind,
            message: message.into(),
            conflict: None,
            recovery: None,
            store: None,
        }
    }

    pub fn recovery(cause: RecoveryCause) -> Self {
        Self {
            recovery: Some(RecoveryNeed { turn: None, cause }),
            ..Self::new(
                LocalErrorKind::StoreUnavailable,
                "session store recovery required",
            )
        }
    }
}

impl From<std::io::Error> for LocalError {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                Self::new(LocalErrorKind::NotRunning, "本地连接目标不存在或拒绝连接")
            }
            std::io::ErrorKind::PermissionDenied => Self::new(
                LocalErrorKind::Permission,
                "本地连接权限不足；回执状态可能未知",
            ),
            _ => Self::new(LocalErrorKind::Transport, "本地连接失败；回执状态可能未知"),
        }
    }
}

mod path_bytes {
    use std::{
        ffi::OsString,
        os::unix::ffi::{OsStrExt, OsStringExt},
        path::{Path, PathBuf},
    };

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        path.as_os_str().as_bytes().serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PathBuf, D::Error> {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        Ok(PathBuf::from(OsString::from_vec(bytes)))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfigureSession {
    New(SessionId),
    Existing(SessionId),
}

impl ConfigureSession {
    pub fn id(self) -> SessionId {
        match self {
            Self::New(id) | Self::Existing(id) => id,
        }
    }

    #[cfg(test)]
    fn is_new(self) -> bool {
        matches!(self, Self::New(_))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigureRequest {
    pub session: ConfigureSession,
    #[serde(with = "path_bytes")]
    pub workspace: PathBuf,
    pub agent: Option<AgentName>,
    pub model: Option<ModelRef>,
    pub effort: Option<Effort>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EffortAdjustment {
    pub from: Option<Effort>,
    pub to: Option<Effort>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfigureReply {
    pub head: SessionHead,
    pub adjustment: Option<EffortAdjustment>,
}

mod image_field {
    use base64::{Engine, display::Base64Display, engine::general_purpose::STANDARD};
    use lattice_conversation::ImageRef;
    use serde::{Deserializer, Serializer};

    pub trait Encode {
        fn encode_image<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error>;
    }

    impl Encode for Vec<u8> {
        fn encode_image<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_str(&Base64Display::new(self, &STANDARD))
        }
    }

    impl Encode for ImageRef {
        fn encode_image<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_str("")
        }
    }

    pub trait Decode: Sized {
        fn decode_image<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error>;
    }

    struct Base64Visitor;

    impl serde::de::Visitor<'_> for Base64Visitor {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("标准 base64 编码的图片字符串")
        }

        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            STANDARD
                .decode(value)
                .map_err(|_| E::custom("图片字段不是合法的标准 base64 字符串"))
        }
    }

    impl Decode for Vec<u8> {
        fn decode_image<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_str(Base64Visitor)
        }
    }

    pub(super) fn serialize<I: Encode, S: Serializer>(
        image: &I,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        image.encode_image(serializer)
    }

    pub(super) fn deserialize<'de, I: Decode, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<I, D::Error> {
        I::decode_image(deserializer)
    }
}

/// 私有请求片段不实现正文 Debug；图片在线上按标准 base64 编码。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    deny_unknown_fields,
    bound(
        serialize = "I: image_field::Encode",
        deserialize = "I: image_field::Decode",
    )
)]
pub enum UserInputPart<I = Vec<u8>> {
    Text {
        text: String,
    },
    Image {
        #[serde(with = "image_field")]
        image: I,
    },
}

/// 私有请求不实现正文 Debug；两种身份均在发送之前确定。
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    deny_unknown_fields,
    bound(
        serialize = "UserInputPart<I>: Serialize",
        deserialize = "UserInputPart<I>: Deserialize<'de>",
    )
)]
pub struct TurnRequest<I = Vec<u8>> {
    pub turn_id: TurnId,
    pub session: SessionId,
    #[serde(with = "path_bytes")]
    pub workspace: PathBuf,
    pub parts: Vec<UserInputPart<I>>,
    pub expected_settings: Option<SettingsVersion>,
}

/// 读取图片字节之前的待发请求，图片段只持 `ImageRef`，只用于 `ipc::encoded_submit_len` 计长。
pub type PlannedTurnRequest = TurnRequest<ImageRef>;

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
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImMountResult {
    Mounted,
    UsingPreviousConfiguration { reason: String },
    Unmounted { reason: String },
    Disabled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImAuthSetReply {
    pub mount: ImMountResult,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImAuthRemoveReply {
    pub removed: bool,
    pub mount: ImMountResult,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImCredentialStatus {
    Present,
    Missing,
    Invalid { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImPlatformAuthStatus {
    pub platform: String,
    pub credential: ImCredentialStatus,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ImAuthStatus {
    pub platforms: Vec<ImPlatformAuthStatus>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComponentState {
    Mounted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        configuration_error: Option<String>,
    },
    Unmounted {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComponentStatus {
    pub name: String,
    pub state: ComponentState,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonStatus {
    pub epoch: String,
    pub stopping: bool,
    pub owners: usize,
    pub unconfirmed_operations: usize,
    pub components: Vec<ComponentStatus>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Status,
    Stop,
    AuthSet {
        provider: String,
        key: AuthKey,
    },
    AuthRemove {
        provider: String,
    },
    AuthLogin {
        provider: String,
    },
    AuthLogout {
        provider: String,
    },
    // 空结构让 serde 拒绝额外字段，避免静默忽略误发给查询的 key。
    AuthStatus {},
    ImAuthSet {
        platform: String,
        token: AuthKey,
    },
    ImAuthRemove {
        platform: String,
    },
    ImAuthStatus {},
    ImReload {},
    Models {
        query: ModelCandidateQuery,
    },
    ModelsRefresh {},
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
    Usage {
        session_id: String,
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
        #[serde(with = "path_bytes")]
        workspace: PathBuf,
    },
    Configure {
        request: ConfigureRequest,
    },
    Submit {
        request: TurnRequest,
    },
    Cancel {
        turn_id: TurnId,
    },
    Watch {
        session_id: SessionId,
    },
}

/// 与 `Request::Submit` 同构的计长包装；与 `Request` 的一致性由计长精确性测试守卫。
#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub(crate) enum PlannedSubmit<'a> {
    Submit { request: &'a PlannedTurnRequest },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "response", content = "value", rename_all = "snake_case")]
pub enum Response {
    Status(DaemonStatus),
    AuthUpdated,
    AuthRemoved(AuthRemoval),
    AuthLoggedOut(AuthLoggedOut),
    AuthStatus(AuthStatus),
    ImAuthSet(ImAuthSetReply),
    ImAuthRemove(ImAuthRemoveReply),
    ImAuthStatus(ImAuthStatus),
    ImReload(ImMountResult),
    Models(ModelCandidates),
    ModelsRefreshed,
    Session(SessionRecord),
    Sessions(Page<SessionListRow, SessionId>),
    Log(SessionLog),
    Usage(Box<SessionUsage>),
    Turn(TurnLookup),
    Head(SessionHead),
    History(LocalHistory),
    Rewind(SessionRecord),
    Configure(ConfigureReply),
    Acceptance(Acceptance),
    View(ViewUpdate),
    Cancelled { owned: bool },
    Error(LocalError),
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    use super::*;

    #[test]
    fn request_shapes_preserve_configure_and_submit_identities() {
        let sid = SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap();
        let tid = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap();
        let raw_workspace = b"/tmp/proj-\xff\xfe/work";
        let non_utf8_ws = PathBuf::from(OsStr::from_bytes(raw_workspace));
        let version: SettingsVersion =
            serde_json::from_value(serde_json::json!("ab".repeat(32))).unwrap();
        for (session, agent, model, effort) in [
            (
                ConfigureSession::New(sid),
                Some(AgentName::parse("local-test").unwrap()),
                None,
                None,
            ),
            (
                ConfigureSession::Existing(sid),
                None,
                Some(ModelRef::parse("local/org/model").unwrap()),
                Some(Effort::High),
            ),
        ] {
            let configure_req = ConfigureRequest {
                session,
                workspace: non_utf8_ws.clone(),
                agent,
                model,
                effort,
            };
            let wire = serde_json::to_value(&configure_req).unwrap();
            assert_eq!(
                wire["workspace"],
                serde_json::to_value(raw_workspace.as_slice()).unwrap(),
                "configure 请求的 workspace 线上表示应为原始字节数组"
            );
            assert_eq!(
                wire["session"][if session.is_new() { "new" } else { "existing" }],
                sid.to_string()
            );
            let wrapped = serde_json::json!({"operation":"configure","request":wire});
            assert!(
                matches!(serde_json::from_value::<Request>(wrapped.clone()).unwrap(), Request::Configure { request: decoded } if decoded == configure_req)
            );
            assert_eq!(
                serde_json::to_value(Request::Configure {
                    request: configure_req
                })
                .unwrap(),
                wrapped
            );
            let mut extra = wrapped;
            extra["request"]["extra_field"] = serde_json::json!(true);
            assert!(
                serde_json::from_value::<Request>(extra).is_err(),
                "ConfigureRequest 必须拒绝未知字段"
            );
        }
        for expected_settings in [None, Some(version)] {
            let png_bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            let request = TurnRequest {
                turn_id: tid,
                session: sid,
                workspace: non_utf8_ws.clone(),
                parts: vec![
                    UserInputPart::Text {
                        text: "前文".into(),
                    },
                    UserInputPart::Image { image: png_bytes },
                    UserInputPart::Text {
                        text: "后文".into(),
                    },
                ],
                expected_settings,
            };
            let wire = serde_json::to_value(&request).unwrap();
            assert_eq!(
                wire["workspace"],
                serde_json::to_value(raw_workspace.as_slice()).unwrap(),
                "submit 请求的 workspace 线上表示应为原始字节数组"
            );
            assert_eq!(wire["session"], sid.to_string());
            assert_eq!(
                wire["parts"][1],
                serde_json::json!({"kind": "image", "image": "iVBORw0KGgo="}),
                "图片片段线上字段应为标准 base64 字符串"
            );
            let submit = serde_json::json!({"operation":"submit","request":wire});
            assert!(
                matches!(serde_json::from_value::<Request>(submit.clone()).unwrap(), Request::Submit { request: decoded } if decoded == request),
                "submit 请求往返解码后应保持一致"
            );
            assert_eq!(
                serde_json::to_value(Request::Submit { request }).unwrap(),
                submit
            );
            let mut bad_base64 = submit.clone();
            bad_base64["request"]["parts"][1]["image"] = serde_json::json!("not-valid-base64!");
            assert!(
                serde_json::from_value::<Request>(bad_base64).is_err(),
                "非法 base64 图片字段必须被拒绝"
            );
            let mut extra_part = submit.clone();
            extra_part["request"]["parts"][0]["extra"] = serde_json::json!(1);
            assert!(
                serde_json::from_value::<Request>(extra_part).is_err(),
                "UserInputPart 必须拒绝未知字段"
            );
            let mut extra = submit;
            extra["configuration"] = serde_json::json!({"profile_toml":"untrusted"});
            assert!(serde_json::from_value::<Request>(extra).is_err());
        }
        let rewind_wire = serde_json::to_value(Request::Rewind {
            session_id: sid,
            target: NodeId(0),
            workspace: non_utf8_ws.clone(),
        })
        .unwrap();
        assert_eq!(
            rewind_wire["workspace"],
            serde_json::to_value(raw_workspace.as_slice()).unwrap(),
            "rewind 请求的 workspace 线上表示应为原始字节数组"
        );
        assert!(matches!(
            serde_json::from_value::<Request>(rewind_wire).unwrap(),
            Request::Rewind { session_id, target: NodeId(0), workspace }
                if session_id == sid && workspace == non_utf8_ws
        ));
        for (query, expected_wire) in [
            (
                ModelCandidateQuery::List {
                    provider: Some(ProviderId::parse("local").unwrap()),
                    ready_only: true,
                },
                serde_json::json!({"operation":"models","query":{"list":{"provider":"local","ready_only":true}}}),
            ),
            (
                ModelCandidateQuery::Target(ModelRef::parse("local/org/model").unwrap()),
                serde_json::json!({"operation":"models","query":{"target":"local/org/model"}}),
            ),
        ] {
            assert_eq!(
                serde_json::to_value(Request::Models {
                    query: query.clone()
                })
                .unwrap(),
                expected_wire
            );
            assert!(
                matches!(serde_json::from_value::<Request>(expected_wire).unwrap(), Request::Models { query: decoded } if decoded == query)
            );
        }
        assert!(
            serde_json::from_value::<Request>(
                serde_json::json!({"operation":"models","provider":"local"})
            )
            .is_err()
        );
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
        let head = SessionHead {
            model: Some(ModelId::parse("test").unwrap()),
            provider: Some(ProviderId::parse("openai-compatible").unwrap()),
            effort: None,
            settings_version: version,
            session_id: sid,
            position: NodeId(0),
            agent: "local-test".into(),
            turn_id: None,
            owner: false,
            diagnostic: None,
            path_replaced: false,
        };
        assert_eq!(
            serde_json::to_value(Response::Head(head.clone())).unwrap(),
            serde_json::json!({"response":"head", "value":{
                "session_id":sid, "position":0, "agent":"local-test", "turn_id":null,
                "model":"test", "provider":"openai-compatible", "effort":null,
                "settings_version":version,
                "owner":false, "diagnostic":null, "path_replaced":false,
            }})
        );
        assert_eq!(
            serde_json::to_value(Response::Configure(ConfigureReply {
                head,
                adjustment: Some(EffortAdjustment {
                    from: Some(Effort::High),
                    to: None,
                }),
            }))
            .unwrap()["value"]["adjustment"],
            serde_json::json!({"from":"high","to":null}),
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
    fn request_conflict_display_messages() {
        for (reason, text) in [
            (RequestConflict::TurnIdUsed, "TurnId is already in use"),
            (RequestConflict::SessionExists, "session already exists"),
            (
                RequestConflict::BindingMismatch,
                "session is bound to another workspace",
            ),
            (RequestConflict::SettingsChanged, "session settings changed"),
        ] {
            assert_eq!(reason.to_string(), text);
        }
    }

    #[test]
    fn im_and_component_wire_types_reject_unknown_fields() {
        for bad_request in [
            serde_json::json!({"operation":"im_auth_set","platform":"telegram","token":"1:abc","extra":1}),
            serde_json::json!({"operation":"im_auth_remove","platform":"telegram","extra":1}),
            serde_json::json!({"operation":"im_auth_status","extra":1}),
            serde_json::json!({"operation":"im_reload","extra":1}),
        ] {
            assert!(
                serde_json::from_value::<Request>(bad_request.clone()).is_err(),
                "IM 请求必须拒绝未知字段：{bad_request}"
            );
        }
        assert!(
            serde_json::from_value::<ImMountResult>(
                serde_json::json!({"status":"unmounted","reason":"x","extra":1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ImMountResult>(
                serde_json::json!({"status":"using_previous_configuration","reason":"x","extra":1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ImAuthSetReply>(
                serde_json::json!({"mount":{"status":"mounted"},"extra":1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ImAuthRemoveReply>(
                serde_json::json!({"removed":true,"mount":{"status":"disabled"},"extra":1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ImCredentialStatus>(
                serde_json::json!({"status":"invalid","reason":"x","extra":1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ImPlatformAuthStatus>(
                serde_json::json!({"platform":"telegram","credential":{"status":"missing"},"extra":1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ImAuthStatus>(serde_json::json!({"platforms":[],"extra":1}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<ComponentState>(
                serde_json::json!({"status":"mounted","extra":1})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<ComponentStatus>(
                serde_json::json!({"name":"im","state":{"status":"mounted"},"extra":1})
            )
            .is_err()
        );
    }
}
