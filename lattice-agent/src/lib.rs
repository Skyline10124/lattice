pub mod bus_event_collector;
pub mod memory;
pub mod prompt;
pub mod sandbox;
pub mod state;
pub mod tool_definitions;
pub mod tool_error;
pub mod tools;

#[cfg(feature = "blob-store")]
pub mod blob;

#[cfg(feature = "blob-store")]
pub mod events_provider;

use std::collections::HashMap;

use async_trait::async_trait;
use lattice_core::retry::RetryPolicy;
use lattice_core::streaming::StreamEvent;
use lattice_core::types::{Role, ToolDefinition};
use lattice_core::ResolvedModel;

/// Executes a tool call and returns the result string.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    async fn execute(&self, call: &lattice_core::types::ToolCall) -> String;
}

/// Max retries for mid-stream errors in Agent::run().
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
pub use tools::RegistryToolAccess;

/// An async LLM agent with tool execution and conversation state.
///
/// # Runtime model
///
/// `Agent::run()` is `async` and **must** be called from within a Tokio runtime
/// (e.g. from `#[tokio::main]` or a `tokio::spawn`-ed task).  Internally it calls
/// [`lattice_core::chat()`] which uses `reqwest` — a Tokio-aware HTTP client.
///
/// For callers that are **not** inside a Tokio runtime (e.g. synchronous library
/// code or FFI), wrap the call at the outermost boundary:
///
/// ```ignore
/// let rt = tokio::runtime::Runtime::new()?;
/// let result = rt.block_on(agent.run("prompt", 10));
/// ```
///
/// Do **not** call `block_on` from within a Tokio context — this will panic
/// or deadlock.  The Python bindings in `lattice-python` handle this via a
/// shared runtime and `tokio::task::block_in_place`.
pub struct Agent {
    // Kept for future per-agent model re-resolution; state also holds a copy for chat().
    #[allow(dead_code)]
    resolved: ResolvedModel,
    state: state::AgentState,
    tools: Vec<ToolDefinition>,
    retry: RetryPolicy,
    memory: Option<Box<dyn memory::Memory>>,
    tool_executor: Option<Box<dyn ToolExecutor>>,
    registry: prompt::PromptRegistry,
    #[cfg(feature = "blob-store")]
    blob_store: Option<std::sync::Arc<crate::blob::BlobStore>>,
    bus_event_collector: crate::bus_event_collector::BusEventCollector,
}

impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self {
        Self {
            resolved: resolved.clone(),
            state: state::AgentState::new(resolved),
            tools: vec![],
            retry: RetryPolicy::default(),
            memory: None,
            tool_executor: None,
            registry: prompt::PromptRegistry::new(),
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_event_collector: crate::bus_event_collector::BusEventCollector::new(),
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

    /// Set the blob store for context persistence (feature-gated).
    /// Takes an Arc<BlobStore> so the same instance can be shared with
    /// DefaultToolExecutor for bus:fetch tool execution.
    #[cfg(feature = "blob-store")]
    pub fn with_blob_store(mut self, store: std::sync::Arc<crate::blob::BlobStore>) -> Self {
        self.blob_store = Some(store);
        self
    }

    /// Inject bus events into the collector for prompt assembly.
    pub fn inject_bus_events(&mut self, events: Vec<lattice_bus::BusEvent>) {
        self.bus_event_collector.push_many(events);
    }

    /// Register a context provider with the prompt engine.
    pub fn with_provider(mut self, provider: impl prompt::ContextProvider + 'static) -> Self {
        self.registry.register(Box::new(provider));
        self
    }

    /// Set or replace the system prompt (fed through prompt engine as System layer).
    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.registry.set_system_prompt(prompt);
    }

    pub fn token_usage(&self) -> u64 {
        self.state.token_usage
    }

    pub async fn send_message(&mut self, content: &str) -> Vec<LoopEvent> {
        self.state.push_user_message(content);
        self.run_chat().await
    }

    pub async fn submit_tools(
        &mut self,
        results: Vec<(String, String)>,
        max_size: Option<usize>,
    ) -> Vec<LoopEvent> {
        for (call_id, result) in &results {
            self.state.push_tool_result(call_id, result, max_size);
        }
        self.run_chat().await
    }

    pub async fn run(&mut self, content: &str, max_turns: u32) -> Vec<LoopEvent> {
        let mut all_events = Vec::new();
        let user_input = content.to_string();

        for _ in 0..max_turns {
            // Compile prompt fresh each iteration: events and memory
            // accumulated from previous tool turns are re-evaluated by providers,
            // and budget pressure from growing tool results naturally compresses
            // lower-priority sections.
            let bus_events = self.bus_event_collector.drain();
            let ctx = prompt::AssemblyContext {
                request_id: "run",
                memory: self.memory.as_deref(),
                model: &self.state.resolved,
                #[cfg(feature = "blob-store")]
                blob_store: self.blob_store.as_deref(),
                bus_events: &bus_events,
            };
            let (sections, budgets) = self.registry.collect(&ctx).await;
            let rendered =
                prompt::compiler::compile(&sections, &budgets, &user_input, &self.state.resolved);
            for msg in &rendered.messages {
                match msg.role {
                    Role::System => self.state.push_system_message(&msg.content),
                    Role::User => self.state.replace_first_user_message(&msg.content),
                    _ => {}
                }
            }

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
        self.registry.set_system_prompt(prompt);
    }

    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.send_message(message).await;
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
        let events = self.run(message, MAX_TOOL_TURNS).await;
        let mut content = String::new();
        let mut has_error = false;
        let mut error_messages = Vec::new();
        for event in &events {
            match event {
                LoopEvent::Token { text } => content.push_str(text),
                LoopEvent::Error { message } => {
                    has_error = true;
                    error_messages.push(message.clone());
                }
                _ => {}
            }
        }
        if has_error && content.is_empty() {
            Err(format!("Agent errors: {}", error_messages.join("; ")).into())
        } else {
            Ok(content)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use lattice_core::catalog::ApiProtocol;
    use std::collections::HashMap;

    fn make_resolved(context_length: u32) -> ResolvedModel {
        ResolvedModel {
            canonical_id: "test".into(),
            api_model_id: "test".into(),
            provider: "test".into(),
            base_url: "http://localhost".to_string(),
            api_key: Some("sk-test".into()),
            api_protocol: ApiProtocol::OpenAiChat,
            context_length,
            credential_status: lattice_core::CredentialStatus::Present,
            provider_specific: HashMap::new(),
        }
    }

    #[test]
    fn test_set_system_prompt_does_not_panic() {
        let resolved = make_resolved(4096);
        let mut agent = Agent::new(resolved);
        agent.set_system_prompt("first");
        agent.set_system_prompt("second");
        // If we get here without panic, the inherent method resolved correctly
    }

    /// A test provider that produces a fixed section at a given layer/priority.
    struct TestProvider {
        layer: prompt::Layer,
        priority: u8,
        content: &'static str,
        tokens: u32,
        budget: prompt::TokenBudget,
    }

    #[async_trait]
    impl prompt::ContextProvider for TestProvider {
        fn layer(&self) -> prompt::Layer {
            self.layer
        }
        fn priority(&self) -> u8 {
            self.priority
        }
        fn budget(&self) -> prompt::TokenBudget {
            self.budget
        }
        async fn produce(
            &self,
            _ctx: &prompt::AssemblyContext<'_>,
        ) -> Option<prompt::PromptSection> {
            Some(prompt::PromptSection {
                content: self.content.to_string(),
                layer: self.layer,
                priority: self.priority,
                tokens: self.tokens,
            })
        }
    }

    #[tokio::test]
    async fn test_agent_prompt_compilation_integration() {
        // Verify that set_system_prompt + registered provider produces
        // correctly ordered messages through the engine.
        let resolved = make_resolved(16384);
        let mut agent = Agent::new(resolved);

        agent.set_system_prompt("You are a code review assistant.");

        agent = agent.with_provider(TestProvider {
            layer: prompt::Layer::Tools,
            priority: 5,
            content: "read_file, grep, patch",
            tokens: 8,
            budget: prompt::TokenBudget::Dynamic,
        });

        // Simulate what Agent::run() does internally (no network call)
        let bus_events: Vec<lattice_bus::BusEvent> = vec![];
        let ctx = prompt::AssemblyContext {
            request_id: "test",
            memory: agent.memory.as_deref(),
            model: &agent.state.resolved,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
        };
        let (sections, budgets) = agent.registry.collect(&ctx).await;
        let rendered = prompt::compiler::compile(
            &sections,
            &budgets,
            "review this file",
            &agent.state.resolved,
        );

        // System message with plain content
        let sys = rendered
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .unwrap();
        assert!(sys.content.contains("You are a code review assistant."));

        // User message with Tools + Input markers
        let user = rendered
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .unwrap();
        assert!(user.content.contains("=== Tools ==="));
        assert!(user.content.contains("=== Input ==="));
        assert!(user.content.contains("read_file, grep, patch"));
        assert!(user.content.contains("review this file"));

        // Verify what gets pushed to state
        for msg in &rendered.messages {
            match msg.role {
                Role::System => agent.state.push_system_message(&msg.content),
                Role::User => agent.state.push_user_message(&msg.content),
                _ => {}
            }
        }
        // System message at the front
        assert_eq!(agent.state.messages[0].role, Role::System);
        assert!(agent.state.messages[0]
            .content
            .contains("You are a code review assistant."));
        // User message after
        assert_eq!(agent.state.messages[1].role, Role::User);
    }

    #[tokio::test]
    async fn test_empty_registry_falls_through_to_raw_input() {
        // No system prompt, no providers → output should be raw user text.
        let resolved = make_resolved(16384);
        let agent = Agent::new(resolved);

        let bus_events: Vec<lattice_bus::BusEvent> = vec![];
        let ctx = prompt::AssemblyContext {
            request_id: "test",
            memory: agent.memory.as_deref(),
            model: &agent.state.resolved,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
        };
        let (sections, budgets) = agent.registry.collect(&ctx).await;
        let rendered =
            prompt::compiler::compile(&sections, &budgets, "hello", &agent.state.resolved);

        assert_eq!(rendered.messages.len(), 1);
        assert_eq!(rendered.messages[0].role, Role::User);
        assert_eq!(
            rendered.messages[0].content, "hello",
            "empty registry → raw pass-through"
        );
    }

    #[tokio::test]
    async fn test_budget_overflow_drops_low_priority_provider() {
        // Tight budget should drop non-essential sections.
        let resolved = make_resolved(5000); // effective: 5000 - 4096 = 904
        let mut agent = Agent::new(resolved);

        agent.set_system_prompt("S");
        // Use a large-token provider at Events layer
        struct BigProvider;
        #[async_trait]
        impl prompt::ContextProvider for BigProvider {
            fn layer(&self) -> prompt::Layer {
                prompt::Layer::Events
            }
            fn priority(&self) -> u8 {
                5
            }
            fn budget(&self) -> prompt::TokenBudget {
                prompt::TokenBudget::Dynamic
            }
            async fn produce(
                &self,
                _ctx: &prompt::AssemblyContext<'_>,
            ) -> Option<prompt::PromptSection> {
                Some(prompt::PromptSection {
                    content: "x".repeat(2000),
                    layer: prompt::Layer::Events,
                    priority: 5,
                    tokens: 2000,
                })
            }
        }
        agent = agent.with_provider(BigProvider);

        let bus_events: Vec<lattice_bus::BusEvent> = vec![];
        let ctx = prompt::AssemblyContext {
            request_id: "test",
            memory: agent.memory.as_deref(),
            model: &agent.state.resolved,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
        };
        let (sections, budgets) = agent.registry.collect(&ctx).await;
        let rendered = prompt::compiler::compile(&sections, &budgets, "hi", &agent.state.resolved);

        // Events (2000 tokens > 904 effective) should be dropped
        let user = rendered
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .unwrap();
        assert!(!user.content.contains("=== Events ==="));
        // System message survives
        let sys = rendered
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .unwrap();
        assert_eq!(sys.content, "S");
    }

    // -----------------------------------------------------------------------
    // Integration tests: context management layer with blob-store
    // -----------------------------------------------------------------------

    #[cfg(feature = "blob-store")]
    #[tokio::test]
    async fn test_agent_with_blob_store_and_bus_events() {
        let resolved = make_resolved(16384);
        let store = crate::blob::BlobStore::connect("sqlite::memory:")
            .await
            .unwrap();

        let mut agent = Agent::new(resolved)
            .with_blob_store(std::sync::Arc::new(store))
            .with_provider(crate::events_provider::EventsProvider::new("test-source"));

        // Inject a small event (should inline)
        let small_event = lattice_bus::BusEvent {
            topic: "audit-pass".to_string(),
            source: lattice_bus::AgentId::new("auditor"),
            payload: serde_json::json!({"status": "clean"}),
        };
        // Inject a large event (should go to blob)
        let large_event = lattice_bus::BusEvent {
            topic: "inspection".to_string(),
            source: lattice_bus::AgentId::new("auditor"),
            payload: serde_json::json!({"lines": vec![String::from("x").repeat(5000)]}),
        };
        agent.inject_bus_events(vec![small_event, large_event]);

        // Simulate prompt compilation (no network call)
        let bus_events = agent.bus_event_collector.drain();
        let ctx = prompt::AssemblyContext {
            request_id: "test",
            memory: agent.memory.as_deref(),
            model: &agent.state.resolved,
            blob_store: agent.blob_store.as_deref(),
            bus_events: &bus_events,
        };
        let (sections, budgets) = agent.registry.collect(&ctx).await;
        let rendered = prompt::compiler::compile(
            &sections,
            &budgets,
            "review these events",
            &agent.state.resolved,
        );

        let user = rendered
            .messages
            .iter()
            .find(|m| m.role == Role::User)
            .unwrap();

        // Small event inlined
        assert!(user.content.contains("[topic: audit-pass]"));
        // Large event as blob reference
        assert!(user.content.contains("blob://test-source/inspection/"));
    }

    #[cfg(feature = "blob-store")]
    #[tokio::test]
    async fn test_bus_fetch_tool_execution() {
        let store = crate::blob::BlobStore::connect("sqlite::memory:")
            .await
            .unwrap();
        let blob = crate::blob::StoredBlob {
            key: "blob://test/data/abc123".to_string(),
            source: "test".to_string(),
            topic: "data".to_string(),
            mime: "application/json".to_string(),
            size: 100,
            payload: "{\"result\": \"ok\"}".to_string(),
            summary: "".to_string(),
        };
        store.insert(&blob).await.unwrap();

        let executor = crate::tools::DefaultToolExecutor::new_with_blob_store(
            "/tmp",
            Some(std::sync::Arc::new(store)),
        )
        .unwrap();

        let call = lattice_core::types::ToolCall {
            id: "call-1".to_string(),
            function: lattice_core::types::FunctionCall {
                name: "bus:fetch".to_string(),
                arguments: "{\"key\": \"blob://test/data/abc123\"}".to_string(),
            },
        };
        let result = executor.execute(&call).await;
        assert_eq!(result, "{\"result\": \"ok\"}");
    }

    #[cfg(feature = "blob-store")]
    #[tokio::test]
    async fn test_bus_fetch_not_found() {
        let store = crate::blob::BlobStore::connect("sqlite::memory:")
            .await
            .unwrap();
        let executor = crate::tools::DefaultToolExecutor::new_with_blob_store(
            "/tmp",
            Some(std::sync::Arc::new(store)),
        )
        .unwrap();

        let call = lattice_core::types::ToolCall {
            id: "call-2".to_string(),
            function: lattice_core::types::FunctionCall {
                name: "bus:fetch".to_string(),
                arguments: "{\"key\": \"blob://test/nonexistent/abc\"}".to_string(),
            },
        };
        let result = executor.execute(&call).await;
        assert!(result.contains("not found"));
    }

    #[cfg(feature = "blob-store")]
    #[tokio::test]
    async fn test_bus_fetch_without_blob_store() {
        let executor =
            crate::tools::DefaultToolExecutor::new_with_blob_store("/tmp", None).unwrap();
        let call = lattice_core::types::ToolCall {
            id: "call-3".to_string(),
            function: lattice_core::types::FunctionCall {
                name: "bus:fetch".to_string(),
                arguments: "{\"key\": \"blob://any/key/here\"}".to_string(),
            },
        };
        let result = executor.execute(&call).await;
        assert!(result.contains("not configured"));
    }

    #[cfg(feature = "blob-store")]
    #[tokio::test]
    async fn test_degradation_without_blob_store() {
        // Agent without blob_store: large events silently skipped
        let resolved = make_resolved(16384);
        let mut agent = Agent::new(resolved)
            .with_provider(crate::events_provider::EventsProvider::new("test-source"));

        // All events are large — will be skipped with no blob_store
        let large_event = lattice_bus::BusEvent {
            topic: "big-data".to_string(),
            source: lattice_bus::AgentId::new("source"),
            payload: serde_json::json!({"data": String::from("x").repeat(5000)}),
        };
        agent.inject_bus_events(vec![large_event]);

        let bus_events = agent.bus_event_collector.drain();
        let ctx = prompt::AssemblyContext {
            request_id: "test",
            memory: agent.memory.as_deref(),
            model: &agent.state.resolved,
            blob_store: None,
            bus_events: &bus_events,
        };
        let (sections, budgets) = agent.registry.collect(&ctx).await;
        let rendered =
            prompt::compiler::compile(&sections, &budgets, "check", &agent.state.resolved);

        // No Events section should appear (all events skipped)
        let user = rendered.messages.iter().find(|m| m.role == Role::User);
        if let Some(user_msg) = user {
            assert!(!user_msg.content.contains("=== Events ==="));
        }
    }
}
