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
pub mod execution_backend;
pub(crate) mod executor;
mod handlers;
pub mod provenance {
    pub use lancet_audit::provenance::*;
}
mod server;

pub use enforcement::isolation::{
    HostProbeEntry, HostProbeReport, MissingRequiredBoundary, probe_host_capabilities,
    unavailable_required_boundaries,
};
pub use execution_backend::{
    DirectVerifyConfig, DirectVerifyConfigError, DirectoryEntry, ExecOutput, ExecRequest,
    ExecRequestError, ExecutionBackend, ExecutionBackendError, ExecutionRouting, FindRequest,
    GrepMatch, GrepRequest, PathFacts, PathIntent, PathKind, RoutingInvocationState, UpperContext,
    WorkspacePath, WorkspacePathError,
};
#[cfg(feature = "inspect-scaffold")]
pub use handlers::register_inspect_scaffold_methods;
pub use lancet_audit::{
    AttestationRecord, AuditQueryFilters, CallAuditMetadata, CepAuditContext, CepAuditStore,
    RejectionRecord, RejectionStage, SEMANTIC_STAGE_NATIVE, impact_kind_for_method,
    impact_kind_from_label, impact_kind_label,
};
pub use lancet_dispatch::{CepClient, CepServer, CepTransport, wire_code_for_error};
pub use provenance::{
    build_in_toto_statement, canonicalize_in_toto_statement, source_audit_range_hash,
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
    /// use lancet_native::DirectToolExecutor;
    /// let _ = DirectToolExecutor::for_project_root(".");
    /// ```
    const _DIRECT_TOOL_EXECUTOR_NOT_PUBLIC: () = ();

    /// ```compile_fail
    /// use lancet_native::ToolExecutor;
    /// ```
    const _TOOL_EXECUTOR_NOT_PUBLIC: () = ();
}
