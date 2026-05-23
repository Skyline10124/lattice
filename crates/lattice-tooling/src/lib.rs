pub mod enabled_tools;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLimits {
    pub max_read_size: usize,
    pub max_write_size: usize,
    pub max_http_response_size: usize,
    pub max_command_timeout: u32,
}

impl Default for ToolLimits {
    fn default() -> Self {
        Self {
            max_read_size: 10 * 1024 * 1024,
            max_write_size: 1024 * 1024,
            max_http_response_size: 10 * 1024 * 1024,
            max_command_timeout: 30,
        }
    }
}

pub use enabled_tools::{resolve_enabled_tools, EnabledToolError, EnabledTools};
pub use tool_error::ToolError;
