// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::hex_digest;
use lancet_protocol::{
    BindingIdentity, CellEvidenceTriple, CepErrorCode, InvocationBinding, InvocationId,
    ModelAttemptKey, ModelMessageAddress, Retriability, SessionId, ToolCallAddress, TurnId,
};
use lattice_conversation::{
    BoundToolCall, NativeMessage, ProviderOrigin, TokenUsage, ToolCallValidation,
    ToolResultOutcome, UnboundToolCall,
};
use lattice_store::ProjectStoreKey;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use super::config::{
    FrozenTurnConfig, MAX_CATALOG_SNAPSHOT_BYTES, MAX_MODEL_ATTEMPTS, MAX_MODEL_STEPS,
    MAX_USER_INPUT_BYTES, ToolCatalogEntry, ToolCatalogSnapshot, TurnExecutionLimits,
};
pub use lattice_conversation::MAX_TOOL_CONTENT_BYTES;
pub type Timestamp = chrono::DateTime<chrono::Utc>;
pub type Result<T> = std::result::Result<T, StoreError>;
pub const MAX_PROVIDER_ERROR_CHARS: usize = 2048;

macro_rules! redacted_debug { ($($ty:ty),+ $(,)?) => {$ (
    impl std::fmt::Debug for $ty { fn fmt(&self, f:&mut std::fmt::Formatter<'_>)->std::fmt::Result { f.write_str(concat!(stringify!($ty), " { … }")) } }
)+}; }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ChangeSeq(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepKey {
    pub turn_id: TurnId,
    pub ordinal: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRef {
    pub session_id: SessionId,
    pub node: NodeId,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    Root,
    UserMessage {
        parent: NodeId,
        turn_id: TurnId,
        message: NativeMessage<BoundToolCall>,
    },
    Step {
        parent: NodeId,
        turn_id: TurnId,
        step_ordinal: u32,
        content: StepContent,
    },
}
impl Node {
    pub fn parent(&self) -> Option<NodeId> {
        match self {
            Self::Root => None,
            Self::UserMessage { parent, .. } | Self::Step { parent, .. } => Some(*parent),
        }
    }
    pub fn turn_id(&self) -> Option<TurnId> {
        match self {
            Self::Root => None,
            Self::UserMessage { turn_id, .. } | Self::Step { turn_id, .. } => Some(*turn_id),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionRecord {
    pub session_id: SessionId,
    pub project_store_key: ProjectStoreKey,
    pub current: NodeId,
    pub active_turn: Option<TurnId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnRecord {
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub user_node: NodeId,
    pub original_input: String,
    pub project_store_key: ProjectStoreKey,
    pub config: FrozenTurnConfig,
    #[serde(with = "hex_digest")]
    pub config_digest: [u8; 32],
    pub accepted_at: Timestamp,
    pub state: TurnState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum TurnState {
    Running,
    Closed {
        outcome: TurnOutcome,
        notice: Option<Notice>,
        usage: UsageSummary,
        ended_at: Timestamp,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum TurnOutcome {
    Completed,
    Failed { reason: StopCause },
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cause", rename_all = "snake_case", deny_unknown_fields)]
pub enum StopCause {
    ProviderError { retryable: bool },
    Cancelled,
    Deadline,
    OutputLimit,
    InvalidBatch,
    ProcessExit,
    InvocationIndeterminate { invocation_id: InvocationId },
    ModelStepLimit,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Error,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notice {
    pub level: NoticeLevel,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicTurnOutcome {
    Completed,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TurnSummary {
    pub outcome: PublicTurnOutcome,
    pub notice: Option<Notice>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsageSummary {
    Complete {
        total: TokenUsage,
    },
    Partial {
        known: TokenUsage,
        unknown_attempts: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepRecord {
    pub key: StepKey,
    pub attempts: Vec<AttemptRecord>,
    pub state: StepState,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepState {
    Active {
        active: ActiveStep,
    },
    Closed {
        end: StepEnd,
        invocations: Vec<ClosedInvocation>,
    },
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActiveStep {
    Requesting {
        attempt: u32,
    },
    RetryReady {
        next_attempt: u32,
    },
    Executing {
        reply: NativeMessage<BoundToolCall>,
        slots: Vec<InvocationSlot>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptRecord {
    pub attempt: u32,
    pub origin: ProviderOrigin,
    pub usage: Option<TokenUsage>,
    pub end: AttemptEnd,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "end", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttemptEnd {
    Completed {
        step: ModelMessageAddress,
    },
    Stopped {
        cause: StopCause,
        diagnostic: AttemptDiagnostic,
    },
}
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptDiagnostic {
    pub provider_error: Option<String>,
    pub partial: Vec<DiagnosticPart>,
    pub invalid_batch: Option<ToolCallValidation>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosticPart {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        provider_call_id: Option<String>,
        name: String,
        arguments: String,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "end", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepEnd {
    Continue {
        node: NodeId,
    },
    Finish {
        node: NodeId,
    },
    Stop {
        node: Option<NodeId>,
        cause: StopCause,
    },
}
impl StepEnd {
    pub fn node(&self) -> Option<NodeId> {
        match self {
            Self::Continue { node } | Self::Finish { node } => Some(*node),
            Self::Stop { node, .. } => *node,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationSlot {
    pub binding: InvocationBinding,
    pub state: SlotState,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "terminal",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SlotState {
    Registered,
    Started,
    Terminal(InvocationTerminal),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClosedInvocation {
    pub binding: InvocationBinding,
    pub terminal: InvocationTerminal,
}
/// 真实 receipt 只能经 `prepare_terminal` 构造，调用方不能伪造摘要或缺失证据。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationTerminal {
    pub(super) result: StoredToolResult,
    pub(super) evidence: InvocationEvidence,
}
impl InvocationTerminal {
    pub fn result(&self) -> &StoredToolResult {
        &self.result
    }
    pub fn evidence(&self) -> &InvocationEvidence {
        &self.evidence
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum InvocationEvidence {
    Recorded(Box<CellEvidenceTriple>),
    Unavailable { reason: EvidenceUnavailableReason },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceUnavailableReason {
    /// 调用从未交给 LANCET（NotExecuted）。
    NotDispatched,
    /// 已 Started 但未取得结果（Indeterminate NoResult）。
    NoResult,
}
pub use lattice_conversation::{
    IndeterminateCause, ResultPayload, StepContent, StoredToolResult, SuccessContent,
};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationIndexEntry {
    pub address: ToolCallAddress,
    pub last_change: ChangeSeq,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationChange {
    pub sequence: ChangeSeq,
    pub project_store_key: ProjectStoreKey,
    pub row: OperatorInvocation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcceptSession {
    New { id: SessionId },
    Existing { id: SessionId },
}
impl AcceptSession {
    pub fn id(&self) -> SessionId {
        match self {
            Self::New { id } | Self::Existing { id } => *id,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSelection {
    Explicit,
    Inherited,
}
#[derive(Clone)]
pub struct AcceptTurn {
    pub session: AcceptSession,
    pub turn_id: TurnId,
    pub project_store_key: ProjectStoreKey,
    pub input: String,
    pub config: FrozenTurnConfig,
    pub accepted_at: Timestamp,
    pub agent_selection: AgentSelection,
}
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptedTurn {
    Accepted(TurnRecord),
    Existing(TurnRecord),
}
#[derive(Debug, Clone)]
pub struct AttemptStopped {
    pub key: ModelAttemptKey,
    pub origin: ProviderOrigin,
    pub usage: Option<TokenUsage>,
    pub cause: StopCause,
    pub diagnostic: AttemptDiagnostic,
}
#[derive(Clone)]
pub struct CompleteReply {
    pub key: ModelAttemptKey,
    pub origin: ProviderOrigin,
    pub usage: Option<TokenUsage>,
    pub message: NativeMessage<UnboundToolCall>,
}
#[derive(Clone)]
pub struct RegisteredCalls {
    pub key: StepKey,
    pub reply: NativeMessage<BoundToolCall>,
    pub bindings: Vec<InvocationBinding>,
}
/// 仅 `start_invocation` 提交成功后取得；不能复制、反序列化或自行构造。
///
/// ```compile_fail
/// fn duplicate(started: lattice_session::store::StartedInvocation) {
///     let _copy = started.clone();
/// }
/// ```
///
/// ```compile_fail
/// let _: lattice_session::store::StartedInvocation = serde_json::from_str("{}").unwrap();
/// ```
///
/// ```compile_fail
/// fn forge(binding: lancet_protocol::InvocationBinding) {
///     let _ = lattice_session::store::StartedInvocation { binding };
/// }
/// ```
#[derive(Debug)]
pub struct StartedInvocation {
    pub(super) binding: InvocationBinding,
}
impl StartedInvocation {
    pub fn binding(&self) -> &InvocationBinding {
        &self.binding
    }
}
#[derive(Debug)]
pub struct TerminalInput {
    pub invocation_id: InvocationId,
    pub terminal: InvocationTerminal,
}
/// 协调层在两次分发之间停止执行的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchStop {
    Cancelled,
    Deadline,
    ProcessExit,
}
impl From<DispatchStop> for StopCause {
    fn from(stop: DispatchStop) -> Self {
        match stop {
            DispatchStop::Cancelled => Self::Cancelled,
            DispatchStop::Deadline => Self::Deadline,
            DispatchStop::ProcessExit => Self::ProcessExit,
        }
    }
}
#[derive(Debug)]
pub enum CloseStep {
    Finish(CompleteReply),
    Continue { key: StepKey },
    StopExecuting { key: StepKey, stop: DispatchStop },
    StopBetweenRequests { turn_id: TurnId, cause: StopCause },
}
#[derive(Debug, Clone, PartialEq)]
pub struct ClosedStep {
    pub record: StepRecord,
    pub node: Option<NodeRef>,
    pub summary: Option<TurnSummary>,
}
#[derive(Debug)]
pub enum AttemptStopResult {
    RetryReady { next_attempt: u32 },
    Closed(ClosedStep),
}
#[derive(Debug, Clone, PartialEq)]
pub struct DurableTurnState {
    pub session: SessionRecord,
    pub turn: TurnRecord,
    pub steps: Vec<StepRecord>,
    pub next_node: NodeId,
}
#[derive(Debug, Clone, PartialEq)]
pub enum AutoClosePlan {
    AlreadyClosed { summary: TurnSummary },
    Close(Box<ClosureWrites>),
}
/// 纯规划结果仅供检查；发布时由库在同一写事务重新读取并规划。
#[derive(Debug, Clone, PartialEq)]
pub struct ClosureWrites {
    pub(super) closed_step: StepRecord,
    pub(super) node: Option<(NodeId, Node)>,
    pub(super) turn_state: TurnState,
    pub(super) session_current: NodeId,
    pub(super) session_updated_at: Timestamp,
    pub(super) changed_invocations: Vec<ClosedInvocation>,
}
#[derive(Debug)]
pub enum AutoCloseResult {
    AlreadyClosed(TurnSummary),
    Closed(ClosedStep),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListRow {
    pub session_id: SessionId,
    pub project_store_key: ProjectStoreKey,
    pub current: NodeId,
    pub node_count: u64,
    pub active_turn: Option<TurnId>,
    pub updated_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeHeader {
    pub id: NodeId,
    pub parent: Option<NodeId>,
    pub turn_id: Option<TurnId>,
    pub kind: NodeKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeKind {
    Root,
    UserMessage,
    Step { ordinal: u32 },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRow {
    pub node: NodeHeader,
    pub current: bool,
    pub turn_start: Option<TurnLogSummary>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnLogSummary {
    pub turn_id: TurnId,
    pub summary: Option<TurnSummary>,
    pub agent_name: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLog {
    pub session: SessionRecord,
    pub current_agent: Option<String>,
    pub nodes: Vec<LogRow>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnDetail {
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub user_node: NodeId,
    pub agent_name: String,
    pub accepted_at: Timestamp,
    pub ended_at: Option<Timestamp>,
    pub summary: Option<TurnSummary>,
    pub usage: UsageSummary,
    pub steps: Vec<StepDetail>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepDetail {
    pub key: StepKey,
    pub attempts: Vec<AttemptDetail>,
    pub state: StepDetailState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptDetail {
    pub attempt: u32,
    pub origin: ProviderOrigin,
    pub usage: Option<TokenUsage>,
    pub end: AttemptEndDetail,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "end", rename_all = "snake_case")]
pub enum AttemptEndDetail {
    Completed {
        step: ModelMessageAddress,
    },
    Stopped {
        cause: StopCause,
        diagnostic: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum StepDetailState {
    Requesting {
        attempt: u32,
    },
    RetryReady {
        next_attempt: u32,
    },
    Executing {
        slots: Vec<OperatorInvocation>,
    },
    Closed {
        end: StepEnd,
        slots: Vec<OperatorInvocation>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryAnchor {
    pub session_id: SessionId,
    pub head: NodeId,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCursor {
    pub anchor: HistoryAnchor,
    pub node: NodeId,
    pub utf8_offset: u64,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeFragment {
    pub node: NodeId,
    pub utf8_offset: u64,
    pub json_fragment: String,
    pub complete: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub anchor: HistoryAnchor,
    pub fragments: Vec<NodeFragment>,
    pub next: Option<HistoryCursor>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct DurableSnapshot {
    pub session: SessionRecord,
    pub current_agent: Option<String>,
    pub selected_turn: Option<TurnDetail>,
    pub history: HistoryAnchor,
    pub active: Option<ActiveStepSnapshot>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveStepSnapshot {
    pub key: StepKey,
    pub attempts: Vec<AttemptDetail>,
    pub state: ActiveStep,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T, C> {
    pub items: Vec<T>,
    pub next: Option<C>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditFilter {
    pub project_store_key: ProjectStoreKey,
    pub session_id: Option<SessionId>,
    pub turn_id: Option<TurnId>,
    pub method: Option<String>,
    pub outcome: Option<ToolResultOutcome>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorInvocation {
    pub invocation_id: InvocationId,
    pub binding: BindingIdentity,
    pub state: OperatorInvocationState,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperatorInvocationState {
    Registered,
    Started,
    Terminal {
        outcome: ToolResultOutcome,
        reason: Option<OperatorReason>,
        material: MaterialSummary,
        evidence: InvocationEvidence,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum OperatorReason {
    Failed {
        code: CepErrorCode,
        retriability: Retriability,
    },
    Indeterminate(IndeterminateCause),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MaterialSummary {
    Available {
        #[serde(with = "hex_digest")]
        digest: [u8; 32],
        byte_length: u64,
    },
    OutputTooLarge {
        #[serde(with = "hex_digest")]
        digest: [u8; 32],
        byte_length: u64,
    },
    NoContent,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditPage {
    pub latest_change: ChangeSeq,
    pub rows: Vec<OperatorInvocation>,
    pub next: Option<InvocationId>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditTailPage {
    pub latest_change: ChangeSeq,
    pub rows: Vec<InvocationChange>,
    pub next_after: ChangeSeq,
    pub done: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReplay {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub latest_change: ChangeSeq,
    pub calls: Vec<OperatorInvocation>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperatorStatus {
    pub latest_change: ChangeSeq,
    pub recent: Vec<OperatorInvocation>,
}

redacted_debug!(
    Node,
    TurnRecord,
    Notice,
    ActiveStep,
    AttemptDiagnostic,
    DiagnosticPart,
    InvocationTerminal,
    AcceptTurn,
    CompleteReply,
    RegisteredCalls,
    AttemptEndDetail,
    NodeFragment
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitFailureKind {
    TransactionPoisoned,
    OutcomeUnknown,
}
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("session store not initialized: {path}")]
    NotInitialized { path: PathBuf },
    #[error("legacy stores must be archived or removed by the operator: {paths:?}")]
    LegacyStores { paths: Vec<PathBuf> },
    #[error("unsupported version in {table}/{key}: {found}, supported {supported}")]
    UnsupportedVersion {
        table: String,
        key: String,
        found: u32,
        supported: u32,
    },
    #[error("session store corruption in {table}/{key}: {detail}")]
    Corruption {
        table: String,
        key: String,
        detail: String,
    },
    #[error("invalid input {field}: {detail}")]
    InvalidInput { field: &'static str, detail: String },
    #[error("{kind} not found: {id}")]
    NotFound { kind: &'static str, id: String },
    #[error("request identity conflict: {turn_id}")]
    Conflict { turn_id: TurnId },
    #[error("session busy: {session_id}")]
    Busy { session_id: SessionId },
    #[error("turn requires closure: {turn_id}")]
    RecoveryRequired { turn_id: TurnId },
    #[error("invalid session execution lock capability")]
    InvalidLockCapability,
    #[error("invalid transition for {turn_id}: {detail}")]
    InvalidTransition { turn_id: TurnId, detail: String },
    #[error("invocation ids already occupied: {ids:?}")]
    InvocationCollision { ids: Vec<InvocationId> },
    #[error("session already exists: {session_id}")]
    SessionCollision { session_id: SessionId },
    #[error("sequence exhausted: {kind}")]
    SequenceExhausted { kind: &'static str },
    #[error("invalid read cursor")]
    InvalidCursor,
    #[error("writes disabled; close and reopen the database")]
    WriteDisabled,
    #[error("commit failed ({kind:?}); close and reopen the database: {source}")]
    CommitFailed {
        kind: CommitFailureKind,
        #[source]
        source: redb::CommitError,
    },
    #[error("created target durability unknown: {path}: {source}")]
    CreationDurabilityUnknown {
        path: PathBuf,
        #[source]
        source: lattice_store::StoreError,
    },
    #[error("database: {0}")]
    Database(#[from] redb::DatabaseError),
    #[error("transaction: {0}")]
    Transaction(#[from] redb::TransactionError),
    #[error("storage: {0}")]
    Storage(#[from] redb::StorageError),
    #[error("durability configuration: {0}")]
    Durability(#[from] redb::SetDurabilityError),
    #[error("{operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("storage kernel: {0}")]
    Kernel(#[from] lattice_store::StoreError),
}
