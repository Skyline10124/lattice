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
    RewindBarrierFacts, RewindBarriers, RewindConflict, RewindError, RewindOutcome,
    RewindPartialState, RewindPathConflict, RewindPathPreflight, RewindRestoredPath, RewindStep,
    RewindSuccess, SessionHeadMove, rewind_to_turn, rewind_to_turn_with_barriers,
};
pub use root::{LocalWorkspaceRoot, resolve_local_workspace_root};
pub use session::{ApiError, CreateSessionRequest, OpenSessionRequest, SessionApi};
pub use turn::{
    LifecycleFailureKind, RunTurnRequest, RunTurnResult, RunTurnStage, RunTurnStatus,
    STREAM_IDLE_TIMEOUT_SECS, ToolResultSummary, TurnEvent, TurnEventStream, TurnLifecycleOutcome,
    TurnUsageSummary, UnfinalizedTurn, collect_turn_result_from_events, finalize_turn,
    outcome_workspace_less, run_turn, stream_turn,
};

#[doc(hidden)]
pub use turn::CepExecutorAdapter;
