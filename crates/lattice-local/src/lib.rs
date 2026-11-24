// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 本地前端客户端门面：客户端句柄、会话观察状态机、登录流、日志初始化与类型重导出。

mod client;
mod display;
mod endpoint;
mod image_file;
mod logging;
mod login;
mod operation;
mod turn_plan;

pub use client::LocalClient;
#[cfg(any(test, feature = "test-utils"))]
pub use client::LocalWatch;
pub use display::format_image_marker;
#[cfg(any(test, feature = "test-utils"))]
pub use endpoint::Departure;
pub use image_file::{image_bytes_ref, image_file_ref, probe_image_file, read_verified_image};
pub use lattice_conversation::{
    BoundToolCall, CostEstimate, CostSummary, Effort, ImageMediaType, ImageRef, InputModality,
    InvocationId, MAX_IMAGE_BYTES, MessageRole, MessageSemantic, ModelAttemptKey, ModelId,
    ModelIdError, ModelRef, ModelRefError, NativeMessage, ProviderId, ProviderIdError,
    ResultPayload, SessionId, StepContent, StoredToolResult, SuccessContent, TokenSummary,
    TokenUsage, ToolError, ToolResultOutcome, TurnId, UsageCompleteness,
};
pub use lattice_ipc::{
    Acceptance, AgentName, AgentNameError, AttemptProgress, AuthCatalogOutcome, AuthLoginOutcome,
    AuthRemoval, AuthStatus, Availability, CancelStatus, Channel, ChannelError, ComponentState,
    ComponentStatus, ConfigurationRequirement, ConfigureReply, ConfigureRequest, ConfigureResult,
    ConfigureSession, ConflictReason, ConnectionStatus, ContextEdit, ContextError, ContextInput,
    ContextOp, ContextText, ConversationEntry, DaemonStatus, DrainTimeout, EffectiveContext,
    EffortAdjustment, HISTORY_TURNS, ImAuthRemoveReply, ImAuthSetReply, ImAuthStatus,
    ImCredentialStatus, ImMountResult, ImPlatformAuthStatus, LocalError, LocalErrorKind,
    LocalHistory, LocalUpdate, MAX_HISTORY_TURNS, MaterialSummary, ModelCandidate,
    ModelCandidateQuery, ModelCandidates, ModelPrice, ModelWarning, Node, NodeId, NodeKind,
    NodeRef, NormalizedUserMessage, Notice, NoticeLevel, Page, PlannedTurnRequest, ProjectPath,
    ProviderAuthStatus, PublicTurnOutcome, RecoveryCause, RecoveryNeed, RequestConflict, Result,
    RetryNotice, RetryPolicy, RetryPolicyError, RevocationStatus, SessionHead, SessionListRow,
    SessionLog, SessionRecord, SessionUsage, SettingsVersion, SlotProgress, SlotReason, SlotState,
    StepDetail, StepEnd, StepKey, StepProgress, StoreDiagnostic, SubmissionReceipt,
    SubmissionResult, SubmissionUpdate, TextSegment, TextUpdate, ToolResultText, TurnDetail,
    TurnLookup, TurnRef, TurnRequest, TurnStartMeta, TurnSummary, TurnTimeout, TurnTimeoutError,
    TurnUsageRow, TurnView, UsageSummary, UserInputPart, ViewUpdate, WATCH_TURNS,
    parse_drain_timeout,
};
pub use lattice_store::{PrivateDirectory, StateLayout, xdg_state_root};
pub use logging::init_logging;
pub use login::{AuthLoginEvent, LocalAuthLogin, SensitiveLoginUrl};
pub use operation::{ConfigureOutcome, DetachReport, LocalTurn, STREAM_IDLE_TIMEOUT_SECS};
pub use turn_plan::check_turn_plan;

pub fn new_session_id() -> SessionId {
    SessionId::parse(&uuid::Uuid::new_v4().simple().to_string()).expect("生成的 SessionId 格式合法")
}

/// 身份在发送之前分配，与输入文本无关；调用者负责保存并显示它。
pub fn new_turn_id() -> TurnId {
    TurnId::parse(&uuid::Uuid::new_v4().to_string()).expect("生成的规范 UUID 格式合法")
}

#[cfg(any(test, feature = "test-utils"))]
pub mod test_support {
    /// 当前 Local IPC 帧魔数。测试替身用它收发帧，协议升级时不必手改字面量。
    pub const FRAME_MAGIC: &[u8; 8] = lattice_ipc::ipc::MAGIC;

    /// 客户端寻址规则的测试辅助包装，供后台实例锁与残留路径测试核对同一 socket 路径。
    pub fn endpoint(layout: &crate::StateLayout) -> crate::Result<std::path::PathBuf> {
        crate::endpoint::endpoint(layout)
    }

    /// 按客户端连接规则建立本地 IPC 连接，供后台入场与协议测试复用。
    pub async fn connect(layout: crate::StateLayout) -> crate::Result<tokio::net::UnixStream> {
        crate::endpoint::connect(layout).await
    }
}
