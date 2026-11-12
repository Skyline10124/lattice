// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

pub use crate::models::{ModelCandidateQuery, ModelCandidates};
pub use crate::turn::{TextSegment, TurnRef};
pub use crate::{
    AgentName, AgentNameError, ModelId, ModelIdError, ModelRef, ModelRefError, ProviderId,
    ProviderIdError,
};
pub use lattice_conversation::{
    CostEstimate, CostSummary, Effort, InvocationId, ModelAttemptKey, ResultPayload, SessionId,
    StepContent, StoredToolResult, SuccessContent, TokenSummary, ToolError, TurnId,
    UsageCompleteness,
};
pub use lattice_ipc::{
    Acceptance, AuthRemoval, AuthStatus, Availability, CancelStatus, ConfigurationRequirement,
    ConfigureReply, ConfigureRequest, ConfigureResult, ConfigureSession, ConflictReason,
    ConnectionStatus, DaemonStatus, EffortAdjustment, HISTORY_TURNS, LocalError, LocalErrorKind,
    LocalHistory, LocalUpdate, MAX_HISTORY_TURNS, ProviderAuthStatus, RecoveryCause, RecoveryNeed,
    Result, RevocationStatus, SessionHead, SessionListRow, StoreDiagnostic, SubmissionReceipt,
    SubmissionResult, SubmissionUpdate, TextUpdate, TurnLookup, TurnRequest, TurnView, ViewUpdate,
    WATCH_TURNS,
};
pub(super) use lattice_ipc::{AuthLoggedOut, Request, Response};
pub use lattice_llm::auth::SensitiveLoginUrl;
pub use lattice_session::store::{
    AttemptProgress, ConversationEntry, MaterialSummary, Node, NodeId, NodeKind, NodeRef, Notice,
    NoticeLevel, Page, PublicTurnOutcome, RetryNotice, SessionLog, SessionRecord, SessionUsage,
    SettingsVersion, SlotProgress, SlotReason, SlotState, StepDetail, StepEnd, StepKey,
    StepProgress, TurnDetail, TurnSummary, TurnUsageRow, UsageSummary,
};

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

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigureOutcome {
    Configured(ConfigureReply),
    Rejected(LocalError),
    Unknown(LocalError),
}
