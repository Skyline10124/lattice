// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET native production implementation.
//!
//! Low-level executor helpers stay crate-private and are reachable only through
//! native catalog handlers. `lattice-agent` cannot reach them at compile time.

pub mod audit {
    //! Audit *persistence* surface only.
    //!
    //! Deliberately re-exports `lancet_audit::audit`, not the `lancet_audit`
    //! crate root: since GitHub #39 the root also carries the operator read
    //! path (`query_audit` / `tail_audit` / `replay_audit` and their DTOs) and
    //! the project-keyed store-path helpers. Those are operator-only and have
    //! no business widening this crate's public API — a glob over the root
    //! would republish them here as `lancet_native::audit::query_audit`.
    pub use lancet_audit::audit::*;
}
pub(crate) mod edit;
pub mod enforcement;
pub(crate) mod executor;
mod handlers;
pub mod provenance {
    pub use lancet_audit::provenance::*;
}
mod server;
pub(crate) mod workspace;

/// Explicit Rewind restore executor (GitHub #64).
///
/// Host-side operator surface: preflights every affected path against its
/// recorded post-state and restores parent-tree content with the same safety
/// primitives the mutation path uses. Not a CEP method — the Agent-visible
/// catalog gains nothing here.
pub mod rewind {
    pub use crate::workspace::rewind::{
        RestoredPath, RewindPathConflict, RewindPathPreflight, RewindRestoreError,
        RewindRestoreOutcome, preflight_and_restore,
    };
}

#[cfg(feature = "inspect-scaffold")]
pub use handlers::register_inspect_scaffold_methods;
pub use lancet_audit::{
    AttestationRecord, AuditQueryFilters, CallAuditMetadata, CallEvidence, CepAuditContext,
    CepAuditStore, EvidenceError, EvidenceId, EvidenceOutcome, RejectionRecord, RejectionStage,
    SEMANTIC_STAGE_NATIVE, StoredCallEvidence, impact_kind_for_method, impact_kind_from_label,
    impact_kind_label,
};
pub use lancet_dispatch::{CepClient, CepServer, CepTransport, wire_code_for_error};
pub use provenance::{
    EvidenceLookup, build_in_toto_statement, build_in_toto_statement_with_evidence,
    canonicalize_in_toto_statement, rebuild_in_toto_statement,
    rebuild_in_toto_statement_with_evidence, source_audit_range_hash, statement_bytes,
};
pub use server::{
    CepServerNativeExt, HandlerContext, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};

#[cfg(test)]
mod effect_contract_tests;

#[cfg(test)]
mod public_api_tests {
    /// ```compile_fail
    /// use lancet_native::ToolExecutor;
    /// ```
    const _TOOL_EXECUTOR_NOT_PUBLIC: () = ();
}
