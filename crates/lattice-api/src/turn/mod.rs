// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn execution and streaming API.
//!
//! [`stream_turn`] is the single production execution core: `TurnCoordinator`
//! drives the durable saga, emits public [`TurnEvent`] values through a bounded
//! channel, and finalizes exactly one typed Turn. [`run_turn`] drains that same
//! stream via [`collector::RunTurnCollector`] into [`RunTurnResult`].

mod cep;
mod collector;
mod coordinator;
#[cfg(feature = "test-crash-barriers")]
mod crash_barrier;
mod execute;
mod finalize;
mod invocation_facts;
mod live;
mod llm;
mod mapping;
mod observer;
mod saga;
mod types;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod run_tests;

#[cfg(test)]
mod stream_tests;

#[cfg(test)]
mod types_tests;

#[cfg(test)]
mod live_tests;

pub use collector::collect_turn_result_from_events;
pub use execute::{STREAM_IDLE_TIMEOUT_SECS, run_turn, stream_turn};
pub use types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, ToolResultSummary, TurnEvent, TurnEventStream,
    TurnUsageSummary,
};
