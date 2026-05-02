pub mod memory;
pub mod sandbox;
pub mod state;
pub mod tool_definitions;
pub mod tool_error;
pub mod tools;

use std::collections::HashMap;
use std::sync::LazyLock;

use async_trait::async_trait;
use lattice_core::retry::RetryPolicy;
use lattice_core::streaming::StreamEvent;
use lattice_core::types::ToolDefinition;
use lattice_core::ResolvedModel;
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
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &lattice_core::types::ToolCall) -> String;
}

/// Max retries for mid-stream errors in Agent::run() and run_async().
const MAX_STREAM_RETRIES: u32 = 2;

/// Tool loop max turns per Agent::run() call for send_message_with_tools.
const MAX_TOOL_TURNS: u32 = 10;

/// Minimal interface for an LLM-calling agent.
/// Used by PluginRunner to call any agent that implements send + system_prompt.
#[async_trait(?Send)]
pub trait PluginAgent {
    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;
    /// Send a user message and automatically handle tool calls via Agent::run().
    async fn send_message_with_tools(
        &mut self,
        message: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        // Default: delegate to send() for backward compat with non-Agent impls
        self.send(message).await
    }
    fn set_system_prompt(&mut self, _prompt: &str) {}
    fn token_usage(&self) -> u64 {
        0
    }
}

// Re-export shared default tools and sandbox for convenience.
pub use sandbox::SandboxConfig;
pub use tool_definitions::default_tool_definitions;
pub use tools::DefaultToolExecutor;

#[allow(dead_code)]
pub struct Agent {
    resolved: ResolvedModel,
    state: state::AgentState,
    tools: Vec<ToolDefinition>,
    retry: RetryPolicy,
    memory: Option<Box<dyn memory::Memory>>,
    tool_executor: Option<Box<dyn ToolExecutor>>,
}

impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self {
        LazyLock::force(&SHARED_RUNTIME);
        Self {
            resolved: resolved.clone(),
            state: state::AgentState::new(resolved),
            tools: vec![],
            retry: RetryPolicy::default(),
            memory: None,
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

    pub fn with_memory(mut self, memory: Box<dyn memory::Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_tool_executor(mut self, executor: Box<dyn ToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    /// Replace the current system message (not append).
    pub fn set_system_prompt(&mut self, prompt: &str) {
        use lattice_core::types::{Message, Role};
        let msg = Message::new(Role::System, prompt.to_string(), None, None, None);
        match self.state.messages.first() {
            Some(m) if m.role == Role::System => {
                self.state.messages[0] = msg;
            }
            _ => {
                self.state.messages.insert(0, msg);
            }
        }
    }

    pub fn token_usage(&self) -> u64 {
        self.state.token_usage
    }

    pub fn send_message(&mut self, content: &str) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        run_async(self.run_chat())
    }

    pub async fn send_message_async(&mut self, content: &str) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        self.run_chat().await
    }

    pub fn submit_tools(
        &mut self,
        results: Vec<(String, String)>,
        max_size: Option<usize>,
    ) -> Vec<LoopEvent> {
        for (call_id, result) in &results {
            self.state.push_tool_result(call_id, result, max_size);
        }
        run_async(self.run_chat())
    }

    pub fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        let mut all_events = Vec::new();

        for _ in 0..max_turns {
            let context_len = if self.state.resolved.context_length > 0 {
                self.state.resolved.context_length
            } else {
                131072
            };
            self.state.trim_messages(context_len, 15);

            let mut events = run_async(self.run_chat());

            let mut retry_count = 0u32;
            while retry_count < MAX_STREAM_RETRIES {
                let has_error = events.iter().any(|e| matches!(e, LoopEvent::Error { .. }));
                if !has_error {
                    break;
                }
                self.state.pop_last_assistant_message();
                retry_count += 1;
                events = run_async(self.run_chat());
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

            if self.tool_executor.is_none() {
                break;
            }

            if let Some(ref executor) = self.tool_executor {
                for call in &tool_calls {
                    let result = run_async(executor.execute(call));
                    self.state.push_tool_result(&call.id, &result, None);
                }
            }
        }

        // --- Auto-save memory entry ---
        if let Some(ref memory) = self.memory {
            let prompt_summary = if content.len() > 200 {
                let mut end = 200;
                while end > 0 && !content.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &content[..end])
            } else {
                content.to_string()
            };
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let entry = crate::memory::MemoryEntry {
                id: format!("{}-{}", now_secs, self.state.token_usage),
                kind: crate::memory::EntryKind::SessionLog,
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
            memory.save_entry(entry);
        }

        all_events
    }

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

            let mut events = self.run_chat().await;

            let mut retry_count = 0u32;
            while retry_count < MAX_STREAM_RETRIES {
                let has_error = events.iter().any(|e| matches!(e, LoopEvent::Error { .. }));
                if !has_error {
                    break;
                }
                self.state.pop_last_assistant_message();
                retry_count += 1;
                events = self.run_chat().await;
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
            if self.tool_executor.is_none() {
                break;
            }
            if let Some(ref executor) = self.tool_executor {
                for call in &tool_calls {
                    let result = executor.execute(call).await;
                    self.state.push_tool_result(&call.id, &result, None);
                }
            }
        }

        all_events
    }

    async fn run_chat(&mut self) -> Vec<LoopEvent> {
        use futures::StreamExt;

        let mut stream = match self.chat_with_retry().await {
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
    ) -> Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = StreamEvent> + Send>>,
        lattice_core::LatticeError,
    > {
        use lattice_core::errors::ErrorClassifier;
        let mut attempt = 0u32;

        loop {
            match lattice_core::chat(&self.state.resolved, &self.state.messages, &self.tools).await
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
    Reasoning {
        text: String,
    },
    ToolCallRequired {
        calls: Vec<lattice_core::types::ToolCall>,
    },
    Done {
        usage: Option<lattice_core::streaming::TokenUsage>,
    },
    Error {
        message: String,
    },
}

struct ToolCallAccum {
    name: String,
    arguments: String,
}

#[async_trait(?Send)]
impl PluginAgent for Agent {
    fn set_system_prompt(&mut self, prompt: &str) {
        self.state.push_system_message(prompt);
    }

    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>> {
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

    async fn send_message_with_tools(
        &mut self,
        message: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.run(message, MAX_TOOL_TURNS);
        let mut content = String::new();
        for event in &events {
            if let LoopEvent::Token { text } = event {
                content.push_str(text);
            }
        }
        Ok(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_system_prompt_does_not_panic() {
        let resolved = lattice_core::ResolvedModel {
            canonical_id: "test".into(),
            api_model_id: "test".into(),
            provider: "test".into(),
            base_url: "http://localhost".to_string(),
            api_key: Some("sk-test".into()),
            api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
            context_length: 4096,
            credential_status: lattice_core::catalog::CredentialStatus::Present,
            provider_specific: Default::default(),
        };
        let mut agent = Agent::new(resolved);
        agent.set_system_prompt("first");
        agent.set_system_prompt("second");
        // If we get here without panic, the inherent method resolved correctly
    }
}
