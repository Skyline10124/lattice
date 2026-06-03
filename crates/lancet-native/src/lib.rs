//! LANCET native production implementation.
//!
//! Low-level executor helpers stay crate-private and are reachable only through
//! native catalog handlers. `lattice-agent` cannot reach them at compile time.
//! [`to_tool_definitions`] is the only public projection from the catalog to
//! LLM-facing tool schemas.

pub mod audit {
    pub use lancet_audit::*;
}
pub(crate) mod call_durable_metadata;
pub(crate) mod edit;
pub mod effect;
pub mod envelope_store;
pub(crate) mod executor;
pub mod guard_l1;
pub(crate) mod guard_l2;
mod handlers;
pub mod impact;
pub mod invariants;
pub mod isolation;
mod native_registry;
pub mod policy;
pub mod provenance {
    pub use lancet_audit::provenance::*;
}
pub mod registry;
pub mod testing;
pub mod tool_definitions;
pub mod transport;
pub mod verify;

pub use envelope_store::ActiveEnvelopeStore;
#[cfg(feature = "inspect-scaffold")]
pub use handlers::register_inspect_scaffold_methods;
pub use isolation::{
    probe_host_capabilities, unavailable_required_boundaries, HostProbeEntry, HostProbeReport,
    MissingRequiredBoundary,
};
pub use lancet_audit::{
    impact_kind_for_method, impact_kind_from_label, impact_kind_label, AttestationRecord,
    AuditQueryFilters, CallAuditMetadata, CepAuditContext, CepAuditStore, SEMANTIC_STAGE_NATIVE,
};
pub use lancet_dispatch::{wire_code_for_error, CepClient, CepServer, CepTransport};
pub use native_registry::register_production_method_registry;
pub use policy::{
    command::CommandPolicy, BudgetLimits, CepPolicyConfig, CepTurnContext, NetworkPolicyConfig,
    ObservationPolicyConfig,
};
pub use provenance::{
    build_in_toto_statement, canonicalize_in_toto_statement, source_audit_range_hash,
};
pub use registry::{HandlerContext, MethodRegistry};
#[cfg(feature = "inspect-scaffold")]
pub use tool_definitions::scaffold_tool_definitions;
pub use tool_definitions::to_tool_definitions;
pub use transport::{CepServerNativeExt, ServerRunConfig};

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
