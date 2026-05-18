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

/// Injectable tool executor for the agent loop.
///
/// Wraps the existing `DefaultToolExecutor` or a test fake.
#[async_trait::async_trait]
pub trait ToolCallExecutor: Send + Sync {
    /// Execute a tool call and return the output string.
    async fn execute(&self, call: &lattice_llm::ToolCall) -> String;
}
