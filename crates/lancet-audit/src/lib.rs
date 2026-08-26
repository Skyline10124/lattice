// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Authoritative Git invocation ledger and operator reads.

pub mod invocation_store;
pub mod operator;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use operator::{
    AUDIT_QUERY_MAX_ROWS, AUDIT_TAIL_MAX_BATCHES, AUDIT_TAIL_MAX_ROWS, AuditQueryOutput,
    AuditReplayRequest, AuditReplayResult, AuditRequest, AuditTailOutput, OperatorAuditError,
    OperatorAuditResult, OperatorAuditRow, OperatorTailRow, OperatorTurnTrace, query_audit,
    replay_audit, tail_audit,
};
#[cfg(any(test, feature = "test-support"))]
pub use test_support::ScopedXdgStateHome;
