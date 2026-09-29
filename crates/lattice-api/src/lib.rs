// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 公开应用接口。
//!
//! [`SessionApi`] 提供会话操作，[`local::LocalClient`] 提供本地提交与观察，
//! [`start_turn`] 提供后台执行接线；run_turn/stream_turn 仅供测试复用执行核心。
//!
//! # Boundary
//!
//! This crate does not export topology migration tables or compatibility
//! shims. Public callers receive typed request, response, event, and session
//! facade values only.

pub mod local;
mod models;
mod profile;
mod profile_paths;
pub use profile_paths::validate_agents_dir;
pub mod root;
mod session;
mod stores;
pub mod turn;

pub use lancet_api::CompositionError;
pub use lattice_session::store::{NodeId, TurnSummary, UsageSummary};
pub use lattice_session::{ProjectStoreKey, StateLayout};
pub use models::{ModelInfo, init_logging, list_models, resolve_model};
pub use profile::{LoadedAgentProfile, load_agent_profile_path, resolve_named_agent_profile};
pub use root::{LocalWorkspaceRoot, resolve_local_workspace_root};
pub use session::{ApiError, SessionApi};
#[cfg(any(test, feature = "test-utils"))]
pub use turn::run_turn;
pub use turn::{
    RunTurnRequest, RunTurnResult, STREAM_IDLE_TIMEOUT_SECS, TurnAcceptance, TurnCompletion,
    TurnEvent, TurnEventStream, TurnExecution, TurnRef, start_turn,
};

/// 提前组成进程共享的 Component 运行时，将首次新执行所需的编译成本移到预热阶段。
/// 组成错误会被缓存，并在后续需要 Component 组成的新执行路径中返回。
/// 只读查询、既有请求找回及 closure-only 恢复不依赖此预热，也不因预热失败而被拒绝。
pub fn warmup_component_runtime() -> Result<(), std::sync::Arc<CompositionError>> {
    lancet_api::ComponentRuntime::shared().map(|_| ())
}
