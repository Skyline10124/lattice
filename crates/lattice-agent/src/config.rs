use crate::bus_event_collector::ContextEvent;
use crate::manifest::SessionContext;
use crate::prompt::MemoryProvider;
use crate::{behavior, memory, prompt, state, Agent};
use lattice_core::retry::RetryPolicy;
use lattice_core::types::ToolDefinition;
use lattice_core::ResolvedModel;
use lattice_hooks::{PromptHook, PromptHookChain};
use lattice_tools::ToolExecutor;

impl Agent {
    pub fn new(resolved: ResolvedModel) -> Self {
        let state = state::AgentState::new(resolved);
        Self {
            state,
            tools: vec![],
            retry: RetryPolicy::default(),
            behavior: behavior::BehaviorMode::default(),
            audit: None,
            memory: None,
            tool_executor: None,
            registry: prompt::PromptRegistry::new(),
            prompt_hooks: PromptHookChain::default(),
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_event_collector: crate::bus_event_collector::BusEventCollector::new(),
            thinking_effort: None,
            session_context: None,
        }
    }

    pub fn with_thinking_effort(mut self, effort: Option<String>) -> Self {
        self.thinking_effort = effort;
        self
    }

    /// Set session context for manifest collection (D-15).
    /// Values propagate to TurnManifestObject.session_id, .branch, .parent_commit_id.
    pub fn with_session_context(
        mut self,
        session_id: impl Into<String>,
        branch: impl Into<String>,
        parent_commit_id: impl Into<String>,
    ) -> Self {
        self.session_context = Some(SessionContext {
            session_id: session_id.into(),
            branch: branch.into(),
            parent_commit_id: parent_commit_id.into(),
        });
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn set_tools(&mut self, tools: Vec<ToolDefinition>) {
        self.tools = tools;
    }

    pub fn with_retry(mut self, policy: RetryPolicy) -> Self {
        self.retry = policy;
        self
    }

    pub fn with_audit(mut self, audit: std::sync::Arc<crate::audit::AuditLog>) -> Self {
        self.audit = Some(audit);
        self
    }

    pub fn with_memory(mut self, memory: std::sync::Arc<dyn memory::Memory>) -> Self {
        self.registry.register(Box::new(
            MemoryProvider::default().with_memory(std::sync::Arc::clone(&memory)),
        ));
        self.memory = Some(memory);
        self
    }

    pub fn with_tool_executor(mut self, executor: Box<dyn ToolExecutor>) -> Self {
        self.tool_executor = Some(executor);
        self
    }

    pub fn set_tool_executor(&mut self, executor: Option<Box<dyn ToolExecutor>>) {
        self.tool_executor = executor;
    }

    /// Set the blob store for context persistence (feature-gated).
    /// Takes an Arc<BlobStore> so the same instance can be shared with
    /// DefaultToolExecutor for bus:fetch tool execution.
    #[cfg(feature = "blob-store")]
    pub fn with_blob_store(mut self, store: std::sync::Arc<crate::blob::BlobStore>) -> Self {
        self.registry.configure("lattice_agent.blob_store", &store);
        self.blob_store = Some(store);
        self
    }

    #[cfg(feature = "blob-store")]
    pub fn with_events_provider(mut self, source: &str) -> Self {
        let mut provider = crate::events_provider::EventsProvider::new(source)
            .with_event_collector(self.bus_event_collector.clone());
        if let Some(blob_store) = &self.blob_store {
            provider = provider.with_blob_store(std::sync::Arc::clone(blob_store));
        }
        self.registry.register(Box::new(provider));
        self
    }

    /// Inject bus events into the collector for prompt assembly.
    pub fn inject_bus_events(&mut self, events: Vec<ContextEvent>) {
        self.bus_event_collector.push_many(events);
    }

    /// Register a context provider with the prompt engine.
    pub fn with_provider(mut self, provider: impl prompt::ContextProvider + 'static) -> Self {
        let mut provider: Box<dyn prompt::ContextProvider> = Box::new(provider);
        self.configure_provider(provider.as_mut());
        self.registry.register(provider);
        self
    }

    /// Register a boxed context provider (for dynamic dispatch from factories).
    pub fn with_provider_dyn(mut self, mut provider: Box<dyn prompt::ContextProvider>) -> Self {
        self.configure_provider(provider.as_mut());
        self.registry.register(provider);
        self
    }

    /// Register a pre-built contribution (from PromptSpec static sections).
    pub fn add_contribution(mut self, contribution: prompt::PromptContribution) -> Self {
        self.registry.register_contribution(contribution);
        self
    }

    /// Register a typed prompt lifecycle hook.
    pub fn with_prompt_hook(mut self, hook: impl PromptHook + 'static) -> Self {
        self.prompt_hooks.push(Box::new(hook));
        self
    }

    /// Replace the prompt hook chain.
    pub fn with_prompt_hooks(mut self, hooks: Vec<Box<dyn PromptHook>>) -> Self {
        self.prompt_hooks = PromptHookChain::new(hooks);
        self
    }

    /// Register a task state provider that renders recovery state into prompts.
    /// The provider shares the agent's live `TaskState` through an Arc<Mutex<>> handle.
    pub fn with_task_state_provider(mut self) -> Self {
        let shared = self.state.shared_task_state();
        let provider = crate::prompt::TaskStateProvider::new().with_state(shared);
        self.registry.register(Box::new(provider));
        self
    }

    /// Set or replace the system prompt (fed through prompt engine as System layer).
    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.registry.set_system_prompt(prompt);
    }

    pub fn set_system_prompt_delta(&mut self, delta: Option<prompt::SystemPromptDelta>) {
        self.registry.set_system_prompt_delta(delta);
    }

    pub fn set_output_contract_delta(&mut self, delta: Option<prompt::SystemPromptDelta>) {
        self.registry.set_output_contract_delta(delta);
    }

    fn configure_provider(&self, provider: &mut dyn prompt::ContextProvider) {
        provider.configure(
            "lattice_agent.bus_event_collector",
            &self.bus_event_collector,
        );
        #[cfg(feature = "blob-store")]
        if let Some(blob_store) = &self.blob_store {
            provider.configure("lattice_agent.blob_store", blob_store);
        }
    }

    /// Append tool definitions to the agent's tool list.
    pub fn add_tools(&mut self, tools: Vec<ToolDefinition>) {
        self.tools.extend(tools);
    }

    /// Return the conversation history accumulated by this agent.
    pub fn messages(&self) -> &[lattice_core::types::Message] {
        &self.state.messages
    }

    pub fn token_usage(&self) -> u64 {
        self.state.token_usage
    }

    pub fn seed_messages(&mut self, messages: Vec<lattice_core::types::Message>) {
        self.state.seed_messages(messages);
    }

    /// Access the agent's live task state (read-only, locks the inner Mutex).
    pub fn task_state(&self) -> std::sync::MutexGuard<'_, state::TaskState> {
        self.state.task_state()
    }

    /// Access the agent's live task state (mutable, locks the inner Mutex).
    pub fn task_state_mut(&self) -> std::sync::MutexGuard<'_, state::TaskState> {
        self.state.task_state_mut()
    }
}
