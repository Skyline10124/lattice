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
    AUDIT_TAIL_MAX_BATCHES, AUDIT_TAIL_MAX_ROWS, AuditQueryOutput, AuditReplayRequest,
    AuditReplayResult, AuditRequest, AuditTailOutput, OperatorAuditRow, OperatorCellEvidence,
    OperatorReplayCall, OperatorTailRow, OperatorTurnTrace,
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
    use lancet_audit::{
        AuditOutcome, CallAuditMetadata, CepAuditContext, CepAuditStore, ScopedXdgStateHome,
        audit_db_path_for_project_in,
    };

    /// `observe.workspace` as a plain method name. It is a Component Tool since
    /// #30, so there is no catalog type to take `NAME` from; the operator audit
    /// read path stores and queries method names as strings and never consults
    /// the catalog.
    const OBSERVE_WORKSPACE: &str = "observe.workspace";
    use std::path::Path;
    use tempfile::TempDir;

    /// Env-isolated project fixture: `XDG_STATE_HOME` redirected to a temp
    /// state root (via the shared [`ScopedXdgStateHome`] guard) plus a distinct
    /// temp project directory the audit store is keyed by.
    struct IsolatedProjectAudit {
        xdg: ScopedXdgStateHome,
        project: TempDir,
    }

    impl IsolatedProjectAudit {
        fn setup() -> Self {
            Self {
                xdg: ScopedXdgStateHome::new(),
                project: TempDir::new().expect("project tempdir"),
            }
        }

        fn state_path(&self) -> &Path {
            self.xdg.path()
        }

        fn project_path(&self) -> &Path {
            self.project.path()
        }
    }

    fn seed_audit_db(
        state: &Path,
        project: &Path,
        session_id: &str,
        count: usize,
    ) -> CepAuditStore {
        let db_path =
            audit_db_path_for_project_in(state, project).expect("resolve external audit path");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let audit = CepAuditStore::open(&db_path).expect("open audit db");
        for i in 0..count {
            let ctx = CepAuditContext {
                turn_id: "turn-op".into(),
                session_id: session_id.into(),
                model_id: "cli-model".into(),
                runtime_version: "test".into(),
            };
            let start = audit
                .start_call(
                    &ctx,
                    i as u64 + 1,
                    OBSERVE_WORKSPACE,
                    &CallAuditMetadata::native_policy_enforced(Some("started")),
                )
                .unwrap();
            audit
                .finalize_call(
                    start,
                    AuditOutcome::Success {
                        summary: format!("read ok {i}"),
                    },
                    Some("native_handler"),
                    Some(r#"{"required_capability":"read_path"}"#),
                )
                .unwrap();
        }
        audit
    }

    /// Delegation smoke: the facade reads the store the fixture seeded.
    /// Behavior coverage lives in `lancet-audit` (`src/operator_tests.rs`).
    #[tokio::test]
    async fn query_audit_reads_store_without_cep_server() {
        let iso = IsolatedProjectAudit::setup();
        let session = "sess-direct-query";
        seed_audit_db(iso.state_path(), iso.project_path(), session, 2);
        let mut request = AuditRequest::new(iso.project_path());
        request.session_id = session.to_string();
        let output = query_audit(request).await.expect("query");
        assert_eq!(output.session_id, session);
        assert_eq!(output.row_count, 2);
        assert_eq!(output.rows.len(), 2);
    }

    /// The error mapping must land operator `Config` errors on
    /// [`LancetError::Config`], not `Internal`.
    #[tokio::test]
    async fn query_audit_errors_when_store_missing() {
        let iso = IsolatedProjectAudit::setup();
        let request = AuditRequest::new(iso.project_path());
        let err = query_audit(request).await.expect_err("missing store");
        assert!(matches!(err, LancetError::Config(_)));
    }
}
