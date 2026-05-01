use super::types::*;
use crate::errors::LatticeError;
use std::collections::HashMap;
use std::sync::OnceLock;

pub struct Catalog {
    models: HashMap<String, ModelCatalogEntry>,
    aliases: HashMap<String, String>,
    provider_defaults: HashMap<String, ProviderDefaults>,
}

static CATALOG: OnceLock<Result<Catalog, LatticeError>> = OnceLock::new();

impl Catalog {
    /// Returns the global catalog, loading and deserializing on first access.
    ///
    /// The catalog is embedded at compile time via `include_str!("data.json")`.
    /// A deserialization failure indicates a corrupt binary and returns a
    /// `ConfigError` instead of panicking.
    pub fn get() -> Result<&'static Catalog, LatticeError> {
        CATALOG
            .get_or_init(|| {
                let data = include_str!("data.json");
                serde_json::from_str(data)
                    .map(Catalog::from_data)
                    .map_err(|e| LatticeError::Config {
                        message: format!("Failed to deserialize catalog data.json: {e}"),
                    })
            })
            .as_ref()
            .map_err(|e| e.clone())
    }

    fn from_data(data: CatalogData) -> Self {
        let mut models: HashMap<String, ModelCatalogEntry> = HashMap::new();
        for m in data.models {
            if models.contains_key(&m.canonical_id) {
                tracing::warn!(
                    "catalog: duplicate canonical_id '{}', later entry overwrites earlier",
                    m.canonical_id
                );
            }
            models.insert(m.canonical_id.clone(), m);
        }
        Catalog {
            models,
            aliases: data.aliases,
            provider_defaults: data.provider_defaults,
        }
    }

    pub fn get_model(&self, canonical_id: &str) -> Option<&ModelCatalogEntry> {
        self.models.get(canonical_id)
    }

    pub fn list_models(&self) -> Vec<&String> {
        self.models.keys().collect()
    }

    pub fn get_provider_defaults(&self, provider_id: &str) -> Option<&ProviderDefaults> {
        self.provider_defaults.get(provider_id)
    }

    pub fn resolve_alias(&self, alias: &str) -> Option<&String> {
        self.aliases.get(alias)
    }

    pub fn aliases(&self) -> &HashMap<String, String> {
        &self.aliases
    }
    pub fn model_count(&self) -> usize {
        self.models.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_catalog_loads() {
        let catalog = Catalog::get().expect("catalog should load from embedded data.json");
        assert!(
            catalog.model_count() > 50,
            "Expected >50 models, got {}",
            catalog.model_count()
        );
    }

    #[test]
    fn test_get_model_claude_sonnet() {
        let catalog = Catalog::get().expect("catalog should load from embedded data.json");
        let model = catalog
            .get_model("claude-sonnet-4-6")
            .expect("claude-sonnet-4-6 should exist in catalog");
        assert!(
            !model.providers.is_empty(),
            "claude-sonnet-4-6 should have providers"
        );
        assert_eq!(
            model.providers[0].provider_id, "nous",
            "First provider should be nous (highest priority)"
        );
    }

    #[test]
    fn test_resolve_alias_sonnet() {
        let catalog = Catalog::get().expect("catalog should load from embedded data.json");
        let resolved = catalog.resolve_alias("sonnet");
        assert!(resolved.is_some(), "alias 'sonnet' should resolve");
    }

    #[test]
    fn test_provider_defaults_anthropic() {
        let catalog = Catalog::get().expect("catalog should load from embedded data.json");
        let defaults = catalog.get_provider_defaults("anthropic");
        assert!(
            defaults.is_some(),
            "anthropic should have provider defaults"
        );
    }

    #[test]
    fn test_api_protocol_from_str() {
        assert_eq!(
            "chat_completions".parse::<ApiProtocol>().unwrap(),
            ApiProtocol::OpenAiChat
        );
        assert_eq!(
            "anthropic".parse::<ApiProtocol>().unwrap(),
            ApiProtocol::AnthropicMessages
        );
        assert_eq!(
            "gemini".parse::<ApiProtocol>().unwrap(),
            ApiProtocol::GeminiGenerateContent
        );
    }

    #[test]
    fn test_api_protocol_custom_variant() {
        let custom: ApiProtocol = "acp".parse().unwrap();
        assert_eq!(custom, ApiProtocol::Custom("acp".to_string()));
    }
}
