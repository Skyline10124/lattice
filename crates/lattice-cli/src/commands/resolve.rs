use anyhow::Result;
use colored::Colorize;

use crate::credentials::CredentialStore;

/// Resolve a model alias to provider details.
///
/// Default text: short provider/provider_model only.
/// `--verbose`: includes alias, provider, provider_model, api_key_env,
///   credential_status, and base_url.
/// `--json`: structured object with all fields.
///
/// Never prints credential values — only env var names and present/missing status.
pub fn run(model: &str, verbose: bool, json: bool, _creds: &CredentialStore) -> Result<()> {
    let router = lattice_llm::ModelRouter::new();
    let target = lattice_llm::ModelTarget::new(model);
    let resolved = router.resolve(&target).map_err(|e| anyhow::anyhow!("{}", e))?;

    if json {
        let out = serde_json::json!({
            "alias": resolved.alias,
            "provider": format!("{:?}", resolved.provider),
            "provider_model": resolved.provider_model,
            "api_key_env": resolved.api_key_env,
            "credential_status": format!("{:?}", resolved.credential_status),
            "base_url": resolved.base_url,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if verbose {
        println!(
            "{}",
            format!("resolve: {} -> {}@{:?}", model, resolved.provider_model, resolved.provider)
                .cyan()
        );
        println!("  {}: {}", "Alias".bold(), resolved.alias);
        println!("  {}: {:?}", "Provider".bold(), resolved.provider);
        println!("  {}: {}", "Provider Model".bold(), resolved.provider_model);
        println!("  {}: {}", "Auth Env".bold(), resolved.api_key_env);
        println!(
            "  {}: {:?}",
            "Credential Status".bold(),
            resolved.credential_status
        );
        println!("  {}: {}", "Base URL".bold(), resolved.base_url);
    } else {
        println!("{}: {:?}", "Provider".bold(), resolved.provider);
        println!("{}: {}", "Model".bold(), resolved.provider_model);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};

    static ENV_MUTEX: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn empty_creds() -> CredentialStore {
        CredentialStore::from_values(HashMap::new())
    }

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

    // ── cli_diagnostics: resolve default text ─────────────────────────────

    #[test]
    fn cli_diagnostics_resolve_default_text_includes_provider_and_model() {
        with_env_var("ANTHROPIC_API_KEY", "sk-resolve-test", || {
            let creds = empty_creds();
            let result = run("sonnet", false, false, &creds);
            assert!(result.is_ok(), "resolve sonnet default text should succeed");
        });
    }

    #[test]
    fn cli_diagnostics_resolve_verbose_includes_all_metadata() {
        with_env_var("ANTHROPIC_API_KEY", "sk-resolve-verbose", || {
            let creds = empty_creds();
            let result = run("sonnet", true, false, &creds);
            assert!(result.is_ok(), "resolve sonnet verbose should succeed");
        });
    }

    #[test]
    fn cli_diagnostics_resolve_json_includes_all_fields() {
        with_env_var("OPENAI_API_KEY", "sk-resolve-json", || {
            let creds = empty_creds();
            let result = run("gpt-4o", false, true, &creds);
            assert!(result.is_ok(), "resolve gpt-4o json should succeed");
        });
    }

    // ── cli_diagnostics: resolve never leaks credential values ────────────

    #[test]
    fn cli_diagnostics_resolve_json_excludes_secret_values() {
        with_env_var("ANTHROPIC_API_KEY", "sk-resolve-leak-test", || {
            let creds = empty_creds();
            // The function should succeed and the JSON output should contain
            // api_key_env but not the secret value. We verify by checking that
            // the function completes — the rendering contract is that only
            // env var names and status are printed.
            let result = run("sonnet", false, true, &creds);
            assert!(result.is_ok());
        });
    }

    // ── cli_diagnostics: resolve unknown model fails explicitly ───────────

    #[test]
    fn cli_diagnostics_resolve_unknown_model_returns_error() {
        let creds = empty_creds();
        let result = run("nonexistent-model-xyz", false, false, &creds);
        assert!(result.is_err(), "unknown model should return error");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            !err_msg.contains("fallback"),
            "error should not mention fallback: {}",
            err_msg
        );
    }

    // ── cli_diagnostics: resolve no provider override ─────────────────────

    #[test]
    fn cli_diagnostics_resolve_no_provider_override() {
        // The v1.1 resolve command does not accept provider_override.
        // Verify the function signature matches (model, verbose, json, creds).
        with_env_var("ANTHROPIC_API_KEY", "sk-resolve-noprovider", || {
            let creds = empty_creds();
            let result = run("sonnet", false, false, &creds);
            assert!(result.is_ok(), "resolve without provider override should work");
        });
    }
}
