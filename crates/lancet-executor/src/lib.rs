//! CEP server-side runtime: method registry, in-process JSON-RPC transport,
//! and adapters for the legacy in-tree tool implementations.
//!
//! Per SRV-02 / SRV-04, the legacy `DefaultToolExecutor` is **not** exported
//! to the rest of the workspace from this crate. It lives behind `pub(crate)`
//! visibility and is wrapped by the private `LegacyToolBackend` bridge so that
//! `lattice-agent` cannot reach it at compile time. CEP-13 makes
//! [`to_tool_definitions`] the only path from the catalog to LLM-facing tool
//! schemas; the legacy `default_tool_definitions()` helper was removed at the
//! end of Phase 19.

pub mod audit;
pub mod edit;
pub(crate) mod executor;
mod legacy_backend;
pub mod registry;
pub mod tool_definitions;
pub mod transport;

pub use audit::{
    impact_kind_for_method, impact_kind_label, AuditError, AuditOutcome, AuditRow, AuditStart,
    CepAuditContext, CepAuditStore, GUARD_STAGE, POLICY_ENFORCED, SEMANTIC_STAGE,
};
pub use edit::{
    AttemptedOperation, ChangedFile, FileOperationKind, PatchDiagnostic, PatchDocument,
    PatchErrorCode, PatchHunk, PatchHunkLine, PatchOperation, PatchResultEnvelope,
};
pub use legacy_backend::register_phase20_legacy_backend;
pub use registry::{
    call_cancelled, duplicate_registration, wire_code_for_error, Cancellation, EventSink,
    HandlerContext, MethodRegistry,
};
pub use tool_definitions::to_tool_definitions;
pub use transport::{CallControl, LancetClient, LancetServer, LancetTransport, ServerRunConfig};

#[cfg(test)]
mod public_api_tests {
    /// ```compile_fail
    /// use lancet_executor::DefaultToolExecutor;
    /// let _ = DefaultToolExecutor::for_project_root(".", ["read_file"]);
    /// ```
    const _DEFAULT_TOOL_EXECUTOR_NOT_PUBLIC: () = ();

    /// ```compile_fail
    /// use lancet_executor::ToolExecutor;
    /// ```
    const _TOOL_EXECUTOR_NOT_PUBLIC: () = ();
}
