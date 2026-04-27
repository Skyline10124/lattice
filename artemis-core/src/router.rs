use std::collections::HashMap;
use crate::catalog::{Catalog, CatalogProviderEntry, ModelCatalogEntry, ResolvedModel};
use crate::errors::ArtemisError;

/// Multi-provider credential fallback map.
/// Maps provider slugs to env var → field name mappings.
/// Used when a provider entry's credential_keys is empty or needs
/// supplementary env var lookups (e.g. openrouter which isn't in provider_defaults).
pub const _PROVIDER_CREDENTIALS: &[(&str, &[(&str, &str)])] = &[
    ("openrouter", &[("OPENROUTER_API_KEY", "api_key")]),
    ("anthropic", &[("ANTHROPIC_API_KEY", "api_key")]),
    ("openai", &[("OPENAI_API_KEY", "api_key")]),
    ("gemini", &[("GEMINI_API_KEY", "api_key")]),
    ("deepseek", &[("DEEPSEEK_API_KEY", "api_key")]),
    ("groq", &[("GROQ_API_KEY", "api_key")]),
    ("mistral", &[("MISTRAL_API_KEY", "api_key")]),
    ("xai", &[("XAI_API_KEY", "api_key")]),
    ("ollama", &[]),
    ("nous", &[("NOUS_API_KEY", "api_key")]),
    ("copilot", &[("GITHUB_TOKEN", "token")]),
    ("opencode-zen", &[("OPENCODE_ZEN_API_KEY", "api_key")]),
    ("kilocode", &[("KILO_API_KEY", "api_key")]),
    ("ai-gateway", &[("AI_GATEWAY_API_KEY", "api_key")]),
    ("openai-codex", &[("OPENAI_API_KEY", "api_key")]),
    ("bedrock", &[]),
    ("minimax", &[("MINIMAX_API_KEY", "api_key")]),
    ("qwen", &[("QWEN_API_KEY", "api_key")]),
    ("volces", &[("ARK_API_KEY", "api_key")]),
    ("infini-ai", &[("INFINI_AI_API_KEY", "api_key")]),
    ("opencode-go", &[("OPENCODE_GO_API_KEY", "api_key")]),
];

/// Normalize a model ID string:
/// - Strip OpenRouter vendor prefixes (e.g. "anthropic/claude-sonnet-4.6" → "claude-sonnet-4.6")
/// - Strip Bedrock inference profile prefixes (e.g. "us.anthropic.claude-sonnet-4-6-v1:0" → "claude-sonnet-4-6")
/// - Strip Bedrock version suffixes (-v1:0, -v1)
/// - Normalize Claude dots to hyphens (claude-sonnet-4.6 → claude-sonnet-4-6)
pub fn normalize_model_id(model_id: &str) -> String {
    let mid = model_id.to_lowercase();

    let mid = if let Some((_prefix, rest)) = mid.split_once('/') {
        rest.to_string()
    } else {
        mid
    };

    let mid = mid.trim_start_matches("us.anthropic.").to_string();
    let mid = mid.trim_start_matches("us.amazon.").to_string();
    let mid = mid.trim_start_matches("us.meta.").to_string();

    let mid = regex::Regex::new(r"-v\d+(:\d+)?$")
        .unwrap()
        .replace(&mid, "")
        .to_string();

    if mid.starts_with("claude-") {
        return regex::Regex::new(r"(\d+)\.(\d+)")
            .unwrap()
            .replace_all(&mid, "$1-$2")
            .to_string();
    }

    mid
}

/// The model-centric request router.
/// Resolves model names → ResolvedModel with connection details.
pub struct ModelRouter {
    catalog: &'static Catalog,
    custom_models: HashMap<String, ModelCatalogEntry>,
}

impl ModelRouter {
    pub fn new() -> Self {
        ModelRouter {
            catalog: Catalog::get(),
            custom_models: HashMap::new(),
        }
    }

    /// Core resolution pipeline:
    /// 1. normalize_model_id(model_name)
    /// 2. resolve_alias → canonical_id
    /// 3. catalog.get_model(canonical_id) or custom_models
    /// 4. provider_override → find specific provider, or priority-sorted iteration
    /// 5. resolve_credentials per provider entry (env var check)
    /// 6. If all fail, try permissive fallback
    /// 7. If even permissive fails, return ModelNotFound error
    pub fn resolve(
        &self,
        model_name: &str,
        provider_override: Option<&str>,
    ) -> Result<ResolvedModel, ArtemisError> {
        let normalized = normalize_model_id(model_name);

        let canonical_id = match self.resolve_alias(&normalized) {
            Some(id) => id,
            None => {
                if self.catalog.get_model(&normalized).is_some() {
                    normalized.clone()
                } else if self.custom_models.contains_key(&normalized) {
                    normalized.clone()
                } else {
                    return self.resolve_permissive(model_name);
                }
            }
        };

        let entry = self
            .catalog
            .get_model(&canonical_id)
            .cloned()
            .or_else(|| self.custom_models.get(&canonical_id).cloned());

        let entry = match entry {
            Some(e) => e,
            None => return self.resolve_permissive(model_name),
        };

        if let Some(override_provider) = provider_override {
            for pe in &entry.providers {
                if pe.provider_id == override_provider {
                    let api_key = self.resolve_credentials(pe);
                    return Ok(ResolvedModel {
                        canonical_id: canonical_id.clone(),
                        provider: pe.provider_id.clone(),
                        api_key,
                        base_url: pe.base_url.clone().unwrap_or_default(),
                        api_protocol: pe.api_protocol.clone(),
                        api_model_id: pe.api_model_id.clone(),
                        context_length: entry.context_length,
                        provider_specific: pe.provider_specific.clone(),
                    });
                }
            }
            return Err(ArtemisError::ModelNotFound {
                model: format!(
                    "provider '{}' not found for model '{}'",
                    override_provider, canonical_id
                ),
            });
        }

        let mut sorted_providers = entry.providers.clone();
        sorted_providers.sort_by_key(|p| p.priority);

        for pe in &sorted_providers {
            let api_key = self.resolve_credentials(pe);
            if api_key.is_some() {
                return Ok(ResolvedModel {
                    canonical_id: canonical_id.clone(),
                    provider: pe.provider_id.clone(),
                    api_key,
                    base_url: pe.base_url.clone().unwrap_or_default(),
                    api_protocol: pe.api_protocol.clone(),
                    api_model_id: pe.api_model_id.clone(),
                    context_length: entry.context_length,
                    provider_specific: pe.provider_specific.clone(),
                });
            }
        }

        self.resolve_permissive(model_name)
    }

    /// Check env vars for a provider entry's credential_keys.
    /// Returns the first env var value found, or None.
    fn resolve_credentials(&self, entry: &CatalogProviderEntry) -> Option<String> {
        for (_field_name, env_var) in &entry.credential_keys {
            if let Ok(val) = std::env::var(env_var) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }

        let provider_id = &entry.provider_id;
        for (slug, creds) in _PROVIDER_CREDENTIALS {
            if *slug == *provider_id {
                for (env_var, _field_name) in *creds {
                    if let Ok(val) = std::env::var(env_var) {
                        let trimmed = val.trim().to_string();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
                break;
            }
        }

        None
    }

    /// Normalize a user-provided model string to a canonical ID.
    ///
    /// Checks catalog aliases, catalog model keys, custom models, and applies
    /// normalize_model_id() before checking.
    pub fn resolve_alias(&self, name: &str) -> Option<String> {
        let normalized = normalize_model_id(name);

        if let Some(canonical) = self.catalog.resolve_alias(&normalized) {
            return Some(canonical.clone());
        }

        if self.catalog.get_model(&normalized).is_some() {
            return Some(normalized);
        }

        if self.custom_models.contains_key(&normalized) {
            return Some(normalized);
        }

        for (canonical_id, entry) in &self.custom_models {
            for alias in &entry.aliases {
                if *alias == normalized {
                    return Some(canonical_id.clone());
                }
            }
        }

        None
    }

    /// Permissive fallback for models not in the catalog.
    ///
    /// Tries "provider/model" split, looks up provider defaults,
    /// and constructs a ResolvedModel from the defaults.
    pub fn resolve_permissive(
        &self,
        model_name: &str,
    ) -> Result<ResolvedModel, ArtemisError> {
        if let Some((provider_part, model_part)) = model_name.split_once('/') {
            if let Some(defaults) = self.catalog.get_provider_defaults(provider_part) {
                let api_key = self.resolve_credentials(&CatalogProviderEntry {
                    provider_id: provider_part.to_string(),
                    api_model_id: model_part.to_string(),
                    priority: 1,
                    weight: 1,
                    credential_keys: defaults.credential_keys.clone(),
                    base_url: Some(defaults.base_url.clone()),
                    api_protocol: defaults.api_protocol.clone(),
                    provider_specific: HashMap::new(),
                });

                return Ok(ResolvedModel {
                    canonical_id: model_name.to_string(),
                    provider: provider_part.to_string(),
                    api_key,
                    base_url: defaults.base_url.clone(),
                    api_protocol: defaults.api_protocol.clone(),
                    api_model_id: model_part.to_string(),
                    context_length: 131072,
                    provider_specific: HashMap::new(),
                });
            }
        }

        Err(ArtemisError::ModelNotFound {
            model: model_name.to_string(),
        })
    }

    /// Register a custom model at runtime (Python-facing API).
    pub fn register_model(&mut self, entry: ModelCatalogEntry) {
        self.custom_models
            .insert(entry.canonical_id.clone(), entry);
    }

    /// List all canonical model IDs (catalog + custom).
    pub fn list_models(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .catalog
            .list_models()
            .iter()
            .map(|s| (*s).clone())
            .collect();
        for id in self.custom_models.keys() {
            ids.push(id.clone());
        }
        ids.sort();
        ids
    }

    /// List models that have at least one provider with valid credentials.
    pub fn list_authenticated_models(&self) -> Vec<String> {
        let mut authenticated = Vec::new();

        for model_id in self.catalog.list_models() {
            if let Some(entry) = self.catalog.get_model(model_id) {
                for pe in &entry.providers {
                    if self.resolve_credentials(pe).is_some() {
                        authenticated.push(model_id.clone());
                        break;
                    }
                }
            }
        }

        for (model_id, entry) in &self.custom_models {
            for pe in &entry.providers {
                if self.resolve_credentials(pe).is_some() {
                    authenticated.push(model_id.clone());
                    break;
                }
            }
        }

        authenticated.sort();
        authenticated
    }

    /// Normalize a canonical model ID to the provider-specific api_model_id.
    /// Most models use the canonical_id directly, but some providers need prefixes or transformations.
    pub fn normalize_model_for_provider(&self, canonical_id: &str, provider_id: &str) -> String {
        if let Some(entry) = self.catalog.get_model(canonical_id) {
            for pe in &entry.providers {
                if pe.provider_id == provider_id {
                    return pe.api_model_id.clone();
                }
            }
        }
        canonical_id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ApiProtocol;
    use std::env;
    use std::sync::{LazyLock, Mutex};

    static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn save_env(key: &str) -> Option<String> {
        env::var(key).ok()
    }

    fn restore_env(key: &str, prev: Option<String>) {
        match prev {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
    }

    #[test]
    fn test_normalize_model_id_openrouter_prefix() {
        assert_eq!(
            normalize_model_id("anthropic/claude-sonnet-4.6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(normalize_model_id("openai/gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_normalize_model_id_bedrock_prefix_and_suffix() {
        assert_eq!(
            normalize_model_id("us.anthropic.claude-sonnet-4-6-v1:0"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            normalize_model_id("us.amazon.nova-pro-v1:0"),
            "nova-pro"
        );
        assert_eq!(
            normalize_model_id("us.meta.llama4-maverick-17b-instruct-v1:0"),
            "llama4-maverick-17b-instruct"
        );
    }

    #[test]
    fn test_normalize_model_id_bedrock_suffix_only() {
        assert_eq!(
            normalize_model_id("claude-sonnet-4-6-v1:0"),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn test_normalize_model_id_bedrock_suffix_no_colon() {
        assert_eq!(
            normalize_model_id("claude-sonnet-4-6-v1"),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn test_normalize_model_id_claude_dots_to_hyphens() {
        assert_eq!(
            normalize_model_id("claude-sonnet-4.6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            normalize_model_id("claude-opus-4.7"),
            "claude-opus-4-7"
        );
        assert_eq!(
            normalize_model_id("claude-haiku-4.5"),
            "claude-haiku-4-5"
        );
    }

    #[test]
    fn test_normalize_model_id_noop() {
        assert_eq!(normalize_model_id("gpt-4o"), "gpt-4o");
        assert_eq!(normalize_model_id("deepseek-v4-pro"), "deepseek-v4-pro");
        assert_eq!(
            normalize_model_id("gemini-3-pro-preview"),
            "gemini-3-pro-preview"
        );
    }

    #[test]
    fn test_normalize_model_id_lowercase() {
        assert_eq!(normalize_model_id("GPT-4O"), "gpt-4o");
        assert_eq!(
            normalize_model_id("ANTHROPIC/CLAUDE-SONNET-4.6"),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn test_resolve_alias_sonnet() {
        let router = ModelRouter::new();
        let alias = router.resolve_alias("sonnet");
        assert_eq!(alias, Some("claude-sonnet-4-6".to_string()));
    }

    #[test]
    fn test_resolve_alias_gpt5() {
        let router = ModelRouter::new();
        let alias = router.resolve_alias("gpt5");
        assert_eq!(alias, Some("gpt-5.4".to_string()));
    }

    #[test]
    fn test_resolve_alias_deepseek() {
        let router = ModelRouter::new();
        let alias = router.resolve_alias("deepseek");
        assert_eq!(alias, Some("deepseek-v4-pro".to_string()));
    }

    #[test]
    fn test_resolve_alias_nonexistent() {
        let router = ModelRouter::new();
        let alias = router.resolve_alias("nonexistent-model-xyz");
        assert_eq!(alias, None);
    }

    #[test]
    fn test_alias_resolution_chain() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev = save_env("ANTHROPIC_API_KEY");
        env::set_var("ANTHROPIC_API_KEY", "test-key-ant");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("sonnet", None)
            .expect("should resolve sonnet alias");
        assert_eq!(resolved.canonical_id, "claude-sonnet-4-6");
        assert_eq!(resolved.provider, "anthropic");
        assert!(resolved.api_key.is_some(), "should have api_key from env var");
        assert_eq!(resolved.api_key.unwrap(), "test-key-ant");

        restore_env("ANTHROPIC_API_KEY", prev);
    }

    #[test]
    fn test_priority_iteration() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev_gh = save_env("GITHUB_TOKEN");
        let prev_ant = save_env("ANTHROPIC_API_KEY");
        let prev_nous = save_env("NOUS_API_KEY");
        let prev_zen = save_env("OPENCODE_ZEN_API_KEY");
        let prev_kilo = save_env("KILO_API_KEY");
        let prev_gw = save_env("AI_GATEWAY_API_KEY");

        env::remove_var("ANTHROPIC_API_KEY");
        env::remove_var("NOUS_API_KEY");
        env::remove_var("OPENCODE_ZEN_API_KEY");
        env::remove_var("KILO_API_KEY");
        env::remove_var("AI_GATEWAY_API_KEY");
        env::set_var("GITHUB_TOKEN", "gh-test-token");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("claude-sonnet-4-6", None)
            .expect("should resolve");
        assert_eq!(resolved.provider, "copilot");
        assert_eq!(resolved.api_key.as_deref(), Some("gh-test-token"));

        restore_env("GITHUB_TOKEN", prev_gh);
        restore_env("ANTHROPIC_API_KEY", prev_ant);
        restore_env("NOUS_API_KEY", prev_nous);
        restore_env("OPENCODE_ZEN_API_KEY", prev_zen);
        restore_env("KILO_API_KEY", prev_kilo);
        restore_env("AI_GATEWAY_API_KEY", prev_gw);
    }

    #[test]
    fn test_gpt4o_resolves() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev = save_env("OPENAI_API_KEY");
        env::set_var("OPENAI_API_KEY", "sk-test");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("gpt-4o", None)
            .expect("should resolve gpt-4o");
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.api_protocol, ApiProtocol::OpenAiChat);
        assert_eq!(resolved.api_model_id, "gpt-4o");

        restore_env("OPENAI_API_KEY", prev);
    }

    #[test]
    fn test_permissive_fallback_openrouter() {
        let router = ModelRouter::new();
        let resolved = router.resolve_permissive("anthropic/claude-sonnet-4.6");
        assert!(resolved.is_ok(), "permissive fallback should work");
        let r = resolved.unwrap();
        assert_eq!(r.provider, "anthropic");
        assert_eq!(r.api_model_id, "claude-sonnet-4.6");
        assert_eq!(r.api_protocol, ApiProtocol::AnthropicMessages);
    }

    #[test]
    fn test_permissive_fallback_anthropic_direct() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev = save_env("ANTHROPIC_API_KEY");
        env::set_var("ANTHROPIC_API_KEY", "ant-key");

        let router = ModelRouter::new();
        let resolved = router.resolve_permissive("anthropic/claude-sonnet-4.6");
        assert!(resolved.is_ok());
        let r = resolved.unwrap();
        assert_eq!(r.provider, "anthropic");
        assert_eq!(r.api_key.as_deref(), Some("ant-key"));
        assert_eq!(r.api_protocol, ApiProtocol::AnthropicMessages);
        assert_eq!(r.base_url, "https://api.anthropic.com");
        assert_eq!(r.api_model_id, "claude-sonnet-4.6");

        restore_env("ANTHROPIC_API_KEY", prev);
    }

    #[test]
    fn test_permissive_fallback_nonexistent_provider() {
        let router = ModelRouter::new();
        let resolved = router.resolve_permissive("nonexistent/model");
        assert!(resolved.is_err());
    }

    #[test]
    fn test_exhaustion_no_credentials() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev_keys: Vec<(String, Option<String>)> = [
            "ANTHROPIC_API_KEY",
            "NOUS_API_KEY",
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "KILO_API_KEY",
            "AI_GATEWAY_API_KEY",
        ]
        .iter()
        .map(|k| (k.to_string(), save_env(k)))
        .collect();

        for k in [
            "ANTHROPIC_API_KEY",
            "NOUS_API_KEY",
            "GITHUB_TOKEN",
            "OPENAI_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "KILO_API_KEY",
            "AI_GATEWAY_API_KEY",
        ] {
            env::remove_var(k);
        }

        let router = ModelRouter::new();
        let result = router.resolve("claude-sonnet-4-6", None);
        assert!(
            result.is_err(),
            "Expected error when no credentials available, got: {:?}",
            result
        );

        for (k, v) in prev_keys {
            restore_env(&k, v);
        }
    }

    #[test]
    fn test_custom_registration() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let mut router = ModelRouter::new();
        let custom = ModelCatalogEntry {
            canonical_id: "my-custom-model".to_string(),
            display_name: "My Custom Model".to_string(),
            description: String::new(),
            context_length: 8192,
            capabilities: vec![],
            providers: vec![CatalogProviderEntry {
                provider_id: "custom".to_string(),
                api_model_id: "my-model".to_string(),
                priority: 1,
                weight: 1,
                credential_keys: HashMap::from([(
                    "api_key".to_string(),
                    "MY_CUSTOM_KEY".to_string(),
                )]),
                base_url: Some("http://localhost:8080/v1".to_string()),
                api_protocol: ApiProtocol::OpenAiChat,
                provider_specific: HashMap::new(),
            }],
            aliases: vec!["mymodel".to_string()],
        };
        router.register_model(custom);

        assert!(
            router.list_models().contains(&"my-custom-model".to_string()),
            "list_models should include custom model"
        );

        let prev = save_env("MY_CUSTOM_KEY");
        env::set_var("MY_CUSTOM_KEY", "custom-key");
        let resolved = router
            .resolve("my-custom-model", None)
            .expect("should resolve custom model");
        assert_eq!(resolved.api_model_id, "my-model");
        assert_eq!(resolved.base_url, "http://localhost:8080/v1");
        assert_eq!(resolved.api_key.as_deref(), Some("custom-key"));

        let resolved_alias = router
            .resolve("mymodel", None)
            .expect("should resolve via alias after normalization");
        assert_eq!(resolved_alias.canonical_id, "my-custom-model");

        restore_env("MY_CUSTOM_KEY", prev);
    }

    #[test]
    fn test_list_models_includes_catalog() {
        let router = ModelRouter::new();
        let models = router.list_models();
        assert!(
            models.contains(&"claude-sonnet-4-6".to_string()),
            "should include claude-sonnet-4-6"
        );
        assert!(
            models.contains(&"gpt-4o".to_string()),
            "should include gpt-4o"
        );
    }

    #[test]
    fn test_list_authenticated_models() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev_ant = save_env("ANTHROPIC_API_KEY");
        let prev_oai = save_env("OPENAI_API_KEY");
        let prev_nous = save_env("NOUS_API_KEY");
        let prev_gh = save_env("GITHUB_TOKEN");

        env::set_var("ANTHROPIC_API_KEY", "test-ant");
        env::remove_var("OPENAI_API_KEY");
        env::remove_var("NOUS_API_KEY");
        env::remove_var("GITHUB_TOKEN");

        let router = ModelRouter::new();
        let authed = router.list_authenticated_models();

        assert!(
            authed.contains(&"claude-sonnet-4-6".to_string()),
            "claude-sonnet-4-6 should be authenticated with ANTHROPIC_API_KEY set"
        );

        restore_env("ANTHROPIC_API_KEY", prev_ant);
        restore_env("OPENAI_API_KEY", prev_oai);
        restore_env("NOUS_API_KEY", prev_nous);
        restore_env("GITHUB_TOKEN", prev_gh);
    }

    #[test]
    fn test_provider_override() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev_ant = save_env("ANTHROPIC_API_KEY");
        let prev_nous = save_env("NOUS_API_KEY");

        env::remove_var("ANTHROPIC_API_KEY");
        env::set_var("NOUS_API_KEY", "nous-key");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("claude-sonnet-4-6", Some("anthropic"))
            .expect("should resolve with provider override");
        assert_eq!(resolved.provider, "anthropic");

        restore_env("ANTHROPIC_API_KEY", prev_ant);
        restore_env("NOUS_API_KEY", prev_nous);
    }

    #[test]
    fn test_resolve_with_normalized_name() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev = save_env("ANTHROPIC_API_KEY");
        env::set_var("ANTHROPIC_API_KEY", "test-ant");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("claude-sonnet-4.6", None)
            .expect("should resolve normalized name");
        assert_eq!(resolved.canonical_id, "claude-sonnet-4-6");

        restore_env("ANTHROPIC_API_KEY", prev);
    }

    #[test]
    fn test_resolve_deepseek_with_direct_key() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let prev = save_env("DEEPSEEK_API_KEY");
        env::set_var("DEEPSEEK_API_KEY", "ds-key");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("deepseek-v4-pro", None)
            .expect("should resolve deepseek-v4-pro");
        assert_eq!(resolved.provider, "deepseek");
        assert_eq!(resolved.api_key.as_deref(), Some("ds-key"));

        restore_env("DEEPSEEK_API_KEY", prev);
    }

    #[test]
    fn test_normalize_model_id_empty() {
        assert_eq!(normalize_model_id(""), "");
    }

    #[test]
    fn test_normalize_model_id_double_slash() {
        let result = normalize_model_id("openrouter/anthropic/claude");
        assert!(!result.contains("openrouter"));
    }

    #[test]
    fn test_normalize_model_for_provider() {
        let router = ModelRouter::new();
        // claude-sonnet-4-6 can be served by multiple providers with different api_model_ids
        let result = router.normalize_model_for_provider("claude-sonnet-4-6", "nous");
        assert_eq!(result, "anthropic/claude-sonnet-4.6"); // nous uses openrouter-style prefixes
    }
}
