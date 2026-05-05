use crate::prompt::provider::ContextProvider;
use crate::prompt::types::*;

/// Holds all registered context providers plus a dedicated system_prompt field.
///
/// Two registration paths:
/// 1. `set_system_prompt()` — convenience method for Agent API
/// 2. `register()` — generic provider registration
pub struct PromptRegistry {
    system_prompt: Option<String>,
    providers: Vec<Box<dyn ContextProvider>>,
}

impl Default for PromptRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptRegistry {
    pub fn new() -> Self {
        Self {
            system_prompt: None,
            providers: Vec::new(),
        }
    }

    /// Set or replace the system prompt. Produces a System-layer section
    /// during collect().
    pub fn set_system_prompt(&mut self, prompt: &str) {
        if prompt.is_empty() {
            self.system_prompt = None;
        } else {
            self.system_prompt = Some(prompt.to_string());
        }
    }

    /// Register a context provider. Takes ownership of a boxed provider.
    pub fn register(&mut self, provider: Box<dyn ContextProvider>) {
        self.providers.push(provider);
    }

    /// Produce all PromptSections with their corresponding budget declarations.
    ///
    /// System section (from set_system_prompt) is generated first with Fixed budget,
    /// then each registered provider's produce() is called in order.
    /// Providers returning None are skipped.
    /// Budget comes from the provider's budget() method — not from the section.
    pub async fn collect(
        &self,
        ctx: &AssemblyContext<'_>,
    ) -> (Vec<PromptSection>, Vec<TokenBudget>) {
        let mut sections = Vec::new();
        let mut budgets = Vec::new();

        // System section from dedicated field
        if let Some(ref prompt) = self.system_prompt {
            let tokens = lattice_core::tokens::TokenEstimator::estimate_text(prompt);
            sections.push(PromptSection {
                content: prompt.clone(),
                layer: Layer::System,
                priority: 0,
                tokens,
            });
            budgets.push(TokenBudget::Fixed(tokens));
        }

        // Registered providers
        for provider in &self.providers {
            if let Some(section) = provider.produce(ctx).await {
                sections.push(section);
                budgets.push(provider.budget());
            }
        }

        (sections, budgets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContextEvent;
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
    async fn empty_registry_returns_empty() {
        let registry = PromptRegistry::new();
        let bus_events: Vec<ContextEvent> = vec![];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
            model: &make_resolved(8192),
        };
        let (sections, budgets) = registry.collect(&ctx).await;
        assert!(sections.is_empty());
        assert!(budgets.is_empty());
    }

    #[tokio::test]
    async fn system_prompt_produces_section_with_fixed_budget() {
        let mut registry = PromptRegistry::new();
        registry.set_system_prompt("You are a helpful assistant.");
        let bus_events: Vec<ContextEvent> = vec![];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
            model: &make_resolved(8192),
        };
        let (sections, budgets) = registry.collect(&ctx).await;
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].layer, Layer::System);
        assert_eq!(budgets[0], TokenBudget::Fixed(sections[0].tokens));
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
        let bus_events: Vec<ContextEvent> = vec![];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
            model: &make_resolved(8192),
        };
        let (sections, budgets) = registry.collect(&ctx).await;
        assert_eq!(sections.len(), 1);
        assert_eq!(budgets[0], TokenBudget::Dynamic);
    }

    #[tokio::test]
    async fn set_system_prompt_replaces_previous() {
        let mut registry = PromptRegistry::new();
        registry.set_system_prompt("first");
        registry.set_system_prompt("second");
        let bus_events: Vec<ContextEvent> = vec![];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
            model: &make_resolved(8192),
        };
        let (sections, _) = registry.collect(&ctx).await;
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].content, "second");
    }

    #[tokio::test]
    async fn provider_returning_none_is_skipped() {
        let mut registry = PromptRegistry::new();
        registry.register(Box::new(NoneProvider));
        let bus_events: Vec<ContextEvent> = vec![];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
            model: &make_resolved(8192),
        };
        let (sections, budgets) = registry.collect(&ctx).await;
        assert!(sections.is_empty());
        assert!(budgets.is_empty());
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
        let bus_events: Vec<ContextEvent> = vec![];
        let ctx = AssemblyContext {
            request_id: "test",
            memory: None,
            #[cfg(feature = "blob-store")]
            blob_store: None,
            bus_events: &bus_events,
            model: &make_resolved(8192),
        };
        let (sections, budgets) = registry.collect(&ctx).await;
        assert_eq!(sections.len(), 2);
        assert_eq!(budgets.len(), 2);
        assert_eq!(sections[0].layer, Layer::Tools);
        assert_eq!(sections[1].layer, Layer::Memory);
    }
}
