// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Linux 单用户本地控制面。后台只持有接管任务与观察连接，Turn saga 仍归 TurnCoordinator。
//! 公共视图不暴露 Pending 内部记录，协议版本只影响 IPC，不改变已有持久 schema。

mod client;
mod config;
mod ipc;
mod server;
mod types;

pub use client::{LocalClient, LocalSubscription};
pub use config::FrozenConfiguration;
pub use lattice_conversation::{
    BoundToolCall, MessageRole, MessageSemantic, NativeMessage, ToolResultOutcome,
};
pub use server::serve;
pub use types::{LocalRewindResult, SessionWriteResult};

/// 身份在发送之前分配，与输入文本无关；调用者负责保存并显示它。
pub fn new_turn_id() -> TurnId {
    TurnId::parse(&uuid::Uuid::new_v4().to_string()).expect("generated canonical UUID")
}

pub use types::{
    Acceptance, CommittedTurnObservation, DaemonStatus, EventCursor, LocalError, LocalErrorKind,
    Observation, PendingObservation, Result, SessionObservation, SessionWrite, TranscriptPage,
    TurnId, TurnObservation, TurnRequest,
};

#[cfg(test)]
mod tests;
