//! Turn execution and streaming API.
//!
//! [`run_turn`] orchestrates durable turn execution through the `TurnDeps`
//! dependency seam: create/open session, read transcript, invoke agent loop,
//! commit exactly one turn, and return typed results. [`stream_turn`] takes
//! the same [`RunTurnRequest`] and returns a live stream of [`TurnEvent`]
//! values backed by a spawned task and bounded channel; startup failures are
//! returned as `Result::Err` before the stream, runtime outcomes arrive as
//! events within the stream.
//!
//! # Dependency seam
//!
//! Production callers use [`run_turn`] which internally constructs
//! [`RealTurnDeps`]. Tests use [`run_turn_with_deps`] with a fake
//! implementation of [`TurnDeps`] to avoid live provider calls, direct
//! SQLite access, or public exposure of internal handles. The live
//! `stream_turn` path bypasses `TurnDeps` entirely.

mod deps;
mod execute;
mod live;
mod mapping;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_tests;

pub use execute::{run_turn, stream_turn};
pub use types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, RunTurnStatus, ToolResultSummary, TurnEvent,
    TurnEventStream, TurnUsageSummary,
};
