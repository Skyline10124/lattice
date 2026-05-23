pub mod edit;
pub mod executor;
pub mod tool_definitions;

pub use edit::{
    AttemptedOperation, ChangedFile, FileOperationKind, PatchDiagnostic, PatchDocument,
    PatchErrorCode, PatchHunk, PatchHunkLine, PatchOperation, PatchResultEnvelope,
};
pub use executor::DefaultToolExecutor;
pub use tool_definitions::default_tool_definitions;

// Re-export protocol types from lattice-tooling for convenience
pub use lattice_tooling::{ToolError, ToolExecutor, ToolLimits};
