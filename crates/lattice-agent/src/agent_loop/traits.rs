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

    /// Execute a chat completion as a stream.
    ///
    /// This default implementation is a single-blob fallback over [`Self::chat`]:
    /// startup or dispatch failures propagate through `Result::Err`, while
    /// provider implementations can still use in-band `LlmEvent::Error` values
    /// for failures that happen after a stream has opened.
    ///
    /// Real provider streaming is wired later by overriding this method on the
    /// concrete client. The provider-level stream-open event is intentionally
    /// not synthesized by this buffered fallback.
    async fn stream_chat(
        &self,
        request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        let response = self.chat(request).await?;
        let mut events: Vec<lattice_llm::LlmEvent> = Vec::new();

        if let Some(reasoning) = &response.reasoning_content {
            if !reasoning.is_empty() {
                events.push(lattice_llm::LlmEvent::Reasoning {
                    content: reasoning.clone(),
                });
            }
        }

        if !response.content.is_empty() {
            events.push(lattice_llm::LlmEvent::Token {
                content: response.content.clone(),
            });
        }

        if let Some(tool_calls) = &response.tool_calls {
            for tool_call in tool_calls {
                events.push(lattice_llm::LlmEvent::ToolCallStart {
                    id: tool_call.id.clone(),
                    name: tool_call.function.name.clone(),
                });

                if !tool_call.function.arguments.is_empty() {
                    events.push(lattice_llm::LlmEvent::ToolCallDelta {
                        id: tool_call.id.clone(),
                        arguments_delta: tool_call.function.arguments.clone(),
                    });
                }

                events.push(lattice_llm::LlmEvent::ToolCallEnd {
                    id: tool_call.id.clone(),
                });
            }
        }

        events.push(lattice_llm::LlmEvent::Done {
            finish_reason: response.finish_reason.clone(),
            usage: response.usage.clone(),
        });

        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// Re-export of the unified tool executor trait.
///
/// The agent loop drives any implementation of `lattice_tooling::ToolExecutor`
/// to handle tool calls. This re-export lets agent-layer callers depend on
/// a single trait without importing `lattice-tooling` directly.
///
/// Implementations return `Ok(content)` on success or `Err(message)` when
/// the tool could not produce a valid result. The agent loop derives the
/// `ok` flag and error summary from the Result variant.
pub use lattice_tooling::ToolExecutor as ToolCallExecutor;
