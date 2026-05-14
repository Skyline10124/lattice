use std::collections::HashMap;
use std::pin::Pin;

use futures::{Stream, StreamExt};
use lattice_core::streaming::StreamEvent;

use super::{Agent, LoopEvent};

/// Classify a mid-stream SSE error message as retryable or not.
/// Mid-stream errors lack HTTP status codes; we infer from known patterns.
fn is_retryable_stream_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("rate_limit")
        || lower.contains("overloaded")
        || lower.contains("timeout")
        || lower.contains("429")
        || lower.contains("503")
        || lower.contains("502")
        || lower.contains("server_error")
        || lower.contains("service unavailable")
        || lower.contains("temporarily")
}

impl Agent {
    pub(super) async fn run_chat(&mut self) -> Vec<LoopEvent> {
        let mut ignore_event = |_| {};
        self.run_chat_with_observer(&mut ignore_event).await
    }

    pub(super) async fn run_chat_with_observer<F>(&mut self, emit: &mut F) -> Vec<LoopEvent>
    where
        F: FnMut(LoopEvent),
    {
        let mut stream = match self.chat_with_retry().await {
            Ok(s) => s,
            Err(e) => {
                use lattice_core::errors::ErrorClassifier;
                let event = LoopEvent::Error {
                    message: e.to_string(),
                    retryable: ErrorClassifier::is_retryable(&e),
                };
                emit(event.clone());
                return vec![event];
            }
        };

        let mut events = Vec::new();
        let mut content_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut tool_builders: HashMap<String, ToolCallAccum> = HashMap::new();

        while let Some(event) = stream.next().await {
            match event {
                StreamEvent::Token { content: c } => {
                    content_buf.push_str(&c);
                    let event = LoopEvent::Token { text: c };
                    emit(event.clone());
                    events.push(event);
                }
                StreamEvent::Reasoning { content: r } => {
                    reasoning_buf.push_str(&r);
                    let event = LoopEvent::Reasoning { text: r };
                    emit(event.clone());
                    events.push(event);
                }
                StreamEvent::ToolCallStart { id, name } => {
                    tool_builders.insert(
                        id,
                        ToolCallAccum {
                            name,
                            arguments: String::new(),
                        },
                    );
                }
                StreamEvent::ToolCallDelta {
                    id,
                    arguments_delta,
                } => {
                    if let Some(tc) = tool_builders.get_mut(&id) {
                        tc.arguments.push_str(&arguments_delta);
                    }
                }
                StreamEvent::ToolCallEnd { .. } => {}
                StreamEvent::Done { usage, .. } => {
                    if let Some(ref u) = usage {
                        self.state.add_token_usage(u.total_tokens as u64);
                    }
                    if !tool_builders.is_empty() {
                        let calls: Vec<lattice_core::types::ToolCall> = tool_builders
                            .iter()
                            .map(|(id, tc)| lattice_core::types::ToolCall {
                                id: id.clone(),
                                function: lattice_core::types::FunctionCall {
                                    name: tc.name.clone(),
                                    arguments: tc.arguments.clone(),
                                },
                            })
                            .collect();
                        let event = LoopEvent::ToolCallRequired { calls };
                        emit(event.clone());
                        events.push(event);
                    }
                    let event = LoopEvent::Done { usage };
                    emit(event.clone());
                    events.push(event);
                }
                StreamEvent::Error { message } => {
                    let retryable = is_retryable_stream_message(&message);
                    let event = LoopEvent::Error { message, retryable };
                    emit(event.clone());
                    events.push(event);
                }
            }
        }

        let tool_calls = if tool_builders.is_empty() {
            None
        } else {
            Some(
                tool_builders
                    .into_iter()
                    .map(|(id, tc)| lattice_core::types::ToolCall {
                        id,
                        function: lattice_core::types::FunctionCall {
                            name: tc.name,
                            arguments: tc.arguments,
                        },
                    })
                    .collect(),
            )
        };

        self.state
            .push_assistant_message(&content_buf, &reasoning_buf, tool_calls);

        events
    }

    async fn chat_with_retry(
        &self,
    ) -> Result<Pin<Box<dyn Stream<Item = StreamEvent> + Send>>, lattice_core::LatticeError> {
        use lattice_core::errors::ErrorClassifier;
        let mut attempt = 0u32;

        loop {
            match lattice_core::chat_with_effort(
                &self.state.resolved,
                &self.state.messages,
                &self.tools,
                self.thinking_effort.as_deref(),
            )
            .await
            {
                Ok(stream) => return Ok(stream),
                Err(ref e) => {
                    if attempt >= self.retry.max_retries || !ErrorClassifier::is_retryable(e) {
                        return Err(e.clone());
                    }
                    let delay = self.retry.jittered_backoff(attempt);
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
            }
        }
    }
}

struct ToolCallAccum {
    name: String,
    arguments: String,
}
