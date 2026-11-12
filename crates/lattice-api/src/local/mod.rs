// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Linux 单用户本地控制面。后台只持有接管任务与观察连接，Turn saga 仍归 TurnCoordinator。
//! 公共视图不暴露 Pending 内部记录，协议版本只影响 IPC，不改变已有持久 schema。

mod auth;
mod client;
mod error;
pub(crate) mod ipc;
mod metadata;
mod operation;
pub(crate) mod presentation;
mod server;
mod types;

pub use auth::login::{AuthCatalogOutcome, AuthLoginEvent, AuthLoginOutcome, LocalAuthLogin};
pub use client::{LocalClient, LocalWatch};
pub use ipc::InstanceLock;
pub use lattice_conversation::{
    BoundToolCall, MessageRole, MessageSemantic, NativeMessage, ToolResultOutcome,
};
pub use operation::LocalTurn;
#[cfg(any(test, feature = "test-utils"))]
pub use server::serve_for_test;
pub use server::{DrainOutcome, ServeExit, serve};
pub use types::{
    Acceptance, AgentName, AgentNameError, AttemptProgress, AuthRemoval, AuthStatus, Availability,
    CancelStatus, ConfigurationRequirement, ConfigureOutcome, ConfigureReply, ConfigureRequest,
    ConfigureResult, ConfigureSession, ConflictReason, ConnectionStatus, ConversationEntry,
    CostEstimate, CostSummary, DaemonStatus, DetachReport, Effort, EffortAdjustment, HISTORY_TURNS,
    InvocationId, LocalError, LocalErrorKind, LocalHistory, LocalUpdate, MAX_HISTORY_TURNS,
    MaterialSummary, ModelAttemptKey, ModelCandidateQuery, ModelCandidates, ModelId, ModelIdError,
    ModelRef, ModelRefError, Node, NodeId, NodeKind, NodeRef, Notice, NoticeLevel, Page,
    ProviderAuthStatus, ProviderId, ProviderIdError, PublicTurnOutcome, RecoveryCause,
    RecoveryNeed, Result, ResultPayload, RetryNotice, RevocationStatus, SensitiveLoginUrl,
    SessionHead, SessionId, SessionListRow, SessionLog, SessionRecord, SessionUsage,
    SettingsVersion, SlotProgress, SlotReason, SlotState, StepContent, StepDetail, StepEnd,
    StepKey, StepProgress, StoreDiagnostic, StoredToolResult, SubmissionReceipt, SubmissionResult,
    SubmissionUpdate, SuccessContent, TextSegment, TextUpdate, TokenSummary, ToolError, TurnDetail,
    TurnId, TurnLookup, TurnRef, TurnRequest, TurnSummary, TurnUsageRow, TurnView,
    UsageCompleteness, UsageSummary, ViewUpdate, WATCH_TURNS,
};
#[cfg(test)]
use types::{AuthLoggedOut, Request, Response};

pub fn new_session_id() -> SessionId {
    SessionId::parse(&uuid::Uuid::new_v4().simple().to_string()).expect("generated SessionId")
}

/// 身份在发送之前分配，与输入文本无关；调用者负责保存并显示它。
pub fn new_turn_id() -> TurnId {
    TurnId::parse(&uuid::Uuid::new_v4().to_string()).expect("generated canonical UUID")
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support {
    /// 当前 Local IPC 帧魔数。测试替身用它收发帧，协议升级时不必手改字面量。
    pub const FRAME_MAGIC: &[u8; 8] = super::ipc::MAGIC;
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod client_tests;
