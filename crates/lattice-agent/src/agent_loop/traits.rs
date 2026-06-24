// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_llm::ToolCall;

/// Structured CEP error metadata preserved by the agent loop for API/CLI observation.
///
/// Plain data fields only — no imports from `lancet-protocol` error enums.
#[derive(Debug, Clone, PartialEq)]
pub struct CepErrorPayload {
    pub code: i32,
    pub message: String,
    pub data: serde_json::Value,
    pub retriability: String,
}

/// Tool execution failure with optional structured CEP metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolExecutionError {
    pub message: String,
    pub cep_error: Option<CepErrorPayload>,
}

impl ToolExecutionError {
    pub fn simple(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cep_error: None,
        }
    }
}

impl From<String> for ToolExecutionError {
    fn from(message: String) -> Self {
        Self::simple(message)
    }
}

/// Injectable tool executor for the agent loop.
///
/// Production implementations dispatch through the API-layer CEP adapter.
/// Test implementations return deterministic fake responses.
#[async_trait::async_trait]
pub trait ToolCallExecutor: Send + Sync {
    async fn execute(&self, call: &ToolCall) -> Result<String, ToolExecutionError>;
}

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

        if let Some(reasoning) = &response.reasoning_content
            && !reasoning.is_empty()
        {
            events.push(lattice_llm::LlmEvent::Reasoning {
                content: reasoning.clone(),
            });
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

/// Decision returned by a provisional completion hook.
///
/// - `Continue` supplies a system message that the loop appends before
///   requesting the next LLM round. The same loop iteration continues
///   without resetting messages, usage, round count, deadline, or
///   cancellation state.
/// - `Complete` terminates the loop. `cleanup_exhausted` signals whether
///   the hook reached its configured cleanup-round budget.
#[derive(Debug, Clone)]
pub enum ProvisionalDecision {
    /// Continue the loop with the supplied workspace fact message.
    Continue(lattice_session::GraphMessage),
    /// Terminate the loop with an explicit exhaustion fact.
    Complete {
        cleanup_exhausted: bool,
        /// Optional terminal fact appended to the transcript on completion
        /// the loop appends it to `all_messages` before returning.
        final_message: Option<lattice_session::GraphMessage>,
    },
}

/// Injectable hook invoked at the no-tool final-response branch of the
/// agent loop.
///
/// The hook receives the provisional assistant message and decides
/// whether to continue the same loop iteration with a system fact or
/// to complete the turn. Errors propagate as terminal agent errors.
///
/// The trait is workspace-agnostic: it uses only `GraphMessage` from
/// `lattice-session` and carries no workspace, Git, snapshot, or
/// disposition concepts.
#[async_trait::async_trait]
pub trait ProvisionalCompletionHook: Send {
    async fn on_provisional_complete(
        &mut self,
        assistant: &lattice_session::GraphMessage,
    ) -> Result<ProvisionalDecision, String>;
}
