// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! PROV projection and unsigned in-toto attestation builders (flat audit rows → graph).

pub mod attestation;
pub mod canonicalize;
pub mod projector;

pub use attestation::{build_in_toto_statement, source_audit_range_hash};
pub use canonicalize::{
    canonicalize_in_toto_statement, canonicalize_prov_fragment, canonicalize_turn_trace,
    traces_equal_modulo_nondeterminism,
};
pub use projector::{project_rows, project_turn_trace};
