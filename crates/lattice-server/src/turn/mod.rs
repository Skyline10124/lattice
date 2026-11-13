// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn 提交、恢复与观察的协调层内部接口。
//!
//! 生产路径经 [`start_execution`] 接管同进程执行任务；[`start_turn`] 仅在测试与 `test-utils` 下导出，
//! `execute::run_turn` 仅在单元测试下复用该执行路径。Owner 同步保存当前尝试的观察段，持久执行与关闭由 `TurnCoordinator` 独占负责。

pub(crate) mod coordinator;
#[cfg(feature = "test-crash-barriers")]
pub(crate) mod crash_barrier;
pub(crate) mod execute;
mod live;
pub(crate) mod observer;
mod saga;
pub(crate) mod tools;
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

pub(crate) use execute::start_execution;
#[cfg(any(test, feature = "test-utils"))]
pub use execute::start_turn;
#[cfg(test)]
pub(crate) use tools::NoTools;
pub use tools::ToolExecutor;
pub(crate) use tools::default_tool_executor;
#[cfg(any(test, feature = "test-utils"))]
pub use tools::{ToolOutcome, ToolSpec, TurnTools};
#[cfg(test)]
pub(crate) use types::TurnEvent;
#[cfg(any(test, feature = "test-utils"))]
pub use types::TurnExecution;
pub use types::{RunTurnRequest, TurnAcceptance, TurnCompletion};
