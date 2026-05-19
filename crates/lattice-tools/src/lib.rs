pub mod edit;
pub mod enabled_tools;
pub mod executor;
pub mod tool_definitions;
pub mod tool_error;

use lattice_llm::ToolCall;

/// Executes a tool call.
///
/// Returns `Ok(output)` on success or `Err(message)` when the tool could
/// not produce a valid result (denied, invalid args, IO error, etc.).
/// Implementations encode success/failure structurally in the Result
/// variant — never via prefixes in the output string.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> Result<String, String>;
}

pub use edit::{
    AttemptedOperation, ChangedFile, FileOperationKind, PatchDiagnostic, PatchDocument,
    PatchErrorCode, PatchHunk, PatchHunkLine, PatchOperation, PatchResultEnvelope,
};
pub use enabled_tools::{resolve_enabled_tools, EnabledToolError, EnabledTools};
pub use executor::{DefaultToolExecutor, ToolLimits};
pub use tool_definitions::default_tool_definitions;
