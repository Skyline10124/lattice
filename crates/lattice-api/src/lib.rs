//! LATTICE v1.1 thin public API facade.
//!
//! This crate replaces the future role of `lattice-runtime` as the application
//! entry point for session creation, turn execution, streaming, and topology
//! introspection. It exposes a flat v1.1 API surface via `create_session`,
//! `open_session`, `run_turn`, and `stream_turn`.
//!
//! # Phase 6 status
//!
//! In Phase 6, `lattice-api` is a skeleton: `run_turn` calls the deterministic
//! [`lattice_llm::StubLlm`] and returns `commit_id: None`. Real provider calls,
//! real session commits, and real tool execution are deferred to Phases 7–9.
//!
//! # Boundary
//!
//! This crate does not import or re-export types from `lattice-core`,
//! `lattice-runtime`, `lattice-prompt`, `lattice-sandbox`, or `lattice-policy`.
//! It defines its own request/response types directly.

mod session;
mod topology;
mod turn;

pub use session::{
    create_session, open_session, ApiError, ApiSession, CreateSessionRequest, OpenSessionRequest,
    SessionApi,
};
pub use topology::{migration_homes, TopologyEntry};
pub use turn::{
    run_turn, stream_turn, RunTurnRequest, RunTurnResult, RunTurnStage, StreamTurnRequest,
    TurnEvent, TurnEventStream,
};
