use anyhow::Result;
use colored::Colorize;
use lattice_core::router::ModelRouter;

use crate::credentials::CredentialStore;
use crate::display::{credential_label, status_icon};

pub fn run(
    model: &str,
    provider_override: Option<&str>,
    trace: bool,
    json: bool,
    creds: &CredentialStore,
) -> Result<()> {
    let router = ModelRouter::with_credentials(creds.to_hashmap());
    let resolved = router.resolve(model, provider_override)?;

    if json {
        let out = serde_json::json!({
            "canonical_id": resolved.canonical_id,
            "provider": resolved.provider,
            "api_model_id": resolved.api_model_id,
            "api_protocol": format!("{:?}", resolved.api_protocol),
            "base_url": resolved.base_url,
            "context_length": resolved.context_length,
            "api_key_present": resolved.api_key.is_some(),
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if trace {
        println!(
            "{}",
            format!(
                "resolve: {} \u{2192} {}@{}",
                model, resolved.canonical_id, resolved.provider
            )
            .cyan()
        );
        println!("  {}: {}", "Provider".bold(), resolved.provider);
        println!("  {}: {}", "Model".bold(), resolved.api_model_id);
        println!("  {}: {:?}", "Protocol".bold(), resolved.api_protocol);
        println!("  {}: {}", "Base URL".bold(), resolved.base_url);
        println!("  {}: {}", "Context".bold(), resolved.context_length);
        println!(
            "  {}: {} {}",
            "Auth".bold(),
            status_icon(resolved.api_key.is_some()),
            credential_label(resolved.api_key.is_some())
        );
    } else {
        println!("{}: {}", "Provider".bold(), resolved.provider);
        println!("{}: {}", "Model".bold(), resolved.api_model_id);
        println!("{}: {:?}", "Protocol".bold(), resolved.api_protocol);
        println!("{}: {}", "Base URL".bold(), resolved.base_url);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    /// Build a CredentialStore from raw env var values for testing.
    fn test_creds() -> CredentialStore {
        let mut values = HashMap::new();
        for var in crate::credentials::KNOWN_ENV_VARS {
            if let Ok(val) = std::env::var(var) {
                if !val.is_empty() {
                    values.insert(var.to_string(), val);
                }
            }
        }
        CredentialStore::from_values(values)
    }

    /// Save and restore env vars for test isolation.
    /// Uses a global mutex to prevent race conditions with parallel tests.
    fn with_env_var(key: &str, value: &str, f: impl FnOnce()) {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        f();
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn test_resolve_gpt4o_with_key() {
        with_env_var("OPENAI_API_KEY", "sk-test-cli", || {
            let creds = test_creds();
            let result = run("gpt-4o", None, false, false, &creds);
            assert!(result.is_ok(), "resolve gpt-4o with key should succeed");
        });
    }

    #[test]
    fn test_resolve_sonnet_with_key() {
        with_env_var("ANTHROPIC_API_KEY", "sk-test-cli", || {
            let creds = test_creds();
            let result = run("sonnet", None, false, false, &creds);
            assert!(result.is_ok(), "resolve sonnet with key should succeed");
        });
    }

    #[test]
    fn test_resolve_json_output_with_key() {
        with_env_var("OPENAI_API_KEY", "sk-test-json", || {
            let creds = test_creds();
            let result = run("gpt-4o", None, false, true, &creds);
            assert!(result.is_ok(), "resolve json output should succeed");
        });
    }

    #[test]
    fn test_resolve_trace_output() {
        with_env_var("ANTHROPIC_API_KEY", "sk-test-trace", || {
            let creds = test_creds();
            let result = run("sonnet", None, true, false, &creds);
            assert!(result.is_ok(), "resolve with trace should succeed");
        });
    }

    #[test]
    fn test_resolve_provider_override() {
        with_env_var("ANTHROPIC_API_KEY", "sk-test-ovr", || {
            let creds = test_creds();
            let result = run("claude-sonnet-4-6", Some("anthropic"), false, false, &creds);
            assert!(
                result.is_ok(),
                "resolve with provider override should succeed"
            );
        });
    }
}
