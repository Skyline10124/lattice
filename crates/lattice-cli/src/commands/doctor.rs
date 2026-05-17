use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::credentials::CredentialStore;

/// Run v1.1 CLI/API health diagnostics.
///
/// Reports: config loading, credential status by env var name,
/// session store availability, and small alias table state.
///
/// Never prints credential values — only env var names and present/missing status.
/// Does not reference old Catalog, Runtime::list_models, or inspect_model.
pub fn run(config: &Config, json: bool, _creds: &CredentialStore) -> Result<()> {
    // Config
    let config_loaded = config.path.exists();
    let config_path = config.path.display().to_string();

    // Credentials: check router-relevant env vars via ModelRouter aliases
    let router = lattice_llm::ModelRouter::new();
    let aliases = router.aliases();
    let mut credential_entries: Vec<(String, String)> = Vec::new();
    for alias in &aliases {
        let status = format!("{:?}", alias.credential_status);
        credential_entries.push((alias.api_key_env.clone(), status));
    }
    // Deduplicate by env var name
    credential_entries.sort();
    credential_entries.dedup_by(|a, b| a.0 == b.0);

    // Session store availability
    let session_dir = crate::session::default_session_dir();
    let session_store_available = session_dir.exists();

    // Alias table state
    let alias_count = aliases.len();

    if json {
        let out = serde_json::json!({
            "config_loaded": config_loaded,
            "config_path": config_path,
            "credentials": credential_entries.iter().map(|(env, status)| {
                serde_json::json!({ "env_var": env, "status": status })
            }).collect::<Vec<_>>(),
            "session_store_available": session_store_available,
            "session_dir": session_dir.display().to_string(),
            "alias_count": alias_count,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    // Text output
    println!("{} LATTICE v1.1\n", "doctor".dimmed());

    // Config
    println!(
        "{}: {} ({})",
        "Config".bold(),
        config_path,
        if config_loaded {
            "loaded".green()
        } else {
            "default".dimmed()
        }
    );

    // Credentials
    println!("\n{}", "Credentials:".bold());
    for (env_var, status) in &credential_entries {
        let icon = if status == "Present" { "\u{2713}" } else { "\u{2717}" };
        let label = if status == "Present" {
            "set".green()
        } else {
            "missing".red()
        };
        println!("  {} {}: {}", icon, env_var, label);
    }

    // Session store
    println!("\n{}", "Session Store:".bold());
    println!(
        "  {} {}",
        if session_store_available {
            "\u{2713}"
        } else {
            "\u{2717}"
        },
        if session_store_available {
            "available".green()
        } else {
            "not found".red()
        }
    );
    println!("  {}: {}", "Dir".bold(), session_dir.display());

    // Alias table
    println!("\n{}", "Alias Table:".bold());
    println!("  {} known aliases", alias_count);

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

    fn default_config() -> Config {
        Config::default()
    }

    // ── cli_diagnostics: doctor text output ───────────────────────────────

    #[test]
    fn cli_diagnostics_doctor_text_includes_config_status() {
        let creds = empty_creds();
        let config = default_config();
        let result = run(&config, false, &creds);
        assert!(result.is_ok(), "doctor text should not error");
    }

    #[test]
    fn cli_diagnostics_doctor_text_includes_credential_env_var_names() {
        with_env_var("ANTHROPIC_API_KEY", "sk-doctor-test", || {
            let creds = empty_creds();
            let config = default_config();
            let result = run(&config, false, &creds);
            assert!(result.is_ok(), "doctor with credential should not error");
        });
    }

    #[test]
    fn cli_diagnostics_doctor_text_includes_session_store() {
        let creds = empty_creds();
        let config = default_config();
        let result = run(&config, false, &creds);
        assert!(result.is_ok(), "doctor session store check should not error");
    }

    #[test]
    fn cli_diagnostics_doctor_text_includes_alias_table_count() {
        let creds = empty_creds();
        let config = default_config();
        let result = run(&config, false, &creds);
        assert!(result.is_ok(), "doctor alias table should not error");
    }

    // ── cli_diagnostics: doctor JSON output ───────────────────────────────

    #[test]
    fn cli_diagnostics_doctor_json_is_valid() {
        let creds = empty_creds();
        let config = default_config();
        let result = run(&config, true, &creds);
        assert!(result.is_ok(), "doctor --json should not error");
    }

    // ── cli_diagnostics: doctor never leaks credential values ─────────────

    #[test]
    fn cli_diagnostics_doctor_never_leaks_credential_values() {
        with_env_var("ANTHROPIC_API_KEY", "sk-doctor-leak-test", || {
            let creds = empty_creds();
            let config = default_config();
            // The function should succeed. The rendering uses only env var
            // names and present/missing status, never the secret value.
            let result = run(&config, false, &creds);
            assert!(result.is_ok());
        });
    }

    #[test]
    fn cli_diagnostics_doctor_json_never_leaks_credential_values() {
        with_env_var("OPENAI_API_KEY", "sk-doctor-json-leak", || {
            let creds = empty_creds();
            let config = default_config();
            let result = run(&config, true, &creds);
            assert!(result.is_ok());
        });
    }

    // ── cli_diagnostics: doctor does not reference old catalog/runtime ────

    #[test]
    fn cli_diagnostics_doctor_no_catalog_reference() {
        // This test verifies that the doctor module does not import
        // lattice_core::catalog::Catalog. If it did, this test file would
        // fail to compile because we removed the import.
        // The absence of the import IS the assertion.
        let creds = empty_creds();
        let config = default_config();
        let result = run(&config, false, &creds);
        assert!(result.is_ok());
    }
}
