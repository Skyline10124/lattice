//! CEP server-side runtime: method registry, in-process JSON-RPC transport,
//! and adapters for the legacy in-tree tool implementations.
//!
//! Per SRV-02 / SRV-04, the legacy `DefaultToolExecutor` is **not** exported
//! to the rest of the workspace from this crate. It lives behind `pub(crate)`
//! visibility and will be wrapped by `LegacyToolBackend` (Phase 20) so that
//! `lattice-agent` cannot reach it at compile time. CEP-13 makes
//! [`to_tool_definitions`] the only path from the catalog to LLM-facing tool
//! schemas; the legacy `default_tool_definitions()` helper was removed at the
//! end of Phase 19.

pub mod edit;
pub(crate) mod executor;
pub mod registry;
pub mod tool_definitions;
pub mod transport;

pub use edit::{
    AttemptedOperation, ChangedFile, FileOperationKind, PatchDiagnostic, PatchDocument,
    PatchErrorCode, PatchHunk, PatchHunkLine, PatchOperation, PatchResultEnvelope,
};
// `DefaultToolExecutor` is intentionally **not** re-exported. Per SRV-02 it
// lives behind `pub(crate)` module visibility (`executor` is `pub(crate)`),
// so in-crate adapters reach it via `crate::executor::DefaultToolExecutor`
// while everything outside this crate cannot import it. Phase 20's
// `LegacyToolBackend` is the only wrapper that exposes the legacy executor
// behind CEP method handlers.
pub use registry::{
    call_cancelled, duplicate_registration, wire_code_for_error, Cancellation, EventSink,
    HandlerContext, MethodRegistry,
};
pub use tool_definitions::to_tool_definitions;
pub use transport::{CallControl, LancetClient, LancetServer, LancetTransport};

// Protocol surface re-exports kept for the few in-crate adapters that still
// reach into `lattice-tooling`. After Phase 20 (`LegacyToolBackend`) lands,
// these re-exports are pruned to whatever the adapter actually needs.
pub use lattice_tooling::{ToolError, ToolExecutor, ToolLimits};
