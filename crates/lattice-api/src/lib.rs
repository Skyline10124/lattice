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

mod config_rejection;
mod redact;
pub use config_rejection::ConfigRejection;
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
pub use profile::{LoadedAgentProfile, resolve_named_agent_profile};
pub use root::{LocalWorkspaceRoot, resolve_local_workspace_root};
pub use session::{ApiError, SessionApi};
#[cfg(any(test, feature = "test-utils"))]
pub use turn::run_turn;
pub use turn::{
    RunTurnRequest, RunTurnResult, STREAM_IDLE_TIMEOUT_SECS, TurnAcceptance, TurnCompletion,
    TurnEvent, TurnExecution, TurnRef, start_turn,
};
