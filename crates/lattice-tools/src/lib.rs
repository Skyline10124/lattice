pub mod capabilities;
pub mod edit;
pub mod enabled_tools;
pub mod executor;
pub mod tool_definitions;
pub mod tool_error;
pub mod tool_registry;

use lattice_core::types::ToolCall;
use lattice_sandbox::DenialReport;

/// Typed result of executing a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// Normal tool output rendered back to the model.
    Text(String),
    /// Structured policy denial. The agent can enrich this without parsing
    /// arbitrary tool output strings.
    Denied(DenialReport),
}

impl ToolOutcome {
    pub fn text(output: impl Into<String>) -> Self {
        Self::Text(output.into())
    }

    pub fn denied(report: DenialReport) -> Self {
        Self::Denied(report)
    }

    pub fn from_text(output: String) -> Self {
        Self::Text(output)
    }

    pub fn into_text(self) -> String {
        match self {
            Self::Text(output) => output,
            Self::Denied(report) => report.to_json(),
        }
    }
}

/// Executes a tool call and returns the result string.
#[async_trait::async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> String;

    async fn execute_outcome(&self, call: &ToolCall) -> ToolOutcome {
        ToolOutcome::from_text(self.execute(call).await)
    }
}

/// Trait for blob store access (used by bus:fetch tool).
/// Abstracts the concrete BlobStore from lattice-agent to avoid circular deps.
#[async_trait::async_trait]
pub trait BlobFetcher: Send + Sync {
    async fn fetch(&self, key: &str) -> Result<String, String>;
}

pub use edit::{
    AttemptedOperation, ChangedFile, FileOperationKind, PatchDiagnostic, PatchDocument,
    PatchErrorCode, PatchHunk, PatchHunkLine, PatchOperation, PatchResultEnvelope,
};
pub use executor::DefaultToolExecutor;
pub use executor::RegistryToolAccess;
pub use lattice_sandbox::SandboxConfig;
pub use tool_definitions::default_tool_definitions;

pub use capabilities::{
    resolve_capabilities, resolve_tool_names, CapabilitySelection, ResolvedCapabilities, ToolBundle,
};
pub use enabled_tools::{resolve_enabled_tools, EnabledToolError, EnabledTools};
