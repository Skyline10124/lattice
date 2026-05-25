//! Production LLM client wired into API-owned turn execution.

/// Production LlmClient that delegates to `lattice_llm::chat` / `stream`.
pub(crate) struct RealLlmClient;

#[async_trait::async_trait]
impl lattice_agent::LlmClient for RealLlmClient {
    async fn chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>>
    {
        lattice_llm::chat(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
    }

    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> std::result::Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        lattice_llm::stream(request)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
    }
}
