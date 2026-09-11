// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE thin public API facade.
//!
//! This crate is the application entry point for session creation, session
//! lookup, turn execution, and streaming. It exposes the public surface through
//! [`SessionApi`], `run_turn`, and `stream_turn`.
//!
//! # Boundary
//!
//! This crate does not export topology migration tables or compatibility
//! shims. Public callers receive typed request, response, event, and session
//! facade values only.

mod models;
mod profile;
mod rewind;
pub mod root;
mod session;
pub mod stores;
pub mod turn;

pub use lancet_api::CompositionError;
pub use lattice_session::{
    ClosureOrigin, TurnFinishReason, TurnOutcome, WorkspaceHistoryCheckpoint,
};
pub use lattice_session::{
    GraphDiff, GraphHead, GraphRef, GraphSession, ResetResult, SessionGraphSummary, SessionStatus,
    TurnLogEntry,
};
/// Private-state addressing, re-exported so `lattice-tui` keeps its single
/// dependency on this facade (ADR 0005 §3).
pub use lattice_session::{ProjectStoreKey, StateLayout};
pub use models::{ModelInfo, init_logging, list_models, resolve_model};
pub use profile::{LoadedAgentProfile, load_agent_profile_path, resolve_named_agent_profile};
pub use rewind::{
    RewindAuditRecordObservation, RewindBarrierFacts, RewindBarriers, RewindConflict, RewindError,
    RewindOutcome, RewindPartialState, RewindPathConflict, RewindPathPreflight,
    RewindRefObservation, RewindRestoredPath, RewindStep, RewindSuccess, SessionHeadMove,
    rewind_to_turn, rewind_to_turn_with_barriers,
};
pub use root::{LocalWorkspaceRoot, resolve_local_workspace_root};
pub use session::{
    ApiError, CreateSessionRequest, EvidenceDurabilityFailed, OpenSessionRequest, RecoveryRequired,
    SessionApi,
};
pub use turn::{
    RunTurnRequest, RunTurnResult, RunTurnStage, STREAM_IDLE_TIMEOUT_SECS, ToolResultSummary,
    TurnAcceptance, TurnCompletion, TurnEvent, TurnEventStream, TurnExecution, TurnUsageSummary,
    collect_turn_result_from_events, recover_turn, run_turn, start_turn, stream_turn,
};

/// 提前组成进程共享的 Component 运行时，将首次新执行所需的编译成本移到预热阶段。
/// 组成错误会被缓存，并在后续需要 Component 组成的新执行路径中返回。
/// 只读查询、既有请求找回及 closure-only 恢复不依赖此预热，也不因预热失败而被拒绝。
pub fn warmup_component_runtime() -> Result<(), std::sync::Arc<CompositionError>> {
    lancet_api::ComponentRuntime::shared().map(|_| ())
}
