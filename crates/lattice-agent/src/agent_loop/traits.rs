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
/// `Ok(content)` carries successful tool output to be appended to the
/// transcript. `Err(message)` signals a tool-level failure (denied, invalid
/// args, IO error, etc.) — the agent loop records the message as the tool
/// message content and marks the result as `ok: false`. Implementations
/// must encode success/failure structurally rather than via string prefixes
/// in the output.
#[async_trait::async_trait]
pub trait ToolCallExecutor: Send + Sync {
    /// Execute a tool call.
    ///
    /// Return `Ok(output)` on success or `Err(error)` when the tool
    /// could not produce a valid result.
    async fn execute(&self, call: &lattice_llm::ToolCall) -> Result<String, String>;
}
