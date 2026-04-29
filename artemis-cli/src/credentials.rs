use anyhow::Result;
use std::collections::HashMap;

use crate::config::Config;

/// Nix Phase 1: Explicit credential store.
/// Credentials are loaded once at startup and passed directly to ModelRouter
/// via `with_credentials()`, eliminating the need for `std::env::set_var`.
#[derive(Debug, Clone)]
pub struct CredentialStore {
    values: HashMap<String, String>,
}

/// All env var names that artemis-core may look up.
const KNOWN_ENV_VARS: &[&str] = &[
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

impl CredentialStore {
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut values = HashMap::new();

        // Load from config's providers section.
        // api_key = "$ANTHROPIC_API_KEY"  -> read env var
        // api_key = "sk-abc123"           -> use literal value
        for (_provider_id, cfg) in &config.providers {
            if let Some(ref key_spec) = cfg.api_key {
                if key_spec.starts_with('$') {
                    // Env var reference: $ANTHROPIC_API_KEY
                    let env_name = key_spec.trim_start_matches('$');
                    if let Ok(val) = std::env::var(env_name) {
                        if !val.is_empty() {
                            values.insert(env_name.to_string(), val);
                        }
                    }
                } else {
                    // Literal value: treat the raw string as the key.
                    // We store it under a synthetic key so it can be looked up.
                    // In practice the provider config in core will still try
                    // env var names, so literal keys need provider-specific wiring.
                    // For now, we store as-is and let the caller decide.
                    values.insert(key_spec.clone(), key_spec.clone());
                }
            }
        }

        // Scan known provider env vars.
        for var in KNOWN_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                if !val.is_empty() {
                    values.insert(var.to_string(), val);
                }
            }
        }

        Ok(CredentialStore { values })
    }

    /// Return credentials as a HashMap for ModelRouter::with_credentials().
    pub fn to_hashmap(&self) -> HashMap<String, String> {
        self.values.clone()
    }

    /// Look up a single credential by env var name.
    pub fn get(&self, key: &str) -> Option<&String> {
        self.values.get(key)
    }

    /// Diagnostic: which env vars are present?
    pub fn diagnostics(&self) -> Vec<(&str, bool)> {
        KNOWN_ENV_VARS
            .iter()
            .map(|&v| (v, self.values.contains_key(v)))
            .collect()
    }
}
