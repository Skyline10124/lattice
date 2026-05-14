use std::any::Any;

use crate::provider::ContextProvider;
use crate::system_stack::{SystemPromptDelta, SystemPromptStack};
use crate::types::*;

/// Holds all registered context providers plus the System Prompt Stack.
///
/// Two registration paths:
/// 1. `set_system_prompt()` — convenience method for Agent API
/// 2. `register()` — generic provider registration
///
/// Pre-built contributions from PromptSpec static sections are stored
/// separately via `register_contribution()`.
pub struct PromptRegistry {
    system_stack: SystemPromptStack,
    providers: Vec<Box<dyn ContextProvider>>,
    static_contributions: Vec<PromptContribution>,
}

impl Default for PromptRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptRegistry {
    pub fn new() -> Self {
        Self {
            system_stack: SystemPromptStack::new(),
            providers: Vec::new(),
            static_contributions: Vec::new(),
        }
    }

    /// Set or replace the agent-specific system prompt delta.
    pub fn set_system_prompt(&mut self, prompt: &str) {
        self.system_stack.set_agent_prompt(prompt);
    }

    pub fn set_system_prompt_delta(&mut self, delta: Option<SystemPromptDelta>) {
        self.system_stack.set_agent_delta(delta);
    }

    pub fn set_output_contract_delta(&mut self, delta: Option<SystemPromptDelta>) {
        self.system_stack.set_contract_delta(delta);
    }

    /// Register a context provider. Takes ownership of a boxed provider.
    pub fn register(&mut self, provider: Box<dyn ContextProvider>) {
        self.providers.push(provider);
    }

    /// Register a pre-built contribution (e.g., from PromptSpec static sections).
    pub fn register_contribution(&mut self, contribution: PromptContribution) {
        self.static_contributions.push(contribution);
    }

    /// Apply runtime-specific configuration to all registered providers.
    pub fn configure(&mut self, key: &'static str, value: &(dyn Any + Send + Sync)) {
        for provider in &mut self.providers {
            provider.configure(key, value);
        }
    }

    /// Produce typed prompt contributions.
    ///
    /// This is the forward-compatible collection path: contribution metadata
    /// travels with content and budget instead of relying on parallel vectors.
    pub async fn collect_contributions(
        &self,
        ctx: &AssemblyContext<'_>,
    ) -> Vec<PromptContribution> {
        let mut sections = Vec::new();

        let system_prompt = self.system_stack.render().unwrap_or_else(|err| {
            tracing::warn!("failed to render system prompt stack: {}", err);
            SystemPromptStack::new()
                .render()
                .unwrap_or_else(|_| "You are a LATTICE agent.".to_string())
        });
        let tokens = lattice_core::tokens::TokenEstimator::estimate_text(&system_prompt);
        sections.push(PromptContribution::new(
            "system.stack",
            system_prompt,
            Layer::System,
            0,
            tokens,
            TokenBudget::Fixed(tokens),
            "system-stack",
        ));

        // Pre-built contributions (from PromptSpec static sections)
        for contribution in &self.static_contributions {
            sections.push(contribution.clone());
        }

        // Registered providers
        for provider in &self.providers {
            if let Some(contributions) = provider.contribute(ctx).await {
                sections.extend(contributions);
            }
        }

        sections
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct TestProvider {
        layer: Layer,
        priority: u8,
        content: &'static str,
        tokens: u32,
        budget: TokenBudget,
    }

    #[async_trait]
    impl ContextProvider for TestProvider {
        fn layer(&self) -> Layer {
            self.layer
        }
        fn priority(&self) -> u8 {
            self.priority
        }
        fn budget(&self) -> TokenBudget {
            self.budget
        }
        async fn produce(&self, _ctx: &AssemblyContext<'_>) -> Option<PromptSection> {
            Some(PromptSection {
                content: self.content.to_string(),
                layer: self.layer,
                priority: self.priority,
                tokens: self.tokens,
            })
        }
    }

    struct NoneProvider;

    #[async_trait]
    impl ContextProvider for NoneProvider {
        fn layer(&self) -> Layer {
            Layer::Events
        }
        async fn produce(&self, _ctx: &AssemblyContext<'_>) -> Option<PromptSection> {
            None
        }
    }

    fn make_resolved(cl: u32) -> lattice_core::ResolvedModel {
        lattice_core::ResolvedModel {
            canonical_id: "test".into(),
            provider: "test".into(),
            api_key: None,
            base_url: "".into(),
            api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
            api_model_id: "test".into(),
            context_length: cl,
            provider_specific: std::collections::HashMap::new(),
            credential_status: lattice_core::CredentialStatus::Missing,
        }
    }

    #[tokio::test]
    async fn empty_registry_returns_kernel_system_section() {
        let registry = PromptRegistry::new();
        let ctx = AssemblyContext {
            request_id: "test",
            model: &make_resolved(8192),
            user_input: "test",
        };
        let contributions = registry.collect_contributions(&ctx).await;
        assert_eq!(contributions.len(), 1);
        assert_eq!(contributions[0].layer, Layer::System);
        assert!(contributions[0]
            .content
            .contains("You are a LATTICE agent."));
        assert_eq!(
            contributions[0].budget,
            TokenBudget::Fixed(contributions[0].tokens)
        );
    }

    #[tokio::test]
    async fn system_prompt_produces_section_with_fixed_budget() {
        let mut registry = PromptRegistry::new();
        registry.set_system_prompt("You are a helpful assistant.");
        let ctx = AssemblyContext {
            request_id: "test",
            model: &make_resolved(8192),
            user_input: "test",
        };
        let contributions = registry.collect_contributions(&ctx).await;
        assert_eq!(contributions.len(), 1);
        assert_eq!(contributions[0].layer, Layer::System);
        assert!(contributions[0]
            .content
            .contains("You are a helpful assistant."));
        assert_eq!(
            contributions[0].budget,
            TokenBudget::Fixed(contributions[0].tokens)
        );
    }

    #[tokio::test]
    async fn registered_provider_appears_in_collect() {
        let mut registry = PromptRegistry::new();
        registry.register(Box::new(TestProvider {
            layer: Layer::Tools,
            priority: 5,
            content: "tool definitions",
            tokens: 10,
            budget: TokenBudget::Dynamic,
        }));
        let ctx = AssemblyContext {
            request_id: "test",
            model: &make_resolved(8192),
            user_input: "test",
        };
        let contributions = registry.collect_contributions(&ctx).await;
        assert_eq!(contributions.len(), 2);
        assert_eq!(contributions[0].layer, Layer::System);
        assert_eq!(contributions[1].layer, Layer::Tools);
        assert_eq!(
            contributions[0].budget,
            TokenBudget::Fixed(contributions[0].tokens)
        );
        assert_eq!(contributions[1].budget, TokenBudget::Dynamic);
    }

    #[tokio::test]
    async fn set_system_prompt_replaces_previous() {
        let mut registry = PromptRegistry::new();
        registry.set_system_prompt("first");
        registry.set_system_prompt("second");
        let ctx = AssemblyContext {
            request_id: "test",
            model: &make_resolved(8192),
            user_input: "test",
        };
        let contributions = registry.collect_contributions(&ctx).await;
        assert_eq!(contributions.len(), 1);
        assert!(contributions[0].content.contains("second"));
        assert!(!contributions[0].content.contains("first"));
    }

    #[tokio::test]
    async fn provider_returning_none_is_skipped() {
        let mut registry = PromptRegistry::new();
        registry.register(Box::new(NoneProvider));
        let ctx = AssemblyContext {
            request_id: "test",
            model: &make_resolved(8192),
            user_input: "test",
        };
        let contributions = registry.collect_contributions(&ctx).await;
        assert_eq!(contributions.len(), 1);
        assert_eq!(contributions[0].layer, Layer::System);
    }

    #[tokio::test]
    async fn register_contribution_appears_in_collect() {
        let mut registry = PromptRegistry::new();
        let contribution = PromptContribution::new(
            "test.contrib",
            "static content",
            Layer::System,
            0,
            10,
            TokenBudget::Fixed(10),
            "test",
        );
        registry.register_contribution(contribution);
        let ctx = AssemblyContext {
            request_id: "test",
            model: &make_resolved(8192),
            user_input: "test",
        };
        let contributions = registry.collect_contributions(&ctx).await;
        // system.stack + static contribution
        assert_eq!(contributions.len(), 2);
        assert_eq!(contributions[1].id, "test.contrib");
        assert_eq!(contributions[1].content, "static content");
    }

    #[tokio::test]
    async fn none_provider_skipped_between_real_providers() {
        let mut registry = PromptRegistry::new();
        registry.register(Box::new(TestProvider {
            layer: Layer::Tools,
            priority: 1,
            content: "real tool",
            tokens: 5,
            budget: TokenBudget::Dynamic,
        }));
        registry.register(Box::new(NoneProvider));
        registry.register(Box::new(TestProvider {
            layer: Layer::Memory,
            priority: 5,
            content: "memory recall",
            tokens: 10,
            budget: TokenBudget::Ratio(0.3),
        }));
        let ctx = AssemblyContext {
            request_id: "test",
            model: &make_resolved(8192),
            user_input: "test",
        };
        let contributions = registry.collect_contributions(&ctx).await;
        assert_eq!(contributions.len(), 3);
        assert_eq!(contributions[0].layer, Layer::System);
        assert_eq!(contributions[1].layer, Layer::Tools);
        assert_eq!(contributions[2].layer, Layer::Memory);
    }
}
