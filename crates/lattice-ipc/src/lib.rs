// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 本地 IPC 协议类型、模型与凭据展示类型、分帧编解码、对端核验与排空时长解析。

mod auth;
mod drain_timeout;
pub mod ipc;
mod models;
mod types;

#[cfg(test)]
mod wire_shape;

pub use auth::{
    AuthCatalogOutcome, AuthKey, AuthLoginOutcome, AuthStatus, LoginAction, LoginResponse,
    ProviderAuthStatus, RevocationStatus, require_chatgpt,
};
pub use drain_timeout::{DrainTimeout, parse_drain_timeout};
pub use lattice_context::{
    Channel, ChannelError, ContextEdit, ContextError, ContextInput, ContextInspection, ContextOp,
    ContextText, EffectiveContext, EffectiveEntrySummary, NodeStatus, NormalizedUserMessage,
    OperatedNode, PathNodeKind, ToolResultText,
};
pub use lattice_conversation::{
    BoundToolCall, CostEstimate, CostSummary, Effort, ImageMediaType, ImageRef, InputModality,
    InvocationId, MAX_IMAGE_BYTES, MAX_USER_IMAGE_TOTAL_BYTES, MAX_USER_IMAGES_PER_TURN,
    MAX_USER_TEXT_BYTES, MessageRole, MessageSemantic, ModelAttemptKey, ModelId, ModelIdError,
    ModelRef, ModelRefError, NativeMessage, ProviderId, ProviderIdError, ResultPayload, SessionId,
    StepContent, StoredToolResult, SuccessContent, TokenSummary, ToolError, ToolResultOutcome,
    TurnId, UsageCompleteness,
};
pub use lattice_session::{
    AgentName, AgentNameError, ProjectPath, RetryPolicy, RetryPolicyError, TurnTimeout,
    TurnTimeoutError,
    store::{
        AttemptDetail, AttemptEndDetail, AttemptProgressDetail, ConversationEntry, MaterialSummary,
        NodeId, NodeKind, NodeRef, Notice, NoticeLevel, Page, PublicTurnOutcome, RetryNotice,
        SessionLog, SessionRecord, SessionUsage, SettingsVersion, SlotDetail, SlotReason,
        SlotState, StepDetail, StepEnd, StepKey, StepProgress, TurnDetail, TurnSummary,
        TurnUsageRow, UsageSummary,
    },
};
pub use models::{
    Availability, ConfigurationRequirement, ModelCandidate, ModelCandidateQuery, ModelCandidates,
    ModelPrice, ModelWarning,
};
pub use types::{
    Acceptance, AuthLoggedOut, AuthRemoval, CancelStatus, ComponentState, ComponentStatus,
    ConfigureReply, ConfigureRequest, ConfigureResult, ConfigureSession, ConflictReason,
    ConnectionStatus, ContextImage, ContextOpReply, ContextOpRequest, DaemonStatus,
    EffortAdjustment, FeedReply, FeedRequest, HISTORY_TURNS, ImAuthRemoveReply, ImAuthSetReply,
    ImAuthStatus, ImCredentialStatus, ImMountResult, ImPlatformAuthStatus, InspectReply,
    LocalError, LocalErrorKind, LocalHistory, LocalUpdate, MAX_HISTORY_TURNS,
    PlannedContextOpRequest, PlannedFeedRequest, PlannedTurnRequest, RecoveryCause, RecoveryNeed,
    Request, RequestConflict, Response, Result, SessionHead, SessionListRow, StoreDiagnostic,
    SubmissionReceipt, SubmissionResult, SubmissionUpdate, TextSegment, TextUpdate, TriggerRequest,
    TurnInputItem, TurnLookup, TurnRef, TurnRequest, TurnView, UserInputPart, ViewUpdate,
    WATCH_TURNS,
};
