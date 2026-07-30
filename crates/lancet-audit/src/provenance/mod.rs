// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! PROV projection and unsigned in-toto attestation builders (flat audit rows → graph).

pub mod attestation;
pub mod canonicalize;
pub mod projector;

pub use attestation::{
    AttestationVersion, build_in_toto_statement, build_in_toto_statement_with_evidence,
    rebuild_in_toto_statement, rebuild_in_toto_statement_with_evidence, source_audit_range_hash,
    statement_bytes,
};
pub use canonicalize::{
    canonicalize_in_toto_statement, canonicalize_prov_fragment, canonicalize_turn_trace,
    traces_equal_modulo_nondeterminism,
};
pub use projector::{
    EvidenceLookup, project_rows, project_rows_with_evidence, project_turn_trace,
    project_turn_trace_with_evidence,
};
