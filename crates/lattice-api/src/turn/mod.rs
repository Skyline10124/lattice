// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn execution and streaming API.
//!
//! [`stream_turn`] is the single production execution core: it runs the agent
//! loop live, emits public [`TurnEvent`] values through a bounded channel, and
//! commits exactly one turn. [`run_turn`] is a synchronous facade that drains
//! the same live stream via [`collector::RunTurnCollector`] and returns
//! [`RunTurnResult`].

mod cep;
mod collector;
// #139-B coordinator shell: private until the single production hard cut.
mod coordinator;
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
mod cep_tests;

#[cfg(test)]
mod live_tests;

pub use collector::collect_turn_result_from_events;
pub use execute::{STREAM_IDLE_TIMEOUT_SECS, run_turn, stream_turn};
pub use types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, ToolResultSummary, TurnEvent, TurnEventStream,
    TurnUsageSummary,
};

#[doc(hidden)]
pub use cep::{CepErrorPayload, ToolExecutionError};
