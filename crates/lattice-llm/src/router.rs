// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fixed alias-table model router.
//!
//! Maps a small set of explicit model aliases to provider metadata:
//! provider kind, provider model name, base URL, API-key environment variable,
//! and live credential status. Unknown aliases return a typed error — there is
//! no catalog, no prefix parsing, and no automatic fallback.

use std::env;

use crate::endpoints::{
    ANTHROPIC_BASE_URL, GEMINI_BASE_URL, OPENAI_BASE_URL, OPENAI_COMPATIBLE_DEFAULT_BASE_URL,
};
use crate::stub::LlmError;
use crate::types::ModelTarget;

/// Supported LLM provider families.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAi,
    OpenAiCompatible,
    Anthropic,
    Gemini,
}

impl ProviderKind {
    /// Stable provider identifier for commit metadata and public events.
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::OpenAi => "openai",
            ProviderKind::OpenAiCompatible => "openai_compatible",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Gemini => "gemini",
        }
    }
}

/// Whether the required API key environment variable is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialStatus {
    Present,
    Missing,
}

impl CredentialStatus {
    /// Read the named environment variable and return a credential status.
    pub fn from_env(env_var: &str) -> Self {
        match env::var(env_var) {
            Ok(val) if !val.is_empty() => CredentialStatus::Present,
            _ => CredentialStatus::Missing,
        }
    }
}

/// Resolved metadata for a model alias.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModel {
    pub alias: String,
    pub provider: ProviderKind,
    pub provider_model: String,
    pub base_url: String,
    pub api_key_env: String,
    pub credential_status: CredentialStatus,
}

/// A fixed alias table that maps model aliases to explicit provider metadata.
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

fn build_openai_compatible(alias: &str, provider_model: &str) -> ResolvedModel {
    let base_url = std::env::var("OPENAI_COMPATIBLE_BASE_URL")
        .unwrap_or_else(|_| OPENAI_COMPATIBLE_DEFAULT_BASE_URL.to_string());
    ResolvedModel {
        alias: alias.to_string(),
        provider: ProviderKind::OpenAiCompatible,
        provider_model: provider_model.to_string(),
        base_url,
        api_key_env: "OPENAI_COMPATIBLE_API_KEY".to_string(),
        credential_status: CredentialStatus::from_env("OPENAI_COMPATIBLE_API_KEY"),
    }
}

impl ModelRouter {
    /// Create a new router with the default alias table.
    pub fn new() -> Self {
        ModelRouter::default()
    }

    /// Return the fixed alias list with full metadata for each entry.
    pub fn aliases(&self) -> Vec<ResolvedModel> {
        let names = [
            "gpt-5.5",
            "gpt-5.4",
            "gpt-5.3-codex",
            "gpt-4o-mini",
            "opus",
            "sonnet",
            "haiku",
            "gemini",
            "openai-compatible",
        ];
        names
            .iter()
            .filter_map(|name| {
                let target = ModelTarget::new(*name);
                self.resolve(&target).ok()
            })
            .collect()
    }

    /// Resolve a model target to explicit provider metadata.
    pub fn resolve(&self, target: &ModelTarget) -> Result<ResolvedModel, LlmError> {
        let alias = target.model.as_str();

        for prefix in ["compat:", "compatible:", "openai-compatible:"] {
            if let Some(model) = alias.strip_prefix(prefix) {
                if model.is_empty() {
                    return Err(LlmError::Other(format!(
                        "compatible model name is empty in '{}'; use {}<model>",
                        alias, prefix
                    )));
                }
                return Ok(build_openai_compatible(alias, model));
            }
        }

        if alias == "openai-compatible" {
            return match std::env::var("OPENAI_COMPATIBLE_MODEL") {
                Ok(model) if !model.is_empty() => Ok(build_openai_compatible(alias, &model)),
                _ => Err(LlmError::Other(
                    "openai-compatible requires OPENAI_COMPATIBLE_MODEL or use compat:<model>"
                        .to_string(),
                )),
            };
        }

        let (provider, provider_model, base_url, api_key_env) = match alias {
            // OpenAI GPT series
            "gpt-5.5" | "gpt-5" => (
                ProviderKind::OpenAi,
                "gpt-5.5",
                OPENAI_BASE_URL,
                "OPENAI_API_KEY",
            ),
            "gpt-5.4" => (
                ProviderKind::OpenAi,
                "gpt-5.4",
                OPENAI_BASE_URL,
                "OPENAI_API_KEY",
            ),
            "gpt-5.3-codex" => (
                ProviderKind::OpenAi,
                "gpt-5.3-codex",
                OPENAI_BASE_URL,
                "OPENAI_API_KEY",
            ),
            "gpt-4o-mini" => (
                ProviderKind::OpenAi,
                "gpt-4o-mini",
                OPENAI_BASE_URL,
                "OPENAI_API_KEY",
            ),
            // Anthropic Claude series
            "opus" => (
                ProviderKind::Anthropic,
                "claude-opus-4-7",
                ANTHROPIC_BASE_URL,
                "ANTHROPIC_API_KEY",
            ),
            "sonnet" => (
                ProviderKind::Anthropic,
                "claude-sonnet-4-6",
                ANTHROPIC_BASE_URL,
                "ANTHROPIC_API_KEY",
            ),
            "haiku" => (
                ProviderKind::Anthropic,
                "claude-haiku-4-5",
                ANTHROPIC_BASE_URL,
                "ANTHROPIC_API_KEY",
            ),
            // Gemini
            "gemini" => (
                ProviderKind::Gemini,
                "gemini-2.5-flash",
                GEMINI_BASE_URL,
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
    use std::sync::{LazyLock, Mutex};

    use super::*;
    use crate::types::ModelTarget;

    static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn with_env_vars(vars: &[(&str, Option<&str>)], f: impl FnOnce()) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());

        let previous: Vec<(&str, Option<String>)> = vars
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect();

        for (key, value) in vars {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }

        f();

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn credential_status_present_from_env() {
        with_env_vars(
            &[("LATTICE_TEST_CRED_PRESENT", Some("sk-test-key"))],
            || {
                let status = CredentialStatus::from_env("LATTICE_TEST_CRED_PRESENT");
                assert_eq!(status, CredentialStatus::Present);
            },
        );
    }

    #[test]
    fn credential_status_missing_when_unset() {
        with_env_vars(&[("LATTICE_TEST_CRED_MISSING_UNSET", None)], || {
            let status = CredentialStatus::from_env("LATTICE_TEST_CRED_MISSING_UNSET");
            assert_eq!(status, CredentialStatus::Missing);
        });
    }

    #[test]
    fn credential_status_missing_when_empty() {
        with_env_vars(&[("LATTICE_TEST_CRED_EMPTY", Some(""))], || {
            let status = CredentialStatus::from_env("LATTICE_TEST_CRED_EMPTY");
            assert_eq!(status, CredentialStatus::Missing);
        });
    }

    struct ResolveCase {
        alias: &'static str,
        provider: ProviderKind,
        provider_model: &'static str,
        api_key_env: &'static str,
    }

    #[test]
    fn resolve_known_aliases() {
        let cases = [
            ResolveCase {
                alias: "gpt-5.5",
                provider: ProviderKind::OpenAi,
                provider_model: "gpt-5.5",
                api_key_env: "OPENAI_API_KEY",
            },
            ResolveCase {
                alias: "gpt-4o-mini",
                provider: ProviderKind::OpenAi,
                provider_model: "gpt-4o-mini",
                api_key_env: "OPENAI_API_KEY",
            },
            ResolveCase {
                alias: "sonnet",
                provider: ProviderKind::Anthropic,
                provider_model: "claude-sonnet-4-6",
                api_key_env: "ANTHROPIC_API_KEY",
            },
            ResolveCase {
                alias: "haiku",
                provider: ProviderKind::Anthropic,
                provider_model: "claude-haiku-4-5",
                api_key_env: "ANTHROPIC_API_KEY",
            },
            ResolveCase {
                alias: "gemini",
                provider: ProviderKind::Gemini,
                provider_model: "gemini-2.5-flash",
                api_key_env: "GEMINI_API_KEY",
            },
        ];

        let router = ModelRouter::new();
        for case in cases {
            let target = ModelTarget::new(case.alias);
            let resolved = router
                .resolve(&target)
                .unwrap_or_else(|_| panic!("{} should resolve", case.alias));
            assert_eq!(
                resolved.provider, case.provider,
                "{}: provider mismatch",
                case.alias
            );
            assert_eq!(
                resolved.provider_model, case.provider_model,
                "{}: model mismatch",
                case.alias
            );
            assert_eq!(
                resolved.api_key_env, case.api_key_env,
                "{}: env var mismatch",
                case.alias
            );
            assert!(
                !resolved.base_url.is_empty(),
                "{}: should have base_url",
                case.alias
            );
        }
    }

    #[test]
    fn provider_kind_as_str_is_stable_snake_case() {
        assert_eq!(ProviderKind::OpenAi.as_str(), "openai");
        assert_eq!(ProviderKind::OpenAiCompatible.as_str(), "openai_compatible");
        assert_eq!(ProviderKind::Anthropic.as_str(), "anthropic");
        assert_eq!(ProviderKind::Gemini.as_str(), "gemini");
    }

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

    #[test]
    fn resolved_model_debug_does_not_leak_secrets() {
        with_env_vars(&[("OPENAI_API_KEY", Some("sk-secret-value-123"))], || {
            let router = ModelRouter::new();
            let target = ModelTarget::new("gpt-5.5");
            let resolved = router.resolve(&target).expect("gpt-5.5 should resolve");
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
        });
    }

    #[test]
    fn resolved_model_serialization_does_not_leak_secrets() {
        with_env_vars(&[("ANTHROPIC_API_KEY", Some("sk-ant-secret"))], || {
            let router = ModelRouter::new();
            let target = ModelTarget::new("sonnet");
            let resolved = router.resolve(&target).expect("sonnet should resolve");
            assert_eq!(resolved.api_key_env, "ANTHROPIC_API_KEY");
            let fields = format!("{:?}", resolved);
            assert!(!fields.contains("sk-ant-secret"));
        });
    }

    #[test]
    fn model_router_default_has_sonnet_default() {
        let router = ModelRouter::default();
        assert_eq!(router.default_model, Some("sonnet".to_string()));
    }

    #[test]
    fn aliases_returns_all_known_aliases() {
        with_env_vars(
            &[("OPENAI_COMPATIBLE_MODEL", Some("deepseek-chat"))],
            || {
                let router = ModelRouter::new();
                let aliases = router.aliases();
                let names: Vec<&str> = aliases.iter().map(|a| a.alias.as_str()).collect();

                let expected = [
                    "gpt-5.5",
                    "gpt-5.4",
                    "gpt-5.3-codex",
                    "gpt-4o-mini",
                    "opus",
                    "sonnet",
                    "haiku",
                    "gemini",
                    "openai-compatible",
                ];
                for name in &expected {
                    assert!(
                        names.contains(name),
                        "aliases() should include '{}', got: {:?}",
                        name,
                        names
                    );
                }
            },
        );
    }

    #[test]
    fn aliases_carry_provider_metadata_and_no_secrets() {
        with_env_vars(
            &[
                ("OPENAI_API_KEY", Some("sk-router-secret")),
                ("ANTHROPIC_API_KEY", Some("sk-ant-secret")),
                ("GEMINI_API_KEY", Some("sk-gem-secret")),
                ("OPENAI_COMPATIBLE_MODEL", Some("deepseek-chat")),
            ],
            || {
                let router = ModelRouter::new();
                let aliases = router.aliases();

                for alias in &aliases {
                    assert!(
                        !alias.api_key_env.is_empty(),
                        "{}: should have api_key_env",
                        alias.alias
                    );
                    assert!(
                        !alias.base_url.is_empty(),
                        "{}: should have base_url",
                        alias.alias
                    );
                    let debug = format!("{:?}", alias);
                    assert!(
                        !debug.contains("sk-router-secret"),
                        "{}: no leak",
                        alias.alias
                    );
                    assert!(!debug.contains("sk-ant-secret"), "{}: no leak", alias.alias);
                    assert!(!debug.contains("sk-gem-secret"), "{}: no leak", alias.alias);
                }
            },
        );
    }

    #[test]
    fn resolve_compat_prefix_model() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("compat:deepseek-chat");
        let resolved = router.resolve(&target).expect("should resolve");
        assert_eq!(resolved.provider, ProviderKind::OpenAiCompatible);
        assert_eq!(resolved.provider_model, "deepseek-chat");
        assert_eq!(resolved.api_key_env, "OPENAI_COMPATIBLE_API_KEY");
        assert_eq!(resolved.alias, "compat:deepseek-chat");
    }

    #[test]
    fn resolve_openai_compatible_prefix_model() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("openai-compatible:deepseek-chat");
        let resolved = router.resolve(&target).expect("should resolve");
        assert_eq!(resolved.provider, ProviderKind::OpenAiCompatible);
        assert_eq!(resolved.provider_model, "deepseek-chat");
        assert_eq!(resolved.api_key_env, "OPENAI_COMPATIBLE_API_KEY");
    }

    #[test]
    fn resolve_compatible_prefix_model() {
        let router = ModelRouter::new();
        let target = ModelTarget::new("compatible:qwen-plus");
        let resolved = router.resolve(&target).expect("should resolve");
        assert_eq!(resolved.provider, ProviderKind::OpenAiCompatible);
        assert_eq!(resolved.provider_model, "qwen-plus");
    }

    #[test]
    fn resolve_bare_openai_compatible_uses_env_model() {
        with_env_vars(
            &[("OPENAI_COMPATIBLE_MODEL", Some("deepseek-chat"))],
            || {
                let router = ModelRouter::new();
                let target = ModelTarget::new("openai-compatible");
                let resolved = router.resolve(&target).expect("should resolve");
                assert_eq!(resolved.provider, ProviderKind::OpenAiCompatible);
                assert_eq!(resolved.provider_model, "deepseek-chat");
                assert_eq!(resolved.alias, "openai-compatible");
            },
        );
    }

    #[test]
    fn resolve_bare_openai_compatible_errors_without_env_model() {
        with_env_vars(&[("OPENAI_COMPATIBLE_MODEL", None)], || {
            let router = ModelRouter::new();
            let target = ModelTarget::new("openai-compatible");
            let err = router
                .resolve(&target)
                .expect_err("bare openai-compatible without env model should error");
            let msg = format!("{}", err);
            assert!(
                msg.contains("OPENAI_COMPATIBLE_MODEL"),
                "error should mention OPENAI_COMPATIBLE_MODEL: {}",
                msg
            );
            assert!(
                msg.contains("compat:<model>"),
                "error should mention compat:<model>: {}",
                msg
            );
        });
    }

    #[test]
    fn resolve_empty_compat_model_errors() {
        let router = ModelRouter::new();
        for alias in ["compat:", "compatible:", "openai-compatible:"] {
            let target = ModelTarget::new(alias);
            let err = router
                .resolve(&target)
                .err()
                .unwrap_or_else(|| panic!("expected error for '{}'", alias));
            let msg = format!("{}", err);
            assert!(
                msg.contains("compatible model name is empty"),
                "error for '{}' should mention empty model: {}",
                alias,
                msg
            );
        }
    }
}
