pub mod sandbox;
pub mod state;
pub mod tools;

use std::collections::HashMap;
use std::sync::LazyLock;

use artemis_core::retry::RetryPolicy;
use artemis_core::streaming::StreamEvent;
use artemis_core::types::ToolDefinition;
use artemis_core::ResolvedModel;
use tokio::runtime::Handle;

/// Run an async task, safely handling both runtime contexts.
fn run_async<F, T>(f: F) -> T
where
    F: futures::Future<Output = T>,
{
    if let Ok(_handle) = Handle::try_current() {
        tokio::task::block_in_place(|| SHARED_RUNTIME.block_on(f))
    } else {
        SHARED_RUNTIME.block_on(f)
    }
}

/// Global tokio runtime shared by all Agent instances.
static SHARED_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("Failed to create shared tokio runtime")
});

/// Executes a tool call and returns the result string.
/// The Agent calls this when the model requests a tool execution.
pub trait ToolExecutor: Send + Sync {
    fn execute(&self, call: &artemis_core::types::ToolCall) -> String;
}

/// Dispatches a sub-agent by name with the given input.
/// This is how `agent_call:security-audit` becomes a tool call:
/// the tool executor detects the `agent_call:` prefix, extracts the agent
/// name, and delegates to this dispatcher to run the agent and return output.
pub trait AgentDispatcher: Send + Sync {
    /// Run a named sub-agent with the given input. Returns the output text.
    fn dispatch(&self, agent_name: &str, input: &str) -> String;
}

// Re-export shared default tools and sandbox for convenience.
pub use sandbox::SandboxConfig;
pub use tools::{default_tool_definitions, DefaultToolExecutor};

#[allow(dead_code)]
pub struct Agent {
    resolved: ResolvedModel,
    state: state::AgentState,
    tools: Vec<ToolDefinition>,
    retry: RetryPolicy,
    memory: Option<Box<dyn artemis_memory::Memory>>,
    token_pool: Option<Box<dyn artemis_token_pool::TokenPool>>,
    tool_executor: Option<Box<dyn ToolExecutor>>,
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
            tool_executor: None,
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

    pub fn with_tool_executor(mut self, executor: Box<dyn ToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    /// Return the cumulative token usage across all turns so far.
    pub fn token_usage(&self) -> u64 {
        self.state.token_usage
    }

    /// Send a user message, returning streaming events.
    /// Each event is either a Token, ToolCallRequired, Done, or Error.
    pub fn send_message(&mut self, content: &str) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        self.run_chat()
    }

    /// Async variant — use this from within a tokio runtime to avoid
    /// the `block_in_place` + `block_on` nesting that can hang.
    pub async fn send_message_async(&mut self, content: &str) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        self.run_chat_async().await
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

    /// Run a message through the Agent, handling tool calls automatically.
    /// If a ToolExecutor is registered, tools are executed and results submitted
    /// in a loop until the model produces a final answer or `max_turns` is reached.
    pub fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        let mut all_events = Vec::new();
        const MAX_STREAM_RETRIES: u32 = 2;

        for _ in 0..max_turns {
            // Trim old messages to stay within the model's context window.
            // Some catalog entries have context_length=0; fall back to 128k.
            let context_len = if self.state.resolved.context_length > 0 {
                self.state.resolved.context_length
            } else {
                131072
            };
            self.state.trim_messages(context_len, 15); // 15% safety margin

            let mut events = self.run_chat();

            // Retry on mid-stream errors (up to MAX_STREAM_RETRIES).
            let mut retry_count = 0u32;
            while retry_count < MAX_STREAM_RETRIES {
                let has_only_errors = events.iter().all(|e| matches!(e, LoopEvent::Error { .. }));
                let has_error = events.iter().any(|e| matches!(e, LoopEvent::Error { .. }));
                if !has_error || !has_only_errors {
                    break;
                }
                retry_count += 1;
                events = self.run_chat();
            }

            let mut tool_calls = Vec::new();

            for event in &events {
                if let LoopEvent::ToolCallRequired { calls } = event {
                    tool_calls.extend(calls.clone());
                }
            }

            all_events.extend(events);

            if tool_calls.is_empty() {
                break;
            }

            // If the model requested tools but no executor is registered,
            // we can't make progress — stop to avoid burning tokens in a loop.
            if self.tool_executor.is_none() {
                break;
            }

            if let Some(ref executor) = self.tool_executor {
                for call in &tool_calls {
                    let result = executor.execute(call);
                    self.state.push_tool_result(&call.id, &result, None);
                }
            }
        }

        // --- Auto-save memory entry ---
        if let Some(ref memory) = self.memory {
            let prompt_summary = if content.len() > 200 {
                format!("{}...", &content[..200])
            } else {
                content.to_string()
            };
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let entry = artemis_memory::MemoryEntry {
                id: format!("{}-{}", now_secs, self.state.token_usage),
                kind: artemis_memory::EntryKind::SessionLog,
                session_id: self.state.resolved.canonical_id.clone(),
                summary: format!(
                    "Model: {} | Provider: {} | Tokens: {}",
                    self.state.resolved.api_model_id,
                    self.state.resolved.provider,
                    self.state.token_usage
                ),
                content: prompt_summary,
                tags: vec![],
                created_at: format!("{now_secs}"),
            };
            SHARED_RUNTIME.block_on(memory.save_entry(entry));
        }

        all_events
    }

    /// Async variant of run — for use within a tokio runtime.
    pub async fn run_async(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        let mut all_events = Vec::new();

        for _ in 0..max_turns {
            let context_len = if self.state.resolved.context_length > 0 {
                self.state.resolved.context_length
            } else {
                131072
            };
            self.state.trim_messages(context_len, 15);

            let events = self.run_chat_async().await;

            let mut tool_calls = Vec::new();
            for event in &events {
                if let LoopEvent::ToolCallRequired { calls } = event {
                    tool_calls.extend(calls.clone());
                }
            }

            all_events.extend(events);

            if tool_calls.is_empty() {
                break;
            }
            if self.tool_executor.is_none() {
                break;
            }
            if let Some(ref executor) = self.tool_executor {
                for call in &tool_calls {
                    let result = executor.execute(call);
                    self.state.push_tool_result(&call.id, &result, None);
                }
            }
        }

        all_events
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

        run_async(async {
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
                        if let Some(ref u) = usage {
                            self.state.add_token_usage(u.total_tokens as u64);
                        }
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

    /// Async variant of run_chat — no run_async wrapper.
    async fn run_chat_async(&mut self) -> Vec<LoopEvent> {
        use futures::StreamExt;

        let mut stream = match self.chat_with_retry_async().await {
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
                StreamEvent::ToolCallEnd { .. } => {}
                StreamEvent::Done { usage, .. } => {
                    if let Some(ref u) = usage {
                        self.state.add_token_usage(u.total_tokens as u64);
                    }
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
            let result = run_async(artemis_core::chat(
                &self.state.resolved,
                &self.state.messages,
                &self.tools,
            ));

            match result {
                Ok(stream) => return Ok(stream),
                Err(ref e) => {
                    if attempt >= self.retry.max_retries || !ErrorClassifier::is_retryable(e) {
                        return Err(e.clone());
                    }
                    let delay = self.retry.jittered_backoff(attempt);
                    run_async(async {
                        tokio::time::sleep(delay).await;
                    });
                    attempt += 1;
                }
            }
        }
    }

    /// Async variant of chat_with_retry — no run_async wrapper.
    async fn chat_with_retry_async(
        &self,
    ) -> Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = StreamEvent> + Send>>,
        artemis_core::ArtemisError,
    > {
        use artemis_core::errors::ErrorClassifier;
        let mut attempt = 0u32;

        loop {
            match artemis_core::chat(&self.state.resolved, &self.state.messages, &self.tools).await
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

// ---------------------------------------------------------------------------
// PluginAgent impl — bridges artemis-agent to artemis-plugin
// ---------------------------------------------------------------------------

impl artemis_plugin::PluginAgent for Agent {
    fn set_system_prompt(&mut self, prompt: &str) {
        self.state.push_system_message(prompt);
    }

    fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.send_message(message);
        let mut content = String::new();
        let mut has_error = false;
        for event in &events {
            match event {
                LoopEvent::Token { text } => content.push_str(text),
                LoopEvent::Error { .. } => has_error = true,
                _ => {}
            }
        }
        if has_error && content.is_empty() {
            Err("Agent returned an error with no content".into())
        } else {
            Ok(content)
        }
    }
}
