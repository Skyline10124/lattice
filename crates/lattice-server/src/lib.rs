// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 本地后台：实例锁持有、IPC 连接服务、准入与 Owner 监督、停机排空及 Turn 协调。
//!
//! 生产公开面仅由根模块导出 [`serve`]、[`ServeExit`]、[`DrainOutcome`] 与 [`InstanceLock`]；
//! 测试与 `test-utils` 下额外导出集成测试所需的 `serve_for_test`、`SessionApi`、`ApiError`、
//! `LocalWorkspaceRoot`、`start_turn` 及工具执行接缝类型。

mod config_rejection;
mod configure;
mod local;
mod profile;
mod redact;
mod root;
mod session;
mod stores;
mod turn;

pub use local::{DrainOutcome, InstanceLock, ServeExit, serve};

#[cfg(any(test, feature = "test-utils"))]
pub use config_rejection::ConfigRejection;
#[cfg(any(test, feature = "test-utils"))]
pub use local::serve_for_test;
#[cfg(any(test, feature = "test-utils"))]
pub use root::LocalWorkspaceRoot;
#[cfg(any(test, feature = "test-utils"))]
pub use session::{ApiError, SessionApi};
#[cfg(any(test, feature = "test-utils"))]
pub use turn::{
    RunTurnRequest, ToolExecutor, ToolOutcome, ToolSpec, TurnAcceptance, TurnCompletion,
    TurnExecution, TurnTools, start_turn,
};
