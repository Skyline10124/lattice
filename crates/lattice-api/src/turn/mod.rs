//! v1.1 turn execution and streaming API.
//!
//! [`run_turn`] orchestrates durable turn execution through the `TurnDeps`
//! dependency seam: create/open session, read transcript, invoke agent loop,
//! commit exactly one turn, and return typed results. [`stream_turn`] forwards
//! live agent events and appends a final `Committed` event after persistence.
//!
//! # Dependency seam
//!
//! Production callers use [`run_turn`] which internally constructs
//! [`RealTurnDeps`]. Tests use [`run_turn_with_deps`] with a fake
//! implementation of [`TurnDeps`] to avoid live provider calls, direct
//! SQLite access, or public exposure of internal handles.

mod deps;
mod execute;
mod mapping;
mod types;

#[cfg(test)]
mod tests;

pub use execute::{run_turn, stream_turn};
pub use types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, RunTurnStatus, StreamTurnRequest,
    ToolResultSummary, TurnEvent, TurnEventStream, TurnUsageSummary,
};
