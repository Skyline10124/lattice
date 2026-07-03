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
mod session;
pub mod turn;

#[cfg(test)]
mod test_env;

#[cfg(test)]
pub use test_env::ScopedXdgStateHome;

pub use lattice_session::{
    GraphDiff, GraphHead, GraphRef, GraphSession, ResetResult, SessionGraphSummary, SessionStatus,
    TurnLogEntry,
};
pub use lattice_workspace::CandidateExport;
pub use lattice_workspace::promotion::PinnedCandidate;
pub use lattice_workspace::promotion::{PromotionLimits, PromotionOutcome, VerificationResult};
pub use models::{ModelInfo, init_logging, list_models, resolve_model};
pub use profile::{LoadedAgentProfile, load_agent_profile_path, resolve_named_agent_profile};
pub use session::{
    ApiError, CreateSessionRequest, OpenSessionRequest, SessionApi, check_migration_guard,
};
pub use turn::{
    LifecycleFailureKind, PromotionLifecycleError, PromotionSessionMetadata, PromotionTurnTarget,
    PruneSnapshotsRequest, ResourceAction, ResumeError, ResumeMode, ResumeTurnRequest,
    RunTurnRequest, RunTurnResult, RunTurnStage, RunTurnStatus, SnapshotPruneResult,
    ToolResultSummary, TurnEvent, TurnEventStream, TurnLifecycleOutcome, TurnTargetError,
    TurnUsageSummary, UnfinalizedTurn, WorkspaceDirtyHook, classify_lifecycle_failure,
    collect_turn_result_from_events, commit_promoted_with_retry, finalize_turn, outcome_promoted,
    outcome_workspace_less, prepare_promotion_target, promote_preexported_candidate,
    promote_verified_candidate, prune_session_snapshots, resolve_resume_mode, resolve_turn_target,
    resume_turn, retry_pinned_candidate, run_terminal_verification, run_turn, stream_turn,
};

#[doc(hidden)]
pub use turn::CepExecutorAdapter;
pub use turn::upper::{ResolvedUpperConfig, UpperPrepareError, resolve_upper_config};
pub use turn::workspace_backend::WorkspaceExecutionBackend;
