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

const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const GEMINI_API_KEY_ENV: &str = "GEMINI_API_KEY";
const OPENAI_COMPATIBLE_API_KEY_ENV: &str = "OPENAI_COMPATIBLE_API_KEY";

/// 路由实际使用的凭据变量名；提交端诊断据此读取当前值做精确脱敏。
pub fn credential_env_vars() -> &'static [&'static str] {
    &[
        OPENAI_API_KEY_ENV,
        ANTHROPIC_API_KEY_ENV,
        GEMINI_API_KEY_ENV,
        OPENAI_COMPATIBLE_API_KEY_ENV,
    ]
}

/// 缺 `OPENAI_COMPATIBLE_MODEL` 时的固定提示：不含别名或凭据值，可直接展示。
///
/// resolve 命令按这个常量识别该分支；提交端配置诊断统一经 redact 处理。
pub const MISSING_OPENAI_COMPATIBLE_MODEL_HINT: &str =
    "openai-compatible requires OPENAI_COMPATIBLE_MODEL or use compat:<model>";
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
        api_key_env: OPENAI_COMPATIBLE_API_KEY_ENV.to_string(),
        credential_status: CredentialStatus::from_env(OPENAI_COMPATIBLE_API_KEY_ENV),
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
                    MISSING_OPENAI_COMPATIBLE_MODEL_HINT.to_string(),
                )),
            };
        }

        let (provider, provider_model, base_url, api_key_env) = match alias {
            // OpenAI GPT series
            "gpt-5.5" | "gpt-5" => (
                ProviderKind::OpenAi,
                "gpt-5.5",
                OPENAI_BASE_URL,
                OPENAI_API_KEY_ENV,
            ),
            "gpt-5.4" => (
                ProviderKind::OpenAi,
                "gpt-5.4",
                OPENAI_BASE_URL,
                OPENAI_API_KEY_ENV,
            ),
            "gpt-5.3-codex" => (
                ProviderKind::OpenAi,
                "gpt-5.3-codex",
                OPENAI_BASE_URL,
                OPENAI_API_KEY_ENV,
            ),
            "gpt-4o-mini" => (
                ProviderKind::OpenAi,
                "gpt-4o-mini",
                OPENAI_BASE_URL,
                OPENAI_API_KEY_ENV,
            ),
            // Anthropic Claude series
            "opus" => (
                ProviderKind::Anthropic,
                "claude-opus-4-7",
                ANTHROPIC_BASE_URL,
                ANTHROPIC_API_KEY_ENV,
            ),
            "sonnet" => (
                ProviderKind::Anthropic,
                "claude-sonnet-4-6",
                ANTHROPIC_BASE_URL,
                ANTHROPIC_API_KEY_ENV,
            ),
            "haiku" => (
                ProviderKind::Anthropic,
                "claude-haiku-4-5",
                ANTHROPIC_BASE_URL,
                ANTHROPIC_API_KEY_ENV,
            ),
            // Gemini
            "gemini" => (
                ProviderKind::Gemini,
                "gemini-2.5-flash",
                GEMINI_BASE_URL,
                GEMINI_API_KEY_ENV,
            ),
            _ => {
                return Err(LlmError::UnknownModel(alias.to_string()));
            }
        };

        // Anthropic and Gemini endpoints can be moved by their formal
        // `*_BASE_URL` setting, read the same way as
        // `OPENAI_COMPATIBLE_BASE_URL`; OpenAI keeps its fixed endpoint.
        let override_env = match provider {
            ProviderKind::Anthropic => Some("ANTHROPIC_BASE_URL"),
            ProviderKind::Gemini => Some("GEMINI_BASE_URL"),
            _ => None,
        };
        let base_url = override_env
            .and_then(|name| std::env::var(name).ok())
            .unwrap_or_else(|| base_url.to_string());

        Ok(ResolvedModel {
            alias: alias.to_string(),
            provider,
            provider_model: provider_model.to_string(),
            base_url,
            api_key_env: api_key_env.to_string(),
            credential_status: CredentialStatus::from_env(api_key_env),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::with_env_vars;
    use crate::types::ModelTarget;

    #[test]
    fn credential_names_cover_router_providers() {
        let router = ModelRouter::new();
        let routed: std::collections::BTreeSet<String> =
            ["gpt-5.5", "sonnet", "gemini", "compat:test"]
                .into_iter()
                .map(|alias| {
                    router
                        .resolve(&ModelTarget::new(alias))
                        .unwrap()
                        .api_key_env
                })
                .collect();
        let listed: std::collections::BTreeSet<String> = credential_env_vars()
            .iter()
            .map(|name| (*name).to_owned())
            .collect();
        assert_eq!(routed, listed);
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
}
