//! Turn execution and streaming API.
//!
//! [`stream_turn`] is the single production execution core: it runs the agent
//! loop live, emits public [`TurnEvent`] values through a bounded channel, and
//! commits exactly one turn. [`run_turn`] is a synchronous facade that drains
//! the same live stream via [`collector::RunTurnCollector`] and returns
//! [`RunTurnResult`].

mod collector;
mod execute;
mod live;
mod mapping;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod live_tests;

pub use collector::collect_turn_result_from_events;
pub use execute::{run_turn, stream_turn};
pub use types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, RunTurnStatus, ToolResultSummary, TurnEvent,
    TurnEventStream, TurnUsageSummary,
};
