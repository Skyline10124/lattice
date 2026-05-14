use crate::provider::ContextProvider;
use crate::spec::{PromptProviderSpec, ProviderKind};

/// Factory that creates ContextProvider instances from a PromptProviderSpec.
///
/// Implementations live in the crate that owns the concrete provider
/// (e.g., MemoryFactory in lattice-agent).
pub trait ProviderFactory: Send + Sync {
    /// Which ProviderKind this factory handles.
    fn kind(&self) -> ProviderKind;

    /// Create a configured ContextProvider from the spec.
    /// The factory is responsible for:
    /// - Creating the provider with appropriate constructor args
    /// - Calling configure_from_spec(spec) to set layer/priority/budget/limit (D-08)
    /// - Setting provenance.provider to spec.id (D-09)
    fn create(&self, spec: &PromptProviderSpec) -> Box<dyn ContextProvider>;
}

/// Minimal trait abstracting tool definitions for ToolSchemaProvider.
/// Avoids direct dependency on lattice-tools from lattice-prompt.
pub trait ToolSchemaSource: Send + Sync {
    fn definitions(&self) -> Vec<lattice_core::types::ToolDefinition>;
}

/// Factory for ProjectInstructionsProvider (lattice-prompt crate).
pub struct ProjectInstructionsFactory;

impl ProviderFactory for ProjectInstructionsFactory {
    fn kind(&self) -> ProviderKind {
        ProviderKind::ProjectInstructions
    }

    fn create(&self, spec: &PromptProviderSpec) -> Box<dyn ContextProvider> {
        let mut provider =
            crate::project_instructions_provider::ProjectInstructionsProvider::from_root(
                std::env::current_dir().unwrap_or_default(),
            );
        provider.configure_from_spec(spec);
        Box::new(provider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{BudgetSpec, ProviderKind};
    use crate::types::Layer;

    #[test]
    fn project_instructions_factory_kind() {
        let factory = ProjectInstructionsFactory;
        assert!(matches!(factory.kind(), ProviderKind::ProjectInstructions));
    }

    #[test]
    fn project_instructions_factory_create() {
        let factory = ProjectInstructionsFactory;
        let spec = PromptProviderSpec {
            id: "test-pi".to_string(),
            kind: ProviderKind::ProjectInstructions,
            layer: Layer::Rules,
            channel: None,
            priority: 0,
            cache_scope: None,
            budget: BudgetSpec::Dynamic,
            limit: None,
        };
        let provider = factory.create(&spec);
        assert_eq!(provider.layer(), Layer::Rules);
        assert_eq!(provider.priority(), 0);
    }
}
