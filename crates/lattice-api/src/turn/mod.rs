// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn 提交、恢复与观察的公开接口。
//!
//! [`start_turn`] 接管同进程执行任务，分别返回持久接受和完成回执。
//! run_turn/stream_turn 仅在测试或 test-utils 下复用该执行路径；
//! 有损事件仅用于观察，持久 saga 与最终发布仍由 TurnCoordinator 独占负责。

pub(crate) mod cep;
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
#[cfg(test)]
pub(crate) use coordinator::finalize_fault;
pub use execute::{STREAM_IDLE_TIMEOUT_SECS, recover_turn, start_turn};
pub(crate) use execute::{recover_turn_with_target, start_execution};
#[cfg(any(test, feature = "test-utils"))]
pub use execute::{run_turn, stream_turn};
pub use types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, SequencedTurnEvent, SequencedTurnEventStream,
    ToolResultSummary, TurnAcceptance, TurnCompletion, TurnEvent, TurnEventStream, TurnExecution,
    TurnUsageSummary,
};
