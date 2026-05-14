use std::sync::Arc;

use async_trait::async_trait;

use crate::provider::ContextProvider;
use crate::provider_factory::ToolSchemaSource;
use crate::spec::PromptProviderSpec;
use crate::types::{AssemblyContext, Layer, PromptSection, TokenBudget};

/// Provider that generates tool schema text from a ToolSchemaSource.
#[derive(Clone)]
pub struct ToolSchemaProvider {
    tool_source: Arc<dyn ToolSchemaSource>,
    layer: Layer,
    priority: u8,
    budget: TokenBudget,
    provenance_id: String,
    limit: Option<usize>,
}

impl ToolSchemaProvider {
    pub fn new(tool_source: Arc<dyn ToolSchemaSource>) -> Self {
        Self {
            tool_source,
            layer: Layer::Tools,
            priority: 5,
            budget: TokenBudget::Ratio(0.15),
            provenance_id: "tool-schema".to_string(),
            limit: None,
        }
    }
}

#[async_trait]
impl ContextProvider for ToolSchemaProvider {
    fn configure_from_spec(&mut self, spec: &PromptProviderSpec) {
        self.priority = spec.priority;
        self.budget = spec.budget.to_token_budget(512);
        self.limit = spec.limit;
        self.provenance_id = spec.id.clone(); // D-09
    }

    fn name(&self) -> &str {
        "tool-schema"
    }

    fn provenance_provider(&self) -> &str {
        &self.provenance_id
    }

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
        let definitions = self.tool_source.definitions();
        if definitions.is_empty() {
            return None;
        }
        let defs: Vec<_> = if let Some(limit) = self.limit {
            definitions.into_iter().take(limit).collect()
        } else {
            definitions
        };
        let mut content = String::from("Available tools:\n");
        for def in &defs {
            content.push_str(&format!(
                "- {}: {}\n  Parameters: {}\n",
                def.name, def.description, def.parameters
            ));
        }
        let tokens = lattice_core::tokens::TokenEstimator::estimate_text(&content);
        Some(PromptSection::new(
            content,
            self.layer,
            self.priority,
            tokens,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Layer;
    use lattice_core::types::ToolDefinition;
    use serde_json::json;

    struct MockToolSource {
        tools: Vec<ToolDefinition>,
    }

    impl MockToolSource {
        fn new(tools: Vec<ToolDefinition>) -> Self {
            Self { tools }
        }
    }

    impl ToolSchemaSource for MockToolSource {
        fn definitions(&self) -> Vec<ToolDefinition> {
            self.tools.clone()
        }
    }

    fn make_ctx() -> (lattice_core::ResolvedModel, String) {
        (
            lattice_core::ResolvedModel {
                canonical_id: "test".into(),
                provider: "test".into(),
                api_key: None,
                base_url: "".into(),
                api_protocol: lattice_core::catalog::ApiProtocol::OpenAiChat,
                api_model_id: "test".into(),
                context_length: 8192,
                provider_specific: std::collections::HashMap::new(),
                credential_status: lattice_core::CredentialStatus::Missing,
            },
            "test".to_string(),
        )
    }

    #[tokio::test]
    async fn produce_returns_none_when_no_tools() {
        let source = Arc::new(MockToolSource::new(vec![]));
        let provider = ToolSchemaProvider::new(source);
        let (model, _) = make_ctx();
        let ctx = AssemblyContext {
            request_id: "test",
            model: &model,
            user_input: "test",
        };
        assert!(provider.produce(&ctx).await.is_none());
    }

    #[tokio::test]
    async fn produce_returns_schema_for_tools() {
        let tools = vec![ToolDefinition::new(
            "read_file".into(),
            "Read a file".into(),
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )];
        let source = Arc::new(MockToolSource::new(tools));
        let provider = ToolSchemaProvider::new(source);
        let (model, _) = make_ctx();
        let ctx = AssemblyContext {
            request_id: "test",
            model: &model,
            user_input: "test",
        };
        let section = provider.produce(&ctx).await.unwrap();
        assert_eq!(section.layer, Layer::Tools);
        assert!(section.content.contains("read_file"));
        assert!(section.content.contains("Read a file"));
    }

    #[test]
    fn configure_from_spec_overrides_priority_and_budget() {
        let source = Arc::new(MockToolSource::new(vec![]));
        let mut provider = ToolSchemaProvider::new(source);
        assert_eq!(provider.priority(), 5);

        let spec = PromptProviderSpec {
            id: "custom-tools".to_string(),
            kind: crate::spec::ProviderKind::ToolSchema,
            layer: Layer::Tools,
            channel: None,
            priority: 10,
            cache_scope: None,
            budget: crate::spec::BudgetSpec::Ratio { value: 0.25 },
            limit: Some(5),
        };
        provider.configure_from_spec(&spec);
        assert_eq!(provider.priority(), 10);
        assert_eq!(provider.budget(), TokenBudget::Ratio(0.25));
        assert_eq!(provider.provenance_id, "custom-tools");
    }
}
