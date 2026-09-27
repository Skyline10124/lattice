// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn 提交、恢复与观察的公开接口。
//!
//! [`start_turn`] 接管同进程执行任务，分别返回持久接受和完成回执。
//! run_turn 仅在测试或 test-utils 下复用该执行路径；
//! Owner 同步保存观察段与有界事件窗口，持久执行与关闭由 TurnCoordinator 独占负责。

pub(crate) mod cep;
pub(crate) mod coordinator;
#[cfg(feature = "test-crash-barriers")]
mod crash_barrier;
pub(crate) mod execute;
mod live;
mod llm;
pub(crate) mod observer;
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

#[cfg(any(test, feature = "test-utils"))]
pub use execute::run_turn;
pub(crate) use execute::start_execution;
pub use execute::{STREAM_IDLE_TIMEOUT_SECS, start_turn};
pub use types::{
    AttemptSegments, RunTurnRequest, RunTurnResult, SequencedTurnEvent, SequencedTurnEventStream,
    TextSegment, TurnAcceptance, TurnCompletion, TurnEvent, TurnEventStream, TurnExecution,
    TurnRef,
};
