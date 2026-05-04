use async_trait::async_trait;
use crate::prompt::types::*;
use crate::prompt::provider::ContextProvider;

/// Holds all registered context providers plus a dedicated system_prompt field.
///
/// Two registration paths:
/// 1. `set_system_prompt()` — convenience method for Agent API
/// 2. `register()` — generic provider registration
pub struct PromptRegistry {
    system_prompt: Option<String>,
    providers: Vec<Box<dyn ContextProvider>>,
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

    /// Produce all PromptSections by iterating registered providers.
    ///
    /// System section (from set_system_prompt) is generated first,
    /// then each registered provider's produce() is called in order.
    /// Providers returning None are skipped.
    pub async fn collect(&self, ctx: &AssemblyContext<'_>) -> Vec<PromptSection> {
        let mut sections = Vec::new();

        // System section from dedicated field
        if let Some(ref prompt) = self.system_prompt {
            let tokens = lattice_core::tokens::TokenEstimator::estimate_text(prompt);
            sections.push(PromptSection {
                content: prompt.clone(),
                layer: Layer::System,
                priority: 0,
                tokens,
            });
        }

        // Registered providers
        for provider in &self.providers {
            if let Some(section) = provider.produce(ctx).await {
                sections.push(section);
            }
        }

        sections
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::types::*;

    struct TestProvider {
        layer: Layer,
        priority: u8,
        content: &'static str,
        tokens: u32,
    }

    #[async_trait]
    impl ContextProvider for TestProvider {
        fn layer(&self) -> Layer { self.layer }
        fn priority(&self) -> u8 { self.priority }
        async fn produce(&self, _ctx: &AssemblyContext<'_>) -> Option<PromptSection> {
            Some(PromptSection {
                content: self.content.to_string(),
                layer: self.layer,
                priority: self.priority,
                tokens: self.tokens,
            })
        }
    }

    fn make_resolved(cl: u32) -> lattice_core::ResolvedModel {
        lattice_core::ResolvedModel {
            canonical_id: "test".into(), provider: "test".into(),
            api_key: None, base_url: "".into(),
            api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
            api_model_id: "test".into(), context_length: cl,
            provider_specific: std::collections::HashMap::new(),
            credential_status: lattice_core::CredentialStatus::Missing,
        }
    }

    #[tokio::test]
    async fn empty_registry_returns_empty_vec() {
        let registry = PromptRegistry::new();
        let ctx = AssemblyContext {
            request_id: "test", memory: None, metadata: None,
            model: &make_resolved(8192),
        };
        assert!(registry.collect(&ctx).await.is_empty());
    }

    #[tokio::test]
    async fn system_prompt_produces_section() {
        let mut registry = PromptRegistry::new();
        registry.set_system_prompt("You are a helpful assistant.");
        let ctx = AssemblyContext {
            request_id: "test", memory: None, metadata: None,
            model: &make_resolved(8192),
        };
        let sections = registry.collect(&ctx).await;
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].layer, Layer::System);
    }

    #[tokio::test]
    async fn registered_provider_appears_in_collect() {
        let mut registry = PromptRegistry::new();
        registry.register(Box::new(TestProvider {
            layer: Layer::Tools, priority: 5,
            content: "tool definitions", tokens: 10,
        }));
        let ctx = AssemblyContext {
            request_id: "test", memory: None, metadata: None,
            model: &make_resolved(8192),
        };
        assert_eq!(registry.collect(&ctx).await.len(), 1);
    }

    #[tokio::test]
    async fn set_system_prompt_replaces_previous() {
        let mut registry = PromptRegistry::new();
        registry.set_system_prompt("first");
        registry.set_system_prompt("second");
        let ctx = AssemblyContext {
            request_id: "test", memory: None, metadata: None,
            model: &make_resolved(8192),
        };
        let sections = registry.collect(&ctx).await;
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].content, "second");
    }
}
