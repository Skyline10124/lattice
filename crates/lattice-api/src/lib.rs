// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 公开应用接口。
//!
//! [`SessionApi`] 提供会话操作，[`local::LocalClient`] 提供本地提交与观察，
//! [`start_turn`] 提供后台执行接线；`run_turn` 只在测试或 `test-utils` 下复用执行核心。
//!
//! # Boundary
//!
//! This crate does not export topology migration tables or compatibility
//! shims. Public callers receive typed request, response, event, and session
//! facade values only.

mod config_rejection;
mod configure;
mod redact;
pub use config_rejection::ConfigRejection;
pub mod local;
mod models;
mod profile;
pub mod root;
mod session;
mod stores;
pub mod turn;

pub use lattice_conversation::Effort;
pub use lattice_session::store::{NodeId, SettingsVersion, TurnSummary, UsageSummary};
pub use lattice_session::{
    AgentName, AgentNameError, ProjectPath, RetryPolicy, RetryPolicyError, StateLayout,
    TurnTimeout, TurnTimeoutError,
};
pub use models::{
    Availability, ConfigurationRequirement, ModelCandidate, ModelCandidateQuery, ModelCandidates,
    ModelId, ModelIdError, ModelPrice, ModelRef, ModelRefError, ModelWarning, ProviderId,
    ProviderIdError, init_logging,
};
pub use profile::LoadedAgentProfile;
pub use root::{LocalWorkspaceRoot, resolve_local_workspace_root};
pub use session::{ApiError, SessionApi};
#[cfg(any(test, feature = "test-utils"))]
pub use turn::run_turn;
pub use turn::{
    NoTools, RunTurnRequest, RunTurnResult, STREAM_IDLE_TIMEOUT_SECS, ToolExecutor, ToolOutcome,
    ToolSpec, TurnAcceptance, TurnCompletion, TurnEvent, TurnExecution, TurnRef, TurnTools,
    start_turn,
};
