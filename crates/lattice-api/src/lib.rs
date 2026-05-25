//! LATTICE v1.1 thin public API facade.
//!
//! This crate is the application entry point for session creation, session
//! lookup, turn execution, and streaming. It exposes the v1.1 surface through
//! [`SessionApi`], `run_turn`, and `stream_turn`.
//!
//! # Boundary
//!
//! This crate does not export topology migration tables or compatibility
//! shims. Public callers receive typed request, response, event, and session
//! facade values only.

mod session;
mod turn;

pub use session::{ApiError, CreateSessionRequest, OpenSessionRequest, SessionApi};
pub use turn::{
    collect_turn_result_from_events, run_turn, stream_turn, RunTurnRequest, RunTurnResult,
    RunTurnStage, RunTurnStatus, ToolResultSummary, TurnEvent, TurnEventStream, TurnUsageSummary,
};

#[doc(hidden)]
pub use turn::LancetExecutorAdapter;
