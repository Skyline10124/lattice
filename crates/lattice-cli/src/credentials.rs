use anyhow::Result;
use std::collections::HashMap;
use std::fmt;
use zeroize::Zeroize;

use crate::config::Config;

/// Nix Phase 1: Explicit credential store.
/// Credentials are loaded once at startup and passed into Runtime model routing.
/// via `with_credentials()`, eliminating the need for `std::env::set_var`.
#[derive(Clone)]
pub struct CredentialStore {
    values: HashMap<String, String>,
}

/// All env var names that the Runtime model router may look up.
pub const KNOWN_ENV_VARS: &[&str] = &[
    "AI_GATEWAY_API_KEY",
    "ANTHROPIC_API_KEY",
    "ARK_API_KEY",
    "DEEPSEEK_API_KEY",
    "GEMINI_API_KEY",
    "GITEA_API_KEY",
    "GITHUB_TOKEN",
    "GROQ_API_KEY",
    "INFINI_AI_API_KEY",
    "MINIMAX_API_KEY",
    "MISTRAL_API_KEY",
    "NOUS_API_KEY",
    "OPENAI_API_KEY",
    "OPENCODE_GO_API_KEY",
    "OPENROUTER_API_KEY",
    "QWEN_API_KEY",
    "XAI_API_KEY",
];

impl CredentialStore {
    pub fn from_config(config: &Config) -> Result<Self> {
        let mut values = HashMap::new();

        // Load from config's providers section.
        // api_key = "$ANTHROPIC_API_KEY"  -> read env var
        // api_key = "sk-abc123"           -> use literal value
        for cfg in config.providers.values() {
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
                    tracing::warn!(
                        "Literal API key found in config file. \
                         Use environment variable references ($VAR_NAME) instead \
                         to avoid leaking credentials in version control."
                    );
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

    /// Create a CredentialStore from raw values (used in tests).
    #[cfg(test)]
    pub(crate) fn from_values(values: HashMap<String, String>) -> Self {
        CredentialStore { values }
    }

    /// Return credentials as a HashMap for Runtime construction.
    pub fn to_hashmap(&self) -> HashMap<String, String> {
        self.values.clone()
    }

    /// Diagnostic: which env vars are present?
    pub fn diagnostics(&self) -> Vec<(&str, bool)> {
        KNOWN_ENV_VARS
            .iter()
            .map(|&v| (v, self.values.contains_key(v)))
            .collect()
    }
}

impl Drop for CredentialStore {
    fn drop(&mut self) {
        for value in self.values.values_mut() {
            value.zeroize();
        }
    }
}

impl fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialStore")
            .field("credential_count", &self.values.len())
            .field("values", &"[REDACTED]")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::CredentialStore;
    use std::collections::HashMap;

    #[test]
    fn debug_redacts_values_and_keys() {
        let store = CredentialStore::from_values(HashMap::from([
            (
                "OPENAI_API_KEY".to_string(),
                "sk-test-secret-value".to_string(),
            ),
            (
                "literal-secret-key".to_string(),
                "literal-secret-value".to_string(),
            ),
        ]));

        let debug = format!("{store:?}");

        assert!(debug.contains("credential_count"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("sk-test-secret-value"));
        assert!(!debug.contains("literal-secret-value"));
        assert!(!debug.contains("literal-secret-key"));
    }
}
