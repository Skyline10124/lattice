pub mod edit;
pub mod enabled_tools;
pub mod executor;
pub mod tool_definitions;
pub mod tool_error;

use lattice_llm::ToolCall;

/// Executes a tool call and returns the result string.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> String;
}

pub use edit::{
    AttemptedOperation, ChangedFile, FileOperationKind, PatchDiagnostic, PatchDocument,
    PatchErrorCode, PatchHunk, PatchHunkLine, PatchOperation, PatchResultEnvelope,
};
pub use enabled_tools::{resolve_enabled_tools, EnabledToolError, EnabledTools};
pub use executor::{DefaultToolExecutor, ToolLimits};
pub use tool_definitions::default_tool_definitions;
