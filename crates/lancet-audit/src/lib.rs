// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET audit persistence, provenance projection, and operator audit reads.

pub mod audit;
pub mod evidence;
pub mod operator;
pub mod provenance;
pub mod store_path;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

pub use audit::*;
pub use evidence::{
    CallEvidence, EVIDENCE_SCHEMA_VERSION, EVIDENCE_SCHEMA_VERSION_PRE_MUTATION, EvidenceError,
    EvidenceId, EvidenceOutcome, StoredCallEvidence,
};
pub use operator::{
    AUDIT_TAIL_MAX_BATCHES, AUDIT_TAIL_MAX_ROWS, AuditQueryOutput, AuditReplayRequest,
    AuditReplayResult, AuditRequest, AuditTailOutput, OperatorAuditError, OperatorAuditResult,
    query_audit, replay_audit, tail_audit,
};
pub use provenance::*;
pub use store_path::{
    ExternalAuditPathError, audit_db_path_for_project, audit_db_path_for_project_in,
    audit_state_root, project_state_dir, project_state_dir_in, project_storage_key,
};
#[cfg(any(test, feature = "test-support"))]
pub use test_support::ScopedXdgStateHome;
