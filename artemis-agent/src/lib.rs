pub mod state;

use std::collections::HashMap;
use std::sync::LazyLock;

use artemis_core::retry::RetryPolicy;
use artemis_core::streaming::StreamEvent;
use artemis_core::types::ToolDefinition;
use artemis_core::ResolvedModel;

/// Global tokio runtime shared by all Agent instances.
static SHARED_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("Failed to create shared tokio runtime")
});

#[allow(dead_code)]
pub struct Agent {
    resolved: ResolvedModel,
    state: state::AgentState,
    tools: Vec<ToolDefinition>,
    retry: RetryPolicy,
    memory: Option<Box<dyn artemis_memory::Memory>>,
    token_pool: Option<Box<dyn artemis_token_pool::TokenPool>>,
}

impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self {
        // Force lazy init so first Agent creation pays the cost, not first send().
        LazyLock::force(&SHARED_RUNTIME);
        Self {
            resolved: resolved.clone(),
            state: state::AgentState::new(resolved),
            tools: vec![],
            retry: RetryPolicy::default(),
            memory: None,
            token_pool: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    pub fn with_memory(mut self, memory: Box<dyn artemis_memory::Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_token_pool(mut self, pool: Box<dyn artemis_token_pool::TokenPool>) -> Self {
        self.token_pool = Some(pool);
        self
    }

    /// Send a user message, returning streaming events.
    /// Each event is either a Token, ToolCallRequired, Done, or Error.
    pub fn send(&mut self, content: &str) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        self.run_chat()
    }

    /// Submit tool call results, continue the conversation.
    /// `max_size` optionally limits the byte size of each tool result
    /// (default: 1 MB). Larger results are truncated with a note.
    pub fn submit_tools(
        &mut self,
        results: Vec<(String, String)>,
        max_size: Option<usize>,
    ) -> Vec<LoopEvent> {
        for (call_id, result) in &results {
            self.state.push_tool_result(call_id, result, max_size);
        }
        self.run_chat()
    }

    /// Internal: call artemis_core::chat() with the current conversation state,
    /// consume the stream, update state, and return LoopEvents.
    fn run_chat(&mut self) -> Vec<LoopEvent> {
        use futures::StreamExt;

        let stream_result = self.chat_with_retry();

        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                return vec![LoopEvent::Error {
                    message: e.to_string(),
                }]
            }
        };

        let mut events = Vec::new();
        let mut content_buf = String::new();
        let mut reasoning_buf = String::new();
        let mut tool_builders: HashMap<String, ToolCallAccum> = HashMap::new();

        SHARED_RUNTIME.block_on(async {
            while let Some(event) = stream.next().await {
                match event {
                    StreamEvent::Token { content: c } => {
                        content_buf.push_str(&c);
                        events.push(LoopEvent::Token { text: c });
                    }
                    StreamEvent::Reasoning { content: r } => {
                        reasoning_buf.push_str(&r);
                        events.push(LoopEvent::Reasoning { text: r });
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
                    StreamEvent::ToolCallEnd { .. } => {
                        // Tool-call argument stream ends; already accumulated.
                    }
                    StreamEvent::Done { usage, .. } => {
                        if !tool_builders.is_empty() {
                            let calls: Vec<artemis_core::types::ToolCall> = tool_builders
                                .iter()
                                .map(|(id, tc)| artemis_core::types::ToolCall {
                                    id: id.clone(),
                                    function: artemis_core::types::FunctionCall {
                                        name: tc.name.clone(),
                                        arguments: tc.arguments.clone(),
                                    },
                                })
                                .collect();
                            events.push(LoopEvent::ToolCallRequired { calls });
                        }
                        events.push(LoopEvent::Done { usage });
                    }
                    StreamEvent::Error { message } => {
                        events.push(LoopEvent::Error { message });
                    }
                }
            }
        });

        // Build assistant message and push to conversation state.
        let tool_calls = if tool_builders.is_empty() {
            None
        } else {
            Some(
                tool_builders
                    .into_iter()
                    .map(|(id, tc)| artemis_core::types::ToolCall {
                        id,
                        function: artemis_core::types::FunctionCall {
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

    /// Call chat() with retry logic. Retries only on retryable errors.
    fn chat_with_retry(
        &self,
    ) -> Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = StreamEvent> + Send>>,
        artemis_core::ArtemisError,
    > {
        use artemis_core::errors::ErrorClassifier;
        let mut attempt = 0u32;

        loop {
            let result = SHARED_RUNTIME.block_on(artemis_core::chat(
                &self.state.resolved,
                &self.state.messages,
                &self.tools,
            ));

            match result {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    if attempt >= self.retry.max_retries || !ErrorClassifier::is_retryable(&e) {
                        return Err(e);
                    }
                    let delay = self.retry.jittered_backoff(attempt);
                    SHARED_RUNTIME.block_on(async {
                        tokio::time::sleep(delay).await;
                    });
                    attempt += 1;
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub enum LoopEvent {
    Token {
        text: String,
    },
    /// A chunk of reasoning/thinking content (e.g., DeepSeek thinking chain).
    Reasoning {
        text: String,
    },
    ToolCallRequired {
        calls: Vec<artemis_core::types::ToolCall>,
    },
    Done {
        usage: Option<artemis_core::streaming::TokenUsage>,
    },
    Error {
        message: String,
    },
}

/// Internal helper for accumulating tool call data during streaming.
struct ToolCallAccum {
    name: String,
    arguments: String,
}
