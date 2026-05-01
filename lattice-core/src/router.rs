use crate::catalog::{
    Catalog, CatalogProviderEntry, CredentialStatus, ModelCatalogEntry, ResolvedModel,
};
use crate::errors::LatticeError;
use std::collections::HashMap;
use std::sync::Mutex;

/// Multi-provider credential fallback map.
/// Maps provider slugs to env var → field name mappings.
/// Used when a provider entry's credential_keys is empty or needs
/// supplementary env var lookups (e.g. openrouter which isn't in provider_defaults).
pub const PROVIDER_CREDENTIALS_RAW: &[(&str, &[(&str, &str)])] = &[
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
    ("minimax", &[("MINIMAX_API_KEY", "api_key")]),
    ("qwen", &[("QWEN_API_KEY", "api_key")]),
    ("volces", &[("ARK_API_KEY", "api_key")]),
    ("infini-ai", &[("INFINI_AI_API_KEY", "api_key")]),
    ("opencode-go", &[("OPENCODE_GO_API_KEY", "api_key")]),
];

/// HashMap-based O(1) lookup over PROVIDER_CREDENTIALS_RAW, built once at first access.
static PROVIDER_CREDENTIALS_MAP: std::sync::LazyLock<
    HashMap<&'static str, &'static [(&'static str, &'static str)]>,
> = std::sync::LazyLock::new(|| {
    let mut map = HashMap::new();
    for (slug, creds) in PROVIDER_CREDENTIALS_RAW {
        map.insert(*slug, *creds);
    }
    map
});

static RE_SUFFIX: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"-v\d+(:\d+)?$").unwrap());
static RE_DOTS: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"(\d+)\.(\d+)").unwrap());

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

    let mid = mid.trim_start_matches("us.anthropic.");
    let mid = mid.trim_start_matches("us.amazon.");
    let mid = mid.trim_start_matches("us.meta.");

    let mid = RE_SUFFIX.replace(mid, "").into_owned();

    if mid.starts_with("claude-") {
        let replaced = RE_DOTS.replace_all(&mid, "$1-$2");
        return if matches!(replaced, std::borrow::Cow::Owned(_)) {
            replaced.into_owned()
        } else {
            mid.to_string()
        };
    }

    mid
}

/// The model-centric request router.
/// Resolves model names → ResolvedModel with connection details.
pub struct ModelRouter {
    catalog: &'static Catalog,
    custom_models: HashMap<String, ModelCatalogEntry>,
    credential_cache: Mutex<HashMap<(String, String), Option<String>>>,
    /// Nix Phase 1: externally supplied credentials.
    /// When non-empty, these take priority over env var lookups.
    external_credentials: HashMap<String, String>,
}

impl Default for ModelRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRouter {
    pub fn new() -> Self {
        ModelRouter {
            catalog: Catalog::get().expect("catalog data.json is embedded at compile time"),
            custom_models: HashMap::new(),
            credential_cache: Mutex::new(HashMap::new()),
            external_credentials: HashMap::new(),
        }
    }

    /// Create a router with externally-supplied credentials.
    /// This avoids the need for `std::env::set_var` injection
    /// and makes `resolve()` a pure function w.r.t. the provided creds.
    pub fn with_credentials(creds: HashMap<String, String>) -> Self {
        let mut router = Self::new();
        router.external_credentials = creds;
        router
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
    ) -> Result<ResolvedModel, LatticeError> {
        let normalized = normalize_model_id(model_name);

        let canonical_id = match self.resolve_alias(&normalized) {
            Some(id) => id,
            None => {
                if self.catalog.get_model(&normalized).is_some()
                    || self.custom_models.contains_key(&normalized)
                {
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
                    let credential_status = Self::credential_status_from_key(&api_key, pe);
                    return Ok(ResolvedModel {
                        canonical_id: canonical_id.clone(),
                        provider: pe.provider_id.clone(),
                        api_key,
                        base_url: self.resolve_base_url(&pe.provider_id, &pe.base_url),
                        api_protocol: pe.api_protocol.clone(),
                        api_model_id: pe.api_model_id.clone(),
                        context_length: entry.context_length,
                        provider_specific: pe.provider_specific.clone(),
                        credential_status,
                    });
                }
            }
            return Err(LatticeError::ModelNotFound {
                model: format!(
                    "provider '{}' not found for model '{}'",
                    override_provider, canonical_id
                ),
            });
        }

        let mut sorted_providers = entry.providers.clone();
        sorted_providers.sort_by_key(|p| p.priority);

        let mut best_credentialless: Option<&CatalogProviderEntry> = None;
        let mut current_priority = u32::MAX;

        for pe in &sorted_providers {
            if pe.priority != current_priority {
                if let Some(cpe) = best_credentialless.take() {
                    return Ok(ResolvedModel {
                        canonical_id: canonical_id.clone(),
                        provider: cpe.provider_id.clone(),
                        api_key: None,
                        base_url: self.resolve_base_url(&cpe.provider_id, &cpe.base_url),
                        api_protocol: cpe.api_protocol.clone(),
                        api_model_id: cpe.api_model_id.clone(),
                        context_length: entry.context_length,
                        provider_specific: cpe.provider_specific.clone(),
                        credential_status: CredentialStatus::NotRequired,
                    });
                }
                current_priority = pe.priority;
            }

            if Self::is_credentialless(pe) {
                if best_credentialless.is_none() {
                    best_credentialless = Some(pe);
                }
                continue;
            }

            let api_key = self.resolve_credentials(pe);
            if api_key.is_some() {
                return Ok(ResolvedModel {
                    canonical_id: canonical_id.clone(),
                    provider: pe.provider_id.clone(),
                    api_key,
                    base_url: self.resolve_base_url(&pe.provider_id, &pe.base_url),
                    api_protocol: pe.api_protocol.clone(),
                    api_model_id: pe.api_model_id.clone(),
                    context_length: entry.context_length,
                    provider_specific: pe.provider_specific.clone(),
                    credential_status: CredentialStatus::Present,
                });
            }
        }

        if let Some(cpe) = best_credentialless.take() {
            return Ok(ResolvedModel {
                canonical_id: canonical_id.clone(),
                provider: cpe.provider_id.clone(),
                api_key: None,
                base_url: self.resolve_base_url(&cpe.provider_id, &cpe.base_url),
                api_protocol: cpe.api_protocol.clone(),
                api_model_id: cpe.api_model_id.clone(),
                context_length: entry.context_length,
                provider_specific: cpe.provider_specific.clone(),
                credential_status: CredentialStatus::NotRequired,
            });
        }

        if sorted_providers.is_empty() {
            return Err(LatticeError::Config {
                message: format!("no providers available for model '{}'", canonical_id),
            });
        }

        let best = &sorted_providers[0];
        if !Self::is_credentialless(best) {
            let keys: Vec<&str> = PROVIDER_CREDENTIALS_RAW
                .iter()
                .filter(|(s, _)| *s == best.provider_id)
                .flat_map(|(_, creds)| creds.iter().map(|(ev, _)| *ev))
                .collect();
            let hint = if keys.is_empty() {
                format!("provider '{}' requires a credential", best.provider_id)
            } else {
                format!(
                    "provider '{}' requires one of: {}",
                    best.provider_id,
                    keys.join(", ")
                )
            };
            return Err(LatticeError::Config { message: hint });
        }
        Ok(ResolvedModel {
            canonical_id: canonical_id.clone(),
            provider: best.provider_id.clone(),
            api_key: None,
            base_url: self.resolve_base_url(&best.provider_id, &best.base_url),
            api_protocol: best.api_protocol.clone(),
            api_model_id: best.api_model_id.clone(),
            context_length: entry.context_length,
            provider_specific: best.provider_specific.clone(),
            credential_status: CredentialStatus::NotRequired,
        })
    }

    /// Determine the credential status from an api_key and provider entry.
    fn credential_status_from_key(
        api_key: &Option<String>,
        entry: &CatalogProviderEntry,
    ) -> CredentialStatus {
        if api_key.is_some() {
            CredentialStatus::Present
        } else if Self::is_credentialless(entry) {
            CredentialStatus::NotRequired
        } else {
            CredentialStatus::Missing
        }
    }

    /// Check whether a provider entry requires no credentials at all.
    ///
    /// A provider is credentialless when:
    /// - Its `credential_keys` map is empty AND
    /// - It has no credential entries in `PROVIDER_CREDENTIALS_RAW` (or its entry is `&[]`)
    fn is_credentialless(entry: &CatalogProviderEntry) -> bool {
        if !entry.credential_keys.is_empty() {
            return false;
        }
        match PROVIDER_CREDENTIALS_MAP.get(entry.provider_id.as_str()) {
            Some(creds) => creds.is_empty(),
            // Unknown provider: require authentication, don't assume credentialless
            None => false,
        }
    }

    /// Check env vars for a provider entry's credential_keys.
    /// Returns the first env var value found, or None.
    /// Results are cached per provider_id to avoid repeated env var lookups.
    /// Build a cache key from provider_id and a fingerprint of credential_keys
    /// to prevent cache pollution when two entries share the same provider_id
    /// but have different credential env var requirements.
    fn credential_cache_key(entry: &CatalogProviderEntry) -> (String, String) {
        let mut env_vars: Vec<&String> = entry.credential_keys.values().collect();
        env_vars.sort();
        let fingerprint = env_vars
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<&str>>()
            .join(",");
        (entry.provider_id.clone(), fingerprint)
    }

    /// Check env vars for a provider entry's credential_keys.
    /// Returns the first env var value found, or None.
    /// Results are cached per (provider_id, credential_keys fingerprint)
    /// to avoid repeated env var lookups and prevent cross-model cache pollution.
    fn resolve_credentials(&self, entry: &CatalogProviderEntry) -> Option<String> {
        let cache_key = Self::credential_cache_key(entry);

        {
            let cache = self.credential_cache.lock().unwrap();
            if let Some(cached) = cache.get(&cache_key) {
                return cached.clone();
            }
        }

        // 1. Check entry's credential_keys against external_credentials first, then env.
        for env_var in entry.credential_keys.values() {
            if let Some(val) = self.external_credentials.get(env_var) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    let result = Some(trimmed);
                    self.credential_cache
                        .lock()
                        .unwrap()
                        .insert(cache_key, result.clone());
                    return result;
                }
            }
            if let Ok(val) = std::env::var(env_var) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    let result = Some(trimmed);
                    self.credential_cache
                        .lock()
                        .unwrap()
                        .insert(cache_key, result.clone());
                    return result;
                }
            }
        }

        // 2. Check PROVIDER_CREDENTIALS_MAP (external first, then env).
        let provider_id = &entry.provider_id;
        if let Some(creds) = PROVIDER_CREDENTIALS_MAP.get(provider_id.as_str()) {
            for (env_var, _field_name) in *creds {
                if let Some(val) = self.external_credentials.get(*env_var) {
                    let trimmed = val.trim().to_string();
                    if !trimmed.is_empty() {
                        let result = Some(trimmed);
                        self.credential_cache
                            .lock()
                            .unwrap()
                            .insert(cache_key, result.clone());
                        return result;
                    }
                }
                if let Ok(val) = std::env::var(env_var) {
                    let trimmed = val.trim().to_string();
                    if !trimmed.is_empty() {
                        let result = Some(trimmed);
                        self.credential_cache
                            .lock()
                            .unwrap()
                            .insert(cache_key, result.clone());
                        return result;
                    }
                }
            }
        }

        self.credential_cache
            .lock()
            .unwrap()
            .insert(cache_key, None);
        None
    }

    /// Clear the credential cache, forcing re-check of environment variables.
    #[cfg(test)]
    pub fn clear_credential_cache(&self) {
        self.credential_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Normalize a user-provided model string to a canonical ID.
    ///
    /// Checks catalog aliases, catalog model keys, custom models, and applies
    /// normalize_model_id() before checking.
    fn resolve_alias(&self, name: &str) -> Option<String> {
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
    pub fn resolve_permissive(&self, model_name: &str) -> Result<ResolvedModel, LatticeError> {
        if let Some((provider_part, model_part)) = model_name.split_once('/') {
            let provider_lower = provider_part.to_lowercase();
            let model_lower = model_part.to_lowercase();
            let defaults = self.catalog.get_provider_defaults(&provider_lower);
            if let Some(defaults) = defaults {
                let temp_entry = CatalogProviderEntry {
                    provider_id: provider_lower.clone(),
                    api_model_id: model_lower.clone(),
                    priority: 1,
                    credential_keys: defaults.credential_keys.clone(),
                    base_url: Some(defaults.base_url.clone()),
                    api_protocol: defaults.api_protocol.clone(),
                    provider_specific: HashMap::new(),
                };
                let api_key = self.resolve_credentials(&temp_entry);
                let credential_status = Self::credential_status_from_key(&api_key, &temp_entry);

                return Ok(ResolvedModel {
                    canonical_id: model_lower.clone(),
                    provider: provider_lower,
                    api_key,
                    base_url: defaults.base_url.clone(),
                    api_protocol: defaults.api_protocol.clone(),
                    api_model_id: model_lower,
                    context_length: 131072,
                    provider_specific: HashMap::new(),
                    credential_status,
                });
            }
        }

        Err(LatticeError::ModelNotFound {
            model: model_name.to_string(),
        })
    }

    /// Register a custom model at runtime.
    pub fn register_model(&mut self, entry: ModelCatalogEntry) {
        self.custom_models.insert(entry.canonical_id.clone(), entry);
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
                    if Self::is_credentialless(pe) || self.resolve_credentials(pe).is_some() {
                        authenticated.push(model_id.clone());
                        break;
                    }
                }
            }
        }

        for (model_id, entry) in &self.custom_models {
            for pe in &entry.providers {
                if Self::is_credentialless(pe) || self.resolve_credentials(pe).is_some() {
                    authenticated.push(model_id.clone());
                    break;
                }
            }
        }

        authenticated.sort();
        authenticated
    }

    /// Normalize a canonical model ID to the provider-specific api_model_id.
    #[cfg(test)]
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

    /// Resolve the effective base_url for a provider entry:
    /// 1. entry's base_url (if set and non-empty)
    /// 2. fall back to provider_defaults
    /// 3. empty string if neither is set
    fn resolve_base_url(&self, provider_id: &str, entry_url: &Option<String>) -> String {
        if let Some(url) = entry_url {
            if !url.is_empty() {
                return url.clone();
            }
        }
        self.catalog
            .get_provider_defaults(provider_id)
            .map(|d| d.base_url.clone())
            .unwrap_or_default()
    }
}

/// Validate that a base_url has proper URL format and uses HTTPS.
///
/// Rules:
/// - Empty URLs are allowed (backward compatibility)
/// - Non-empty URLs must contain `://` with a non-empty host
/// - HTTP is only allowed for localhost/127.0.0.1/::1
pub fn validate_base_url(url: &str) -> Result<(), LatticeError> {
    if url.is_empty() {
        return Ok(());
    }

    let proto_end = url.find("://").ok_or_else(|| LatticeError::Config {
        message: format!(
            "Invalid base_url '{}': URL must contain a scheme separator (://)",
            url
        ),
    })?;

    let scheme = &url[..proto_end];
    let after_proto = &url[proto_end + 3..];
    let host_port = after_proto.split('/').next().unwrap_or("");
    let host = host_port.split(':').next().unwrap_or("");
    if host.is_empty() {
        return Err(LatticeError::Config {
            message: format!("Invalid base_url '{}': URL has scheme but no host", url),
        });
    }

    if scheme == "http" && host != "localhost" && host != "127.0.0.1" && host != "::1" {
        return Err(LatticeError::Config {
            message: format!(
                "Insecure base_url '{}': HTTP is only allowed for localhost. Use HTTPS.",
                url
            ),
        });
    }

    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::env;
    use std::sync::{LazyLock, Mutex};

    pub static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    pub const ALL_CREDENTIAL_ENV_VARS: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "DEEPSEEK_API_KEY",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "XAI_API_KEY",
        "NOUS_API_KEY",
        "GITHUB_TOKEN",
        "OPENROUTER_API_KEY",
        "OPENCODE_ZEN_API_KEY",
        "KILO_API_KEY",
        "AI_GATEWAY_API_KEY",
        "OPENCODE_GO_API_KEY",
        "MINIMAX_API_KEY",
        "QWEN_API_KEY",
        "ARK_API_KEY",
        "INFINI_AI_API_KEY",
        "MY_CUSTOM_KEY",
    ];

    pub fn save_and_clear_all() -> Vec<(String, Option<String>)> {
        ALL_CREDENTIAL_ENV_VARS
            .iter()
            .map(|k| {
                let key = k.to_string();
                let prev = env::var(&key).ok();
                env::remove_var(&key);
                (key, prev)
            })
            .collect()
    }

    pub fn restore_all(saved: &[(String, Option<String>)]) {
        for (key, prev) in saved {
            match prev {
                Some(v) => env::set_var(key, v),
                None => env::remove_var(key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::ApiProtocol;
    use crate::router::test_support::{restore_all, save_and_clear_all, ENV_MUTEX};
    use std::env;

    #[test]
    fn test_normalize_model_id_openrouter_prefix() {
        assert_eq!(
            normalize_model_id("anthropic/claude-sonnet-4.6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(normalize_model_id("openai/gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_custom_registration() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();

        let mut router = ModelRouter::new();
        let custom = ModelCatalogEntry {
            canonical_id: "my-custom-model".to_string(),
            context_length: 8192,
            providers: vec![CatalogProviderEntry {
                provider_id: "custom".to_string(),
                api_model_id: "my-model".to_string(),
                priority: 1,
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
            router
                .list_models()
                .contains(&"my-custom-model".to_string()),
            "list_models should include custom model"
        );

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

        restore_all(&saved);
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
        let saved = save_and_clear_all();

        env::set_var("ANTHROPIC_API_KEY", "test-ant");

        let router = ModelRouter::new();
        let authed = router.list_authenticated_models();

        assert!(
            authed.contains(&"claude-sonnet-4-6".to_string()),
            "claude-sonnet-4-6 should be authenticated with ANTHROPIC_API_KEY set"
        );

        restore_all(&saved);
    }

    #[test]
    fn test_provider_override() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();

        env::set_var("NOUS_API_KEY", "nous-key");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("claude-sonnet-4-6", Some("anthropic"))
            .expect("should resolve with provider override");
        assert_eq!(resolved.provider, "anthropic");

        restore_all(&saved);
    }

    #[test]
    fn test_resolve_with_normalized_name() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();

        env::set_var("ANTHROPIC_API_KEY", "test-ant");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("claude-sonnet-4.6", None)
            .expect("should resolve normalized name");
        assert_eq!(resolved.canonical_id, "claude-sonnet-4-6");

        restore_all(&saved);
    }

    #[test]
    fn test_resolve_deepseek_with_direct_key() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();

        env::set_var("DEEPSEEK_API_KEY", "ds-key");

        let router = ModelRouter::new();
        let resolved = router
            .resolve("deepseek-v4-pro", None)
            .expect("should resolve deepseek-v4-pro");
        assert_eq!(resolved.provider, "deepseek");
        assert_eq!(resolved.api_key.as_deref(), Some("ds-key"));

        restore_all(&saved);
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

    #[test]
    fn test_credentialless_provider_wins_over_lower_priority_credentialed() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();

        env::set_var("ANTHROPIC_API_KEY", "ant-key");

        let mut router = ModelRouter::new();
        let custom = ModelCatalogEntry {
            canonical_id: "test-credless-priority".to_string(),
            context_length: 8192,
            providers: vec![
                CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: "test-model".to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost:11434/v1".to_string()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                },
                CatalogProviderEntry {
                    provider_id: "anthropic".to_string(),
                    api_model_id: "test-model".to_string(),
                    priority: 5,
                    credential_keys: HashMap::from([(
                        "api_key".to_string(),
                        "ANTHROPIC_API_KEY".to_string(),
                    )]),
                    base_url: Some("https://api.anthropic.com".to_string()),
                    api_protocol: ApiProtocol::AnthropicMessages,
                    provider_specific: HashMap::new(),
                },
            ],
            aliases: vec![],
        };
        router.register_model(custom);

        let resolved = router
            .resolve("test-credless-priority", None)
            .expect("should resolve");
        assert_eq!(
            resolved.provider, "ollama",
            "Ollama (priority 1, credentialless) should beat Anthropic (priority 5)"
        );
        assert!(resolved.api_key.is_none());

        restore_all(&saved);
    }

    #[test]
    fn test_credentialed_provider_wins_over_credentialless_at_same_priority() {
        let _lock = ENV_MUTEX.lock().unwrap();
        let saved = save_and_clear_all();

        env::set_var("ANTHROPIC_API_KEY", "ant-key");

        let mut router = ModelRouter::new();
        let custom = ModelCatalogEntry {
            canonical_id: "test-cred-same-priority".to_string(),
            context_length: 8192,
            providers: vec![
                CatalogProviderEntry {
                    provider_id: "ollama".to_string(),
                    api_model_id: "test-model".to_string(),
                    priority: 1,
                    credential_keys: HashMap::new(),
                    base_url: Some("http://localhost:11434/v1".to_string()),
                    api_protocol: ApiProtocol::OpenAiChat,
                    provider_specific: HashMap::new(),
                },
                CatalogProviderEntry {
                    provider_id: "anthropic".to_string(),
                    api_model_id: "test-model".to_string(),
                    priority: 1,
                    credential_keys: HashMap::from([(
                        "api_key".to_string(),
                        "ANTHROPIC_API_KEY".to_string(),
                    )]),
                    base_url: Some("https://api.anthropic.com".to_string()),
                    api_protocol: ApiProtocol::AnthropicMessages,
                    provider_specific: HashMap::new(),
                },
            ],
            aliases: vec![],
        };
        router.register_model(custom);

        let resolved = router
            .resolve("test-cred-same-priority", None)
            .expect("should resolve");
        assert_eq!(
            resolved.provider, "anthropic",
            "Credentialed provider at same priority should win over credentialless"
        );
        assert_eq!(resolved.api_key.as_deref(), Some("ant-key"));

        restore_all(&saved);
    }

    #[test]
    fn test_validate_base_url_empty() {
        assert!(validate_base_url("").is_ok());
    }

    #[test]
    fn test_validate_base_url_valid() {
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
        assert!(validate_base_url("http://localhost:8080").is_ok());
        assert!(validate_base_url("http://127.0.0.1:11434").is_ok());
        assert!(validate_base_url("custom://host/path").is_ok());
    }

    #[test]
    fn test_validate_base_url_no_host() {
        assert!(validate_base_url("https:///path").is_err());
        assert!(validate_base_url("http://").is_err());
    }

    #[test]
    fn test_validate_base_url_no_scheme() {
        assert!(validate_base_url("api.openai.com").is_err());
        assert!(validate_base_url("localhost:8080").is_err());
        assert!(validate_base_url("not-a-url").is_err());
    }

    #[test]
    fn test_unknown_provider_is_not_credentialless() {
        let entry = CatalogProviderEntry {
            provider_id: "antrhopic".to_string(),
            api_model_id: "some-model".to_string(),
            priority: 1,
            credential_keys: HashMap::new(),
            base_url: None,
            api_protocol: ApiProtocol::AnthropicMessages,
            provider_specific: HashMap::new(),
        };
        // Unknown provider not in PROVIDER_CREDENTIALS_RAW should NOT be treated as credentialless
        assert!(
            !ModelRouter::is_credentialless(&entry),
            "Unknown provider should not be treated as credentialless"
        );
    }

    #[test]
    fn test_resolve_model_with_no_providers_returns_config_error() {
        let mut router = ModelRouter::new();
        let custom = ModelCatalogEntry {
            canonical_id: "no-providers-model".to_string(),
            context_length: 4096,
            providers: vec![],
            aliases: vec![],
        };
        router.register_model(custom);

        let result = router.resolve("no-providers-model", None);
        match result {
            Err(LatticeError::Config { message }) => {
                assert!(
                    message.contains("no-providers-model"),
                    "Error should mention the model name, got: {}",
                    message
                );
            }
            other => panic!("Expected Err(LatticeError::Config), got: {:?}", other),
        }
    }
}
