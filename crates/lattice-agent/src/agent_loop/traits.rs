/// Injectable LLM client for the agent loop.
///
/// Production implementations call real providers. Test implementations
/// return deterministic fake responses.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Execute a chat completion.
    async fn chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>;
}

/// Re-export of the unified tool executor trait.
///
/// The agent loop drives any implementation of `lattice_tools::ToolExecutor`
/// to handle tool calls. This re-export lets agent-layer callers depend on
/// a single trait without importing `lattice-tools` directly.
///
/// Implementations return `Ok(content)` on success or `Err(message)` when
/// the tool could not produce a valid result. The agent loop derives the
/// `ok` flag and error summary from the Result variant.
pub use lattice_tools::ToolExecutor as ToolCallExecutor;
