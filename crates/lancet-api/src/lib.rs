// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Thin LANCET facade for TurnCoordinator composition and operator reads.
//!
//! 生产调用仅由 `lattice-api` 发起；门面消费会话库提交的 Started 凭据，
//! 交给生成的 Component 运行时，不提供独立执行产品或调用方自组目录。

mod audit;
mod config;
mod doctor;
mod error;
mod runtime;
mod status;

pub use audit::{
    AuditFilter, AuditPage, AuditReplay, AuditTailPage, ChangeSeq, InvocationChange,
    OperatorInvocation, OperatorInvocationState, query_audit, replay_audit, tail_audit,
};
pub use config::{LancetRuntimeConfig, workspace_history_backing_in};
pub use doctor::{LancetDoctorOutput, LancetDoctorRequest, run_doctor_checks};
pub use error::LancetError;
pub use runtime::LancetRuntime;
pub use status::{OperatorStatus, collect_status};

pub use lancet_authority::policy::default::PolicyConfig;
pub use lancet_protocol::{CapabilityAdmissionPolicy, CepError};
pub use lancet_runtime::{
    ComponentRuntime, CompositionError, ExecutionReceipt, HistorySnapshot, ToolDefinition,
    WorkspaceHistory, WorkspaceHistoryBacking, WorkspaceHistoryError,
};
