// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 本地 IPC 协议类型、模型与凭据展示类型、分帧编解码与对端 UID 核验。

pub mod auth;
pub mod ipc;
pub mod models;
pub mod types;

#[cfg(test)]
mod wire_shape;

pub use auth::{
    AuthCatalogOutcome, AuthKey, AuthLoginOutcome, AuthStatus, LoginAction, LoginResponse,
    ProviderAuthStatus, RevocationStatus, require_chatgpt,
};
pub use lattice_conversation::{
    BoundToolCall, CostEstimate, CostSummary, Effort, InvocationId, MessageRole, MessageSemantic,
    ModelAttemptKey, ModelId, ModelIdError, ModelRef, ModelRefError, NativeMessage, ProviderId,
    ProviderIdError, ResultPayload, SessionId, StepContent, StoredToolResult, SuccessContent,
    TokenSummary, ToolError, ToolResultOutcome, TurnId, UsageCompleteness,
};
pub use lattice_session::{
    AgentName, AgentNameError, ProjectPath, RetryPolicy, RetryPolicyError, TurnTimeout,
    TurnTimeoutError,
    store::{
        AttemptProgress, ConversationEntry, MaterialSummary, Node, NodeId, NodeKind, NodeRef,
        Notice, NoticeLevel, Page, PublicTurnOutcome, RetryNotice, SessionLog, SessionRecord,
        SessionUsage, SettingsVersion, SlotProgress, SlotReason, SlotState, StepDetail, StepEnd,
        StepKey, StepProgress, TurnDetail, TurnSummary, TurnUsageRow, UsageSummary,
    },
};
pub use models::{
    Availability, ConfigurationRequirement, ModelCandidate, ModelCandidateQuery, ModelCandidates,
    ModelPrice, ModelWarning,
};
pub use types::{
    Acceptance, AuthLoggedOut, AuthRemoval, CancelStatus, ConfigureReply, ConfigureRequest,
    ConfigureResult, ConfigureSession, ConflictReason, ConnectionStatus, DaemonStatus,
    EffortAdjustment, HISTORY_TURNS, LocalError, LocalErrorKind, LocalHistory, LocalUpdate,
    MAX_HISTORY_TURNS, RecoveryCause, RecoveryNeed, Request, RequestConflict, Response, Result,
    SessionHead, SessionListRow, StoreDiagnostic, SubmissionReceipt, SubmissionResult,
    SubmissionUpdate, TextSegment, TextUpdate, TurnLookup, TurnRef, TurnRequest, TurnView,
    ViewUpdate, WATCH_TURNS,
};
