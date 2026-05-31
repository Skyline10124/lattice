//! CEP server-side runtime: method registry, in-process JSON-RPC transport,
//! and native CEP method handlers.
//!
//! Per SRV-02 / SRV-04, low-level executor helpers remain `pub(crate)` and are
//! reachable only through native catalog handlers. `lattice-agent` cannot reach
//! them at compile time. CEP-13 makes [`to_tool_definitions`] the only path from
//! the catalog to LLM-facing tool schemas; the legacy `default_tool_definitions()`
//! helper was removed at the end of CEP.

pub mod audit;
pub(crate) mod call_durable_metadata;
pub(crate) mod edit;
pub mod effect;
pub mod envelope_store;
pub(crate) mod executor;
pub mod guard;
pub(crate) mod guard_l2;
mod handlers;
pub mod impact;
pub mod invariants;
mod native_registry;
pub mod policy;
pub mod provenance;
pub mod registry;
pub mod testing;
pub mod tool_definitions;
pub mod transport;
pub mod verify;

pub use audit::{
    impact_kind_for_method, impact_kind_from_label, impact_kind_label, AttestationRecord,
    AuditQueryFilters, CallAuditMetadata, CepAuditContext, CepAuditStore, SEMANTIC_STAGE_NATIVE,
};
pub use envelope_store::ActiveEnvelopeStore;
#[cfg(feature = "inspect-scaffold")]
pub use handlers::register_inspect_scaffold_methods;
pub use native_registry::register_production_method_registry;
pub use policy::{
    command::CommandPolicy, BudgetLimits, CepPolicyConfig, CepTurnContext, NetworkPolicyConfig,
    ObservationPolicyConfig,
};
pub use provenance::{
    build_in_toto_statement, canonicalize_in_toto_statement, source_audit_range_hash,
};
pub use registry::{wire_code_for_error, HandlerContext, MethodRegistry};
#[cfg(feature = "inspect-scaffold")]
pub use tool_definitions::scaffold_tool_definitions;
pub use tool_definitions::to_tool_definitions;
pub use transport::{LancetClient, LancetTransport, ServerRunConfig};

#[cfg(test)]
mod public_api_tests {
    /// ```compile_fail
    /// use cep_executor::DirectToolExecutor;
    /// let _ = DirectToolExecutor::for_project_root(".");
    /// ```
    const _DIRECT_TOOL_EXECUTOR_NOT_PUBLIC: () = ();

    /// ```compile_fail
    /// use cep_executor::ToolExecutor;
    /// ```
    const _TOOL_EXECUTOR_NOT_PUBLIC: () = ();
}
