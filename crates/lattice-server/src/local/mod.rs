// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Linux 单用户本地控制面。后台只持有接管任务与观察连接，Turn saga 仍归 TurnCoordinator。
//! 公共视图不暴露 Pending 内部记录，协议版本只影响 IPC，不改变已有持久 schema。

mod auth;
mod error;
pub(crate) mod ipc;
mod metadata;
pub(crate) mod presentation;
mod server;
mod types;

pub use ipc::InstanceLock;
#[cfg(any(test, feature = "test-utils"))]
pub use server::serve_for_test;
pub use server::{DrainOutcome, HostLifecycle, ServeExit, serve};
pub(crate) use types::*;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod client_tests;
