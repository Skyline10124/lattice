use anyhow::Result;
use std::collections::HashMap;

use crate::config::{Config, ProviderConfig};

/// Nix Phase 1: Explicit credential store.
/// Instead of calling std::env::var() deep inside the resolve chain,
/// credentials are loaded once at startup and explicitly injected.
#[derive(Debug, Clone)]
pub struct CredentialStore {
    values: HashMap<String, String>,
}

impl CredentialStore {
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut values = HashMap::new();

        // Load from config's providers section
        for (provider_id, cfg) in &config.providers {
            if let Some(ref key) = cfg.api_key {
                let env_key = if key.starts_with('$') {
                    key.trim_start_matches('$').to_string()
                } else {
                    key.clone()
                };
                if let Ok(val) = std::env::var(&env_key) {
                    if !val.is_empty() {
                        values.insert(env_key, val);
                    }
                }
            }
        }

        // Also scan known provider env vars
        let known_vars = [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "DEEPSEEK_API_KEY",
            "NOUS_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
            "XAI_API_KEY",
            "GEMINI_API_KEY",
            "GITEA_API_KEY",
            "GITHUB_TOKEN",
            "OPENROUTER_API_KEY",
        ];
        for var in &known_vars {
            if let Ok(val) = std::env::var(var) {
                if !val.is_empty() {
                    values.insert(var.to_string(), val);
                }
            }
        }

        Ok(CredentialStore { values })
    }

    /// Inject credentials into process environment so artemis-core's
    /// ModelRouter (which currently reads env) can find them.
    /// This is the transitional bridge toward a fully pure resolve().
    pub fn inject_env(&self) {
        for (key, val) in &self.values {
            if std::env::var(key).is_err() || std::env::var(key).unwrap().is_empty() {
                std::env::set_var(key, val);
            }
        }
    }

    pub fn diagnostics(&self) -> Vec<(&str, bool)> {
        let vars = [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "DEEPSEEK_API_KEY",
            "NOUS_API_KEY",
            "GROQ_API_KEY",
            "MISTRAL_API_KEY",
            "XAI_API_KEY",
            "GEMINI_API_KEY",
        ];
        vars.iter()
            .map(|&v| (v, self.values.contains_key(v)))
            .collect()
    }
}
