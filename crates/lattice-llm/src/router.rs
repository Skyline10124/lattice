//! Fixed alias-table model router for v1.1.
//!
//! Maps a small set of explicit model aliases to provider metadata:
//! provider kind, provider model name, base URL, API-key environment variable,
//! and live credential status. Unknown aliases return a typed error — there is
//! no catalog, no prefix parsing, and no automatic fallback.

use std::env;

use crate::stub::LlmError;
use crate::types::ModelTarget;

/// Supported LLM provider families.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI API (`https://api.openai.com/v1`)
    OpenAi,
    /// OpenAI-compatible API endpoint (e.g. Ollama, vLLM, LiteLLM)
    OpenAiCompatible,
    /// Anthropic Messages API
    Anthropic,
    /// Google Gemini API
    Gemini,
}

/// Whether the required API key environment variable is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialStatus {
    /// The API key environment variable is present and non-empty.
    Present,
    /// The API key environment variable is missing or empty.
    Missing,
}

impl CredentialStatus {
    /// Read the named environment variable and return a credential status.
    ///
    /// Returns `Present` when the variable is set and non-empty.
    /// Returns `Missing` when the variable is absent or empty.
    /// Never exposes the key value.
    pub fn from_env(env_var: &str) -> Self {
        match env::var(env_var) {
            Ok(val) if !val.is_empty() => CredentialStatus::Present,
            _ => CredentialStatus::Missing,
        }
    }
}

/// Resolved metadata for a model alias.
///
/// Contains everything a provider module needs to construct an HTTP request:
/// which provider kind, which model name to send, which base URL to post to,
/// which env var holds the API key, and whether that key is available.
///
/// Never stores credential values. Callers read env vars at request time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModel {
    /// The user-supplied alias that was resolved.
    pub alias: String,
    /// Which provider family this alias maps to.
    pub provider: ProviderKind,
    /// The provider-specific model name to include in the API request body.
    pub provider_model: String,
    /// The base URL for the provider API (e.g. `https://api.openai.com/v1`).
    pub base_url: String,
    /// The environment variable name that holds the API key for this provider.
    pub api_key_env: String,
    /// Whether the API key is currently available in the environment.
    pub credential_status: CredentialStatus,
}

/// A fixed alias table that maps model aliases to explicit provider metadata.
///
/// The table is deliberately small: enough aliases to exercise the OpenAI,
/// Anthropic, Gemini, and OpenAI-compatible provider paths without recreating
/// a catalog, prefix system, or automatic fallback.
#[derive(Clone, Debug)]
pub struct ModelRouter {
    /// Optional default model alias when no target is specified.
    pub default_model: Option<String>,
}

impl Default for ModelRouter {
    fn default() -> Self {
        ModelRouter {
            default_model: Some("sonnet".to_string()),
        }
    }
}

impl ModelRouter {
    /// Create a new router with the default alias table.
    pub fn new() -> Self {
        ModelRouter::default()
    }

    /// Resolve a model target to explicit provider metadata.
    ///
    /// Returns `Ok(ResolvedModel)` for known aliases.
    /// Returns `Err(LlmError::UnknownModel)` for unknown aliases.
    /// Never falls back silently and never uses prefix syntax or catalog data.
    pub fn resolve(&self, target: &ModelTarget) -> Result<ResolvedModel, LlmError> {
        let alias = target.model.as_str();

        if alias == "openai-compatible" {
            let base_url = std::env::var("OPENAI_COMPATIBLE_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:11434/v1".to_string());
            return Ok(ResolvedModel {
                alias: alias.to_string(),
                provider: ProviderKind::OpenAiCompatible,
                provider_model: alias.to_string(),
                base_url,
                api_key_env: "OPENAI_COMPATIBLE_API_KEY".to_string(),
                credential_status: CredentialStatus::from_env("OPENAI_COMPATIBLE_API_KEY"),
            });
        }

        let (provider, provider_model, base_url, api_key_env) = match alias {
            "gpt-4o" => (
                ProviderKind::OpenAi,
                "gpt-4o",
                "https://api.openai.com/v1",
                "OPENAI_API_KEY",
            ),
            "gpt-4o-mini" => (
                ProviderKind::OpenAi,
                "gpt-4o-mini",
                "https://api.openai.com/v1",
                "OPENAI_API_KEY",
            ),
            "sonnet" => (
                ProviderKind::Anthropic,
                "claude-sonnet-4-20250514",
                "https://api.anthropic.com",
                "ANTHROPIC_API_KEY",
            ),
            "haiku" => (
                ProviderKind::Anthropic,
                "claude-3-5-haiku-20241022",
                "https://api.anthropic.com",
                "ANTHROPIC_API_KEY",
            ),
            "gemini" => (
                ProviderKind::Gemini,
                "gemini-2.5-flash",
                "https://generativelanguage.googleapis.com/v1beta",
                "GEMINI_API_KEY",
            ),
            _ => {
                return Err(LlmError::UnknownModel(alias.to_string()));
            }
        };

        Ok(ResolvedModel {
            alias: alias.to_string(),
            provider,
            provider_model: provider_model.to_string(),
            base_url: base_url.to_string(),
            api_key_env: api_key_env.to_string(),
            credential_status: CredentialStatus::from_env(api_key_env),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ModelTarget;

    // ── CREdentialStatus ──────────────────────────────────────────────────

    #[test]
    fn credential_status_present_from_env() {
        // Set a temporary env var for the test
        std::env::set_var("LATTICE_TEST_CRED_PRESENT", "sk-test-key");
        let status = CredentialStatus::from_env("LATTICE_TEST_CRED_PRESENT");
        assert_eq!(status, CredentialStatus::Present);
        std::env::remove_var("LATTICE_TEST_CRED_PRESENT");
    }

    #[test]
    fn credential_status_missing_when_unset() {
        let status = CredentialStatus::from_env("LATTICE_TEST_CRED_MISSING_UNSET");
        assert_eq!(status, CredentialStatus::Missing);
    }

    #[test]
    fn credential_status_missing_when_empty() {
        std::env::set_var("LATTICE_TEST_CRED_EMPTY", "");
        let status = CredentialStatus::from_env("LATTICE_TEST_CRED_EMPTY");
        assert_eq!(status, CredentialStatus::Missing);
        std::env::remove_var("LATTICE_TEST_CRED_EMPTY");
    }

    // ── ModelRouter::resolve — known aliases ──────────────────────────────

    #[test]
    fn resolve_gpt4o() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("gpt-4o");
        let resolved = router.resolve(&target).expect("gpt-4o should resolve");
        assert_eq!(resolved.alias, "gpt-4o");
        assert_eq!(resolved.provider, ProviderKind::OpenAi);
        assert_eq!(resolved.provider_model, "gpt-4o");
        assert_eq!(resolved.base_url, "https://api.openai.com/v1");
        assert_eq!(resolved.api_key_env, "OPENAI_API_KEY");
    }

    #[test]
    fn resolve_gpt4o_mini() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("gpt-4o-mini");
        let resolved = router.resolve(&target).expect("gpt-4o-mini should resolve");
        assert_eq!(resolved.provider, ProviderKind::OpenAi);
        assert_eq!(resolved.provider_model, "gpt-4o-mini");
        assert_eq!(resolved.base_url, "https://api.openai.com/v1");
        assert_eq!(resolved.api_key_env, "OPENAI_API_KEY");
    }

    #[test]
    fn resolve_sonnet() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("sonnet");
        let resolved = router.resolve(&target).expect("sonnet should resolve");
        assert_eq!(resolved.alias, "sonnet");
        assert_eq!(resolved.provider, ProviderKind::Anthropic);
        assert_eq!(resolved.provider_model, "claude-sonnet-4-20250514");
        assert_eq!(resolved.base_url, "https://api.anthropic.com");
        assert_eq!(resolved.api_key_env, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn resolve_haiku() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("haiku");
        let resolved = router.resolve(&target).expect("haiku should resolve");
        assert_eq!(resolved.provider, ProviderKind::Anthropic);
        assert_eq!(resolved.provider_model, "claude-3-5-haiku-20241022");
        assert_eq!(resolved.api_key_env, "ANTHROPIC_API_KEY");
    }

    #[test]
    fn resolve_gemini() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("gemini");
        let resolved = router.resolve(&target).expect("gemini should resolve");
        assert_eq!(resolved.provider, ProviderKind::Gemini);
        assert_eq!(resolved.provider_model, "gemini-2.5-flash");
        assert_eq!(
            resolved.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(resolved.api_key_env, "GEMINI_API_KEY");
    }

    #[test]
    fn resolve_openai_compatible() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("openai-compatible");
        let resolved = router
            .resolve(&target)
            .expect("openai-compatible should resolve");
        assert_eq!(resolved.provider, ProviderKind::OpenAiCompatible);
        assert_eq!(resolved.provider_model, "openai-compatible");
        assert_eq!(resolved.api_key_env, "OPENAI_COMPATIBLE_API_KEY");
        // base_url for openai-compatible comes from env, not hardcoded
        assert!(!resolved.base_url.is_empty());
    }

    // ── ModelRouter::resolve — unknown alias ──────────────────────────────

    #[test]
    fn resolve_unknown_alias_returns_error() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("nonexistent-model-xyz");
        let result = router.resolve(&target);
        assert!(result.is_err(), "unknown alias should return error");
    }

    #[test]
    fn resolve_unknown_alias_error_is_not_fallback() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("random-string-that-is-not-an-alias");
        let err = router
            .resolve(&target)
            .expect_err("should be an error, not fallback");
        let err_str = format!("{}", err);
        assert!(
            !err_str.contains("fallback"),
            "error should not mention fallback: {}",
            err_str
        );
    }

    // ── ResolvedModel never leaks credential values ──────────────────────

    #[test]
    fn resolved_model_debug_does_not_leak_secrets() {
        // Set a credential env var to simulate an available key
        std::env::set_var("OPENAI_API_KEY", "sk-secret-value-123");
        let router = ModelRouter::new();
        let target = ModelTarget::new("gpt-4o");
        let resolved = router.resolve(&target).expect("gpt-4o should resolve");
        let debug_str = format!("{:?}", resolved);
        assert!(
            !debug_str.contains("sk-secret-value-123"),
            "debug output must not leak API key value: {}",
            debug_str
        );
        assert!(
            debug_str.contains("OPENAI_API_KEY"),
            "debug output should contain the env var name"
        );
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[test]
    fn resolved_model_serialization_does_not_leak_secrets() {
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-secret");
        let router = ModelRouter::new();
        let target = ModelTarget::new("sonnet");
        let resolved = router.resolve(&target).expect("sonnet should resolve");
        // ResolvedModel should not contain the key value — it only stores the env var name
        assert_eq!(resolved.api_key_env, "ANTHROPIC_API_KEY");
        // The struct itself should never hold the secret
        let fields = format!("{:?}", resolved);
        assert!(!fields.contains("sk-ant-secret"));
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    // ── ModelRouter default ──────────────────────────────────────────────

    #[test]
    fn model_router_default_has_sonnet_default() {
        let router = ModelRouter::default();
        assert_eq!(router.default_model, Some("sonnet".to_string()));
    }
}
