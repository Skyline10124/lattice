// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Thin LANCET facade for TurnCoordinator composition and operator reads.
//!
//! Production Tool calls originate only in `lattice-api`. This crate binds the
//! durable InvocationStore to the generated Component runtime without exposing a
//! standalone server, registry, caller-supplied catalog, or direct CEP product API.

mod audit;
mod config;
mod doctor;
mod error;
mod rewind;
mod runtime;
mod status;

pub use audit::{
    AUDIT_QUERY_MAX_ROWS, AUDIT_TAIL_MAX_BATCHES, AUDIT_TAIL_MAX_ROWS, AuditQueryOutput,
    AuditReplayRequest, AuditReplayResult, AuditRequest, AuditTailOutput, OperatorAuditRow,
    OperatorTailRow, OperatorTurnTrace, query_audit, replay_audit, tail_audit,
};
pub use config::{LancetRuntimeConfig, workspace_history_backing, workspace_history_backing_in};
pub use doctor::{LancetDoctorOutput, LancetDoctorRequest, run_doctor_checks};
pub use error::{LancetError, LancetResult};
pub use rewind::{
    HeldWorkspaceOperation, WorkspaceRewindConflict, WorkspaceRewindError, WorkspaceRewindOutcome,
    WorkspaceRewindReport, plan_workspace_rewind, record_rewind_audit, rewind_audit_records,
    rewind_workspace,
};
pub use runtime::{DispatchNotStartedCause, DispatchOutcome, LancetRuntime};
pub use status::{LancetStatusOutput, collect_status};

pub use lancet_authority::policy::default::PolicyConfig;
pub use lancet_protocol::{CapabilityAdmissionPolicy, CepError};
pub use lancet_runtime::rewind_audit::{
    MAX_REWIND_AUDIT_READ, REWIND_AUDIT_SCHEMA_VERSION, RewindAuditLog, RewindAuditPage,
    RewindAuditRecord,
};
pub use lancet_runtime::workspace::rewind::{
    RestoredPath, RewindPathConflict, RewindPathPreflight, RewindRestoreError, RewindRestoreOutcome,
};
pub use lancet_runtime::{
    ComponentRuntime, CompositionError, ExecutionReceipt, HistorySnapshot, RewindPlan, RewindStep,
    ToolDefinition, WorkspaceHistory, WorkspaceHistoryBacking, WorkspaceHistoryError,
};
