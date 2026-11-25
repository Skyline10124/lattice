// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LATTICE 本地后台：实例锁持有、IPC 连接服务、进程内服务入口、IM 凭据存储、准入与 Owner 监督、停机排空及 Turn 协调。
//!
//! 生产公开面由根模块导出后台服务入口（[`serve`]、[`ServeExit`]、[`DrainOutcome`]、[`HostLifecycle`]、[`InstanceLock`]）、
//! 组件托管组装接口（[`ComponentRegistration`]、[`ComponentBuildOutcome`]、[`PreparedComponent`]、[`ComponentStop`]、[`ComponentRemountHandle`]、[`ScopedComponentFuture`]、[`install_component_panic_hook`]、[`scoped_component_future`]、[`with_component_panic_scope`]）、
//! 进程内服务接口（[`Service`]、[`InProcessTurn`]、[`InProcessTurnOutcome`]、[`StopNotification`]）以及
//! IM 凭据存储接口（[`ImAuthStore`]、[`ImAuthError`]、[`ImToken`]、[`ImPlatformCredentialState`]、[`TELEGRAM_PLATFORM`]）；
//! 测试与 `test-utils` 下额外导出集成测试所需的 `serve_for_test`、`serve_for_test_with_service`、
//! `serve_for_test_with_components`、`SessionApi`、`ApiError`、`LocalWorkspaceRoot`、`start_turn` 及工具执行接缝类型。

mod config_rejection;
mod configure;
mod im_auth;
mod local;
mod profile;
mod redact;
mod root;
mod session;
mod stores;
mod turn;

pub use im_auth::{
    ImAuthError, ImAuthStore, ImPlatformCredentialState, ImToken, TELEGRAM_PLATFORM,
};
pub use local::{
    ComponentBuildOutcome, ComponentRegistration, ComponentRemountHandle, ComponentStop,
    DrainOutcome, HostLifecycle, InProcessTurn, InProcessTurnOutcome, InstanceLock,
    PreparedComponent, ScopedComponentFuture, ServeExit, Service, StopNotification,
    install_component_panic_hook, scoped_component_future, serve, with_component_panic_scope,
};

#[cfg(any(test, feature = "test-utils"))]
pub use config_rejection::ConfigRejection;
#[cfg(any(test, feature = "test-utils"))]
pub use local::{serve_for_test, serve_for_test_with_components, serve_for_test_with_service};
#[cfg(any(test, feature = "test-utils"))]
pub use root::LocalWorkspaceRoot;
#[cfg(any(test, feature = "test-utils"))]
pub use session::{ApiError, SessionApi};
#[cfg(any(test, feature = "test-utils"))]
pub use turn::{
    RunTurnRequest, ToolExecutor, ToolOutcome, ToolSpec, TurnAcceptance, TurnCompletion,
    TurnExecution, TurnPayload, TurnTools, start_turn,
};
