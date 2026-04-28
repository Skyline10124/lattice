use crate::catalog::ResolvedModel;
use crate::provider::{ChatRequest, Provider};
use crate::tokens::TokenEstimator;
use crate::types::{Message, Role, ToolCall, ToolDefinition};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone)]
pub struct LoopConfig {
    pub max_iterations: u32,
    pub budget_tokens: u32,
}

impl Default for LoopConfig {
    fn default() -> Self {
        LoopConfig {
            max_iterations: 10,
            budget_tokens: 100_000,
        }
    }
}

fn trim_conversation(
    messages: &[Message],
    resolved: &ResolvedModel,
    budget_tokens: u32,
) -> Vec<Message> {
    let token_count = TokenEstimator::estimate_messages_for_model(messages, &resolved.api_model_id);
    if token_count <= budget_tokens {
        return messages.to_vec();
    }

    let system_msgs: Vec<Message> = messages
        .iter()
        .filter(|m| matches!(m.role, Role::System))
        .cloned()
        .collect();

    let mut non_system_msgs: Vec<Message> = messages
        .iter()
        .filter(|m| !matches!(m.role, Role::System))
        .cloned()
        .collect();

    const MIN_KEEP: usize = 4;

    if non_system_msgs.len() <= MIN_KEEP {
        return messages.to_vec();
    }

    loop {
        let mut current = system_msgs.clone();
        current.extend(non_system_msgs.clone());
        let current_tokens =
            TokenEstimator::estimate_messages_for_model(&current, &resolved.api_model_id);

        if current_tokens <= budget_tokens || non_system_msgs.len() <= MIN_KEEP {
            return current;
        }

        non_system_msgs.remove(0);
    }
}

#[derive(Clone)]
pub enum LoopEvent {
    Token {
        content: String,
    },
    ToolCallRequired {
        tool_calls: Vec<ToolCall>,
    },
    Done {
        finish_reason: String,
        final_message: Message,
    },
    Error {
        message: String,
    },
    Interrupted,
}

pub struct AgentLoop {
    pub interrupted: Arc<AtomicBool>,
    runtime: tokio::runtime::Runtime,
}

impl Default for AgentLoop {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentLoop {
    pub fn new() -> Self {
        AgentLoop {
            interrupted: Arc::new(AtomicBool::new(false)),
            runtime: tokio::runtime::Runtime::new().expect("Failed to create tokio runtime"),
        }
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }

    pub fn run(
        &self,
        provider: &dyn Provider,
        resolved: ResolvedModel,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        config: LoopConfig,
    ) -> Vec<LoopEvent> {
        let mut events = Vec::new();
        let mut conversation = messages;
        let mut iteration = 0;

        loop {
            if self.interrupted.load(Ordering::SeqCst) {
                events.push(LoopEvent::Interrupted);
                break;
            }
            if iteration >= config.max_iterations {
                events.push(LoopEvent::Done {
                    finish_reason: "max_iterations".into(),
                    final_message: Message {
                        role: Role::Assistant,
                        content: String::new(),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    },
                });
                break;
            }

            conversation = trim_conversation(&conversation, &resolved, config.budget_tokens);

            let request = ChatRequest::new(conversation.clone(), tools.clone(), resolved.clone());

            let response = self.runtime.block_on(provider.chat(request));

            match response {
                Ok(resp) => {
                    if let Some(tool_calls) = &resp.tool_calls {
                        if !tool_calls.is_empty() {
                            events.push(LoopEvent::ToolCallRequired {
                                tool_calls: tool_calls.clone(),
                            });
                            for tc in tool_calls {
                                conversation.push(Message {
                                    role: Role::Assistant,
                                    content: String::new(),
                                    tool_calls: Some(vec![tc.clone()]),
                                    tool_call_id: None,
                                    name: None,
                                });
                                conversation.push(Message {
                                    role: Role::Tool,
                                    content: "mock tool result".to_string(),
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                    name: None,
                                });
                            }
                        } else {
                            events.push(LoopEvent::Token {
                                content: resp.content.clone().unwrap_or_default(),
                            });
                            events.push(LoopEvent::Done {
                                finish_reason: resp.finish_reason.clone(),
                                final_message: Message {
                                    role: Role::Assistant,
                                    content: resp.content.unwrap_or_default(),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    name: None,
                                },
                            });
                            break;
                        }
                    } else if let Some(content) = &resp.content {
                        events.push(LoopEvent::Token {
                            content: content.clone(),
                        });
                        events.push(LoopEvent::Done {
                            finish_reason: resp.finish_reason.clone(),
                            final_message: Message {
                                role: Role::Assistant,
                                content: content.clone(),
                                tool_calls: None,
                                tool_call_id: None,
                                name: None,
                            },
                        });
                        break;
                    }
                }
                Err(e) => {
                    events.push(LoopEvent::Error {
                        message: format!("{}", e),
                    });
                    break;
                }
            }
            iteration += 1;
        }
        events
    }

    /// Continue a conversation with real tool results instead of mock ones.
    ///
    /// Appends `Role::Tool` messages for each provided result, then
    /// resumes the conversation loop. Any *further* tool calls the model
    /// makes during the continuation will fall back to the hardcoded
    /// `"mock tool result"` string.
    #[allow(clippy::too_many_arguments)]
    pub fn resume_with_tool_results(
        &self,
        provider: &dyn Provider,
        resolved: ResolvedModel,
        mut messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        config: LoopConfig,
        results: Vec<(String, String)>,
    ) -> Vec<LoopEvent> {
        for (tool_call_id, result) in results {
            messages.push(Message {
                role: Role::Tool,
                content: result,
                tool_calls: None,
                tool_call_id: Some(tool_call_id),
                name: None,
            });
        }
        self.run(provider, resolved, messages, tools, config)
    }

    /// Try providers in priority order, using fallback on failure.
    #[allow(clippy::too_many_arguments)]
    pub fn run_with_fallback(
        &self,
        providers: Vec<&dyn Provider>,
        resolved: ResolvedModel,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        config: LoopConfig,
        _classifier: &crate::errors::ErrorClassifier,
        policy: &crate::retry::RetryPolicy,
    ) -> Vec<LoopEvent> {
        let mut last_error: Option<String> = None;

        for (i, provider) in providers.iter().enumerate() {
            if self.interrupted.load(Ordering::SeqCst) {
                return vec![LoopEvent::Interrupted];
            }

            let events = self.run(
                *provider,
                resolved.clone(),
                messages.clone(),
                tools.clone(),
                config.clone(),
            );

            let has_error = events.iter().any(|e| matches!(e, LoopEvent::Error { .. }));
            if !has_error {
                return events;
            }

            if let Some(LoopEvent::Error { message }) = events.last() {
                last_error = Some(message.clone());
            }

            if i < providers.len() - 1 {
                let duration = policy.jittered_backoff(i as u32);
                self.runtime.block_on(async {
                    tokio::time::sleep(duration).await;
                });
            }
        }

        vec![LoopEvent::Error {
            message: format!("All providers exhausted: {:?}", last_error),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ApiProtocol;
    use crate::mock::MockProvider;
    use crate::types::FunctionCall;
    use std::collections::HashMap;

    fn make_resolved() -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test-model".to_string(),
            provider: "mock".to_string(),
            api_key: None,
            base_url: "http://localhost".to_string(),
            api_protocol: ApiProtocol::OpenAiChat,
            api_model_id: "test-model".to_string(),
            context_length: 131072,
            provider_specific: HashMap::new(),
        }
    }

    #[test]
    fn test_simple_conversation_no_tools() {
        let mut provider = MockProvider::new("mock");
        provider.set_response("Hello, world!");
        let agent = AgentLoop::new();
        let messages = vec![Message {
            role: Role::User,
            content: "hi".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let events = agent.run(
            &provider,
            make_resolved(),
            messages,
            vec![],
            LoopConfig::default(),
        );
        assert!(events.iter().any(|e| matches!(e, LoopEvent::Done { .. })));
    }

    #[test]
    fn test_interrupt() {
        let mut provider = MockProvider::new("mock");
        provider.set_response("Hello!");
        let agent = AgentLoop::new();
        agent.interrupt();
        let events = agent.run(
            &provider,
            make_resolved(),
            vec![],
            vec![],
            LoopConfig::default(),
        );
        assert!(events.iter().any(|e| matches!(e, LoopEvent::Interrupted)));
    }

    #[test]
    fn test_resume_with_tool_results_single() {
        // First call returns tool calls, second call returns final answer.
        let provider = MockProvider::new("mock")
            .with_first_content("Let me check.")
            .with_first_tool_calls(vec![ToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "get_weather".to_string(),
                    arguments: r#"{"city":"Paris"}"#.to_string(),
                },
            }])
            .with_final_content("Paris is sunny and 22°C.");
        let agent = AgentLoop::new();

        // Simulate: user sends message, assistant responds with tool call
        let messages = vec![
            Message {
                role: Role::User,
                content: "What's the weather in Paris?".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".to_string(),
                    function: FunctionCall {
                        name: "get_weather".to_string(),
                        arguments: r#"{"city":"Paris"}"#.to_string(),
                    },
                }]),
                tool_call_id: None,
                name: None,
            },
        ];

        let events = agent.resume_with_tool_results(
            &provider,
            make_resolved(),
            messages,
            vec![],
            LoopConfig::default(),
            vec![("call_1".to_string(), "Sunny, 22°C".to_string())],
        );

        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
            "should emit Done after processing real tool result"
        );

        if let Some(LoopEvent::Done { final_message, .. }) =
            events.iter().find(|e| matches!(e, LoopEvent::Done { .. }))
        {
            assert_eq!(
                final_message.content, "Paris is sunny and 22°C.",
                "final_message should contain the provider's response to real tool results"
            );
        }
    }

    #[test]
    fn test_resume_with_tool_results_multiple() {
        let provider = MockProvider::new("mock")
            .with_first_content("Checking multiple things.")
            .with_first_tool_calls(vec![
                ToolCall {
                    id: "call_search".to_string(),
                    function: FunctionCall {
                        name: "search".to_string(),
                        arguments: r#"{"query":"rust"}"#.to_string(),
                    },
                },
                ToolCall {
                    id: "call_calc".to_string(),
                    function: FunctionCall {
                        name: "calculate".to_string(),
                        arguments: r#"{"expr":"2+2"}"#.to_string(),
                    },
                },
            ])
            .with_final_content("Combined results: 3 articles found, 2+2=4.");
        let agent = AgentLoop::new();

        let messages = vec![
            Message {
                role: Role::User,
                content: "Search for rust and calculate 2+2".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: Some(vec![
                    ToolCall {
                        id: "call_search".to_string(),
                        function: FunctionCall {
                            name: "search".to_string(),
                            arguments: r#"{"query":"rust"}"#.to_string(),
                        },
                    },
                    ToolCall {
                        id: "call_calc".to_string(),
                        function: FunctionCall {
                            name: "calculate".to_string(),
                            arguments: r#"{"expr":"2+2"}"#.to_string(),
                        },
                    },
                ]),
                tool_call_id: None,
                name: None,
            },
        ];

        let events = agent.resume_with_tool_results(
            &provider,
            make_resolved(),
            messages,
            vec![],
            LoopConfig::default(),
            vec![
                (
                    "call_search".to_string(),
                    "Found 3 articles about Rust".to_string(),
                ),
                ("call_calc".to_string(), "4".to_string()),
            ],
        );

        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
            "should emit Done after processing multiple real tool results"
        );

        if let Some(LoopEvent::Done { final_message, .. }) =
            events.iter().find(|e| matches!(e, LoopEvent::Done { .. }))
        {
            assert_eq!(
                final_message.content, "Combined results: 3 articles found, 2+2=4.",
                "final_message should contain final response based on real tool results"
            );
        }
    }

    #[test]
    fn test_run_still_uses_mock_fallback() {
        let provider = MockProvider::new("mock")
            .with_first_content("Need to search.")
            .with_first_tool_calls(vec![ToolCall {
                id: "call_search".to_string(),
                function: FunctionCall {
                    name: "search".to_string(),
                    arguments: r#"{"q":"rust"}"#.to_string(),
                },
            }])
            .with_final_content("Search results: ...");
        let agent = AgentLoop::new();

        let events = agent.run(
            &provider,
            make_resolved(),
            vec![Message {
                role: Role::User,
                content: "Search for rust".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            vec![],
            LoopConfig::default(),
        );

        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
            "run() still auto-completes tool calls with mock fallback"
        );
    }

    #[test]
    fn test_trim_conversation_preserves_system_and_recent() {
        let resolved = make_resolved();
        let long_text = "a".repeat(500);
        let short1 = "recent one";
        let short2 = "recent two";
        let short3 = "recent three";
        let short4 = "recent four";

        let messages = vec![
            Message {
                role: Role::System,
                content: "You are helpful.".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: long_text.clone(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::Assistant,
                content: long_text.clone(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: long_text.clone(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::Assistant,
                content: long_text.clone(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: short1.into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::Assistant,
                content: short2.into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: short3.into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::Assistant,
                content: short4.into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        let trimmed = trim_conversation(&messages, &resolved, 30);

        assert!(
            matches!(trimmed[0].role, Role::System),
            "system message should be preserved at front"
        );
        assert!(
            trimmed.len() < messages.len(),
            "some messages should be trimmed"
        );

        let contents: Vec<&str> = trimmed.iter().map(|m| m.content.as_str()).collect();

        assert!(
            contents.contains(&short1),
            "recent messages should be preserved"
        );
        assert!(
            contents.contains(&short4),
            "most recent message should be preserved"
        );

        assert!(
            !contents.contains(&long_text.as_str()),
            "old long messages should be trimmed away"
        );
    }

    #[test]
    fn test_trim_conversation_under_budget_no_trim() {
        let resolved = make_resolved();
        let messages = vec![
            Message {
                role: Role::User,
                content: "hi".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::Assistant,
                content: "hello".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        let trimmed = trim_conversation(&messages, &resolved, 100_000);
        assert_eq!(trimmed.len(), 2);
        assert_eq!(trimmed[0].content, "hi");
        assert_eq!(trimmed[1].content, "hello");
    }

    #[test]
    fn test_run_with_tight_budget_completes() {
        let mut provider = MockProvider::new("mock");
        provider.set_response("Short reply.");

        let agent = AgentLoop::new();
        let long_msg = "x".repeat(500);

        let messages = vec![
            Message {
                role: Role::System,
                content: "You are helpful.".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: long_msg,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
            Message {
                role: Role::User,
                content: "hi".into(),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ];

        let config = LoopConfig {
            max_iterations: 3,
            budget_tokens: 20,
        };

        let events = agent.run(&provider, make_resolved(), messages, vec![], config);

        assert!(
            events.iter().any(|e| matches!(e, LoopEvent::Done { .. })),
            "should emit Done even with tight budget"
        );
        assert!(
            !events.iter().any(|e| matches!(e, LoopEvent::Error { .. })),
            "should not error due to budget trimming"
        );
    }
}
