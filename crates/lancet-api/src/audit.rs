// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Thin operator audit facade over [`lancet_audit::operator`].
//!
//! Behavior, DTO ownership, and contract documentation (pagination, the
//! cancellation contract, the replay output contract, and the concurrency
//! invariant) live in [`lancet_audit::operator`]. This module only re-exports
//! the operator DTOs under their established `lancet_api` paths and maps
//! [`OperatorAuditError`] onto [`LancetError`] (messages pass through
//! verbatim). Operator reads never compose a CEP server: the behavior lives in
//! `lancet-audit`, which depends on none of `lancet-native`, `lancet-dispatch`,
//! `lancet-cep`, or `lancet-runtime`, so the boundary holds structurally rather
//! than by assertion.

use lancet_audit::OperatorAuditError;
pub use lancet_audit::{
    AUDIT_QUERY_MAX_ROWS, AUDIT_TAIL_MAX_BATCHES, AUDIT_TAIL_MAX_ROWS, AuditQueryOutput,
    AuditReplayRequest, AuditReplayResult, AuditRequest, AuditTailOutput, OperatorAuditRow,
    OperatorTailRow, OperatorTurnTrace,
};

use crate::error::{LancetError, LancetResult};

/// Map operator audit errors onto the facade error vocabulary 1:1.
///
/// Messages pass through verbatim so wire-visible error text stays
/// byte-identical across the mapping (see [`OperatorAuditError`]'s Display
/// contract).
fn map_operator_error(err: OperatorAuditError) -> LancetError {
    match err {
        OperatorAuditError::Config(msg) => LancetError::Config(msg),
        OperatorAuditError::Internal(msg) => LancetError::Internal(msg),
    }
}

/// Query finalized audit rows directly from the project audit store.
///
/// Delegates to [`lancet_audit::query_audit`].
pub async fn query_audit(request: AuditRequest) -> LancetResult<AuditQueryOutput> {
    lancet_audit::query_audit(request)
        .await
        .map_err(map_operator_error)
}

/// Tail finalized audit rows directly from the project audit store.
///
/// Delegates to [`lancet_audit::tail_audit`]; see its docs for the
/// pagination and cancellation contract.
pub async fn tail_audit(request: AuditRequest) -> LancetResult<AuditTailOutput> {
    lancet_audit::tail_audit(request)
        .await
        .map_err(map_operator_error)
}

/// Read one turn's finalized operator calls.
///
/// Delegates to [`lancet_audit::replay_audit`]; see its docs for the output
/// contract.
pub async fn replay_audit(request: AuditReplayRequest) -> LancetResult<AuditReplayResult> {
    lancet_audit::replay_audit(request)
        .await
        .map_err(map_operator_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_audit::{ScopedXdgStateHome, invocation_store::InvocationStore};
    use tempfile::TempDir;

    #[tokio::test]
    async fn query_audit_reads_invocation_store_without_cep_server() {
        let _xdg = ScopedXdgStateHome::new();
        let project = TempDir::new().unwrap();
        InvocationStore::create_project(project.path()).unwrap();

        let output = query_audit(AuditRequest::new(project.path()))
            .await
            .expect("query");
        assert_eq!(output.row_count, 0);
        assert_eq!(output.ledger_oid.len(), 40);
    }

    #[tokio::test]
    async fn query_audit_errors_when_store_missing() {
        let _xdg = ScopedXdgStateHome::new();
        let project = TempDir::new().unwrap();
        let err = query_audit(AuditRequest::new(project.path()))
            .await
            .expect_err("missing store");
        assert!(matches!(err, LancetError::Config(_)));
    }
}
