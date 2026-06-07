// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET native production implementation.
//!
//! Low-level executor helpers stay crate-private and are reachable only through
//! native catalog handlers. `lattice-agent` cannot reach them at compile time.

pub mod audit {
    pub use lancet_audit::*;
}
pub(crate) mod edit;
pub mod enforcement;
pub(crate) mod executor;
mod handlers;
pub mod provenance {
    pub use lancet_audit::provenance::*;
}
mod server;

pub use enforcement::isolation::{
    probe_host_capabilities, unavailable_required_boundaries, HostProbeEntry, HostProbeReport,
    MissingRequiredBoundary,
};
#[cfg(feature = "inspect-scaffold")]
pub use handlers::register_inspect_scaffold_methods;
pub use lancet_audit::{
    impact_kind_for_method, impact_kind_from_label, impact_kind_label, AttestationRecord,
    AuditQueryFilters, CallAuditMetadata, CepAuditContext, CepAuditStore, SEMANTIC_STAGE_NATIVE,
};
pub use lancet_dispatch::{wire_code_for_error, CepClient, CepServer, CepTransport};
pub use provenance::{
    build_in_toto_statement, canonicalize_in_toto_statement, source_audit_range_hash,
};
pub use server::{
    register_production_method_registry, CepServerNativeExt, HandlerContext, MethodRegistry,
    ServerRunConfig,
};

#[cfg(test)]
mod effect_contract_tests;

#[cfg(test)]
mod public_api_tests {
    /// ```compile_fail
    /// use lancet_native::DirectToolExecutor;
    /// let _ = DirectToolExecutor::for_project_root(".");
    /// ```
    const _DIRECT_TOOL_EXECUTOR_NOT_PUBLIC: () = ();

    /// ```compile_fail
    /// use lancet_native::ToolExecutor;
    /// ```
    const _TOOL_EXECUTOR_NOT_PUBLIC: () = ();
}
