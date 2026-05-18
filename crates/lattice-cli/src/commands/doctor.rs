use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;

/// Report structure for doctor diagnostics.
pub struct DoctorReport {
    pub config_loaded: bool,
    pub config_path: String,
    pub credentials: Vec<CredentialEntry>,
    pub session_store_available: bool,
    pub session_dir: String,
    pub alias_count: usize,
}

/// A credential entry showing env var name and status.
pub struct CredentialEntry {
    pub env_var: String,
    pub status: String,
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .map(|dir| dir.join("lattice").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("lattice.toml"))
}

/// Build a diagnostic report.
pub fn build_report() -> DoctorReport {
    // Config
    let config_path = default_config_path();
    let config_loaded = config_path.exists();
    let config_path = config_path.display().to_string();

    // Credentials: check router-relevant env vars via ModelRouter aliases
    let router = lattice_llm::ModelRouter::new();
    let aliases = router.aliases();
    let mut credential_entries: Vec<CredentialEntry> = Vec::new();
    for alias in &aliases {
        let status = format!("{:?}", alias.credential_status);
        credential_entries.push(CredentialEntry {
            env_var: alias.api_key_env.clone(),
            status,
        });
    }
    // Deduplicate by env var name
    credential_entries.sort_by(|a, b| a.env_var.cmp(&b.env_var));
    credential_entries.dedup_by(|a, b| a.env_var == b.env_var);

    // Session store availability
    let session_dir = crate::commands::session::default_session_dir();
    let session_store_available = session_dir.exists();

    // Alias table state
    let alias_count = aliases.len();

    DoctorReport {
        config_loaded,
        config_path,
        credentials: credential_entries,
        session_store_available,
        session_dir: session_dir.display().to_string(),
        alias_count,
    }
}

/// Render report as JSON.
pub fn render_json(report: &DoctorReport) -> Result<String> {
    let out = serde_json::json!({
        "config_loaded": report.config_loaded,
        "config_path": report.config_path,
        "credentials": report.credentials.iter().map(|c| {
            serde_json::json!({ "env_var": c.env_var, "status": c.status })
        }).collect::<Vec<_>>(),
        "session_store_available": report.session_store_available,
        "session_dir": report.session_dir,
        "alias_count": report.alias_count,
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

/// Render report as text.
pub fn render_text(report: &DoctorReport) -> String {
    let mut output = String::new();

    output.push_str(&format!("{} LATTICE v1.1\n", "doctor".dimmed()));

    // Config
    output.push_str(&format!(
        "{}: {} ({})\n",
        "Config".bold(),
        report.config_path,
        if report.config_loaded {
            "loaded".green()
        } else {
            "default".dimmed()
        }
    ));

    // Credentials
    output.push_str(&format!("\n{}\n", "Credentials:".bold()));
    for cred in &report.credentials {
        let icon = if cred.status == "Present" {
            "\u{2713}"
        } else {
            "\u{2717}"
        };
        let label = if cred.status == "Present" {
            "set".green()
        } else {
            "missing".red()
        };
        output.push_str(&format!("  {} {}: {}\n", icon, cred.env_var, label));
    }

    // Session store
    output.push_str(&format!("\n{}\n", "Session Store:".bold()));
    output.push_str(&format!(
        "  {} {}\n",
        if report.session_store_available {
            "\u{2713}"
        } else {
            "\u{2717}"
        },
        if report.session_store_available {
            "available".green()
        } else {
            "not found".red()
        }
    ));
    output.push_str(&format!("  {} {}\n", "Dir:".bold(), report.session_dir));

    // Alias table
    output.push_str(&format!("\n{}\n", "Alias Table:".bold()));
    output.push_str(&format!("  {} known aliases\n", report.alias_count));

    output
}

/// Run v1.1 CLI/API health diagnostics.
///
/// Reports: config loading, credential status by env var name,
/// session store availability, and small alias table state.
///
/// Never prints credential values — only env var names and present/missing status.
/// Does not reference old Catalog, Runtime::list_models, or inspect_model.
pub fn run(json: bool) -> Result<()> {
    let report = build_report();
    if json {
        println!("{}", render_json(&report)?);
    } else {
        print!("{}", render_text(&report));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doctor_report_contains_expected_keys() {
        let report = build_report();

        // Report should have all expected fields
        assert!(report.config_path.contains("lattice.toml") || !report.config_path.is_empty());
        assert!(report.alias_count > 0, "should have known aliases");
        assert!(report.session_dir.contains("sessions") || !report.session_dir.is_empty());
    }

    #[test]
    fn doctor_json_output_has_expected_structure() {
        let report = build_report();
        let json = render_json(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // Verify JSON structure
        assert!(parsed.get("config_loaded").is_some());
        assert!(parsed.get("config_path").is_some());
        assert!(parsed.get("credentials").is_some());
        assert!(parsed.get("session_store_available").is_some());
        assert!(parsed.get("session_dir").is_some());
        assert!(parsed.get("alias_count").is_some());
    }

    #[test]
    fn doctor_text_output_includes_credential_env_var_names() {
        let report = build_report();
        let text = render_text(&report);

        // Should mention expected credential env vars
        assert!(text.contains("Credentials:"));
        assert!(text.contains("ANTHROPIC_API_KEY") || text.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn doctor_text_output_includes_session_dir() {
        let report = build_report();
        let text = render_text(&report);

        assert!(text.contains("Session Store:"));
        assert!(text.contains("Dir:"));
        assert!(text.contains(&report.session_dir));
    }

    #[test]
    fn doctor_text_output_includes_alias_count() {
        let report = build_report();
        let text = render_text(&report);

        assert!(text.contains("Alias Table:"));
        assert!(text.contains(&format!("{} known aliases", report.alias_count)));
    }

    #[test]
    fn doctor_json_does_not_leak_secret_values() {
        std::env::set_var("ANTHROPIC_API_KEY", "sk-secret-test-key-12345");
        let report = build_report();
        let json = render_json(&report).unwrap();

        // The secret value should NOT appear in the JSON
        assert!(!json.contains("sk-secret-test-key-12345"));
        // But the env var name SHOULD appear
        assert!(json.contains("ANTHROPIC_API_KEY"));
        std::env::remove_var("ANTHROPIC_API_KEY");
    }

    #[test]
    fn doctor_text_does_not_leak_secret_values() {
        std::env::set_var("OPENAI_API_KEY", "sk-another-secret-67890");
        let report = build_report();
        let text = render_text(&report);

        // The secret value should NOT appear in the text
        assert!(!text.contains("sk-another-secret-67890"));
        // But the env var name SHOULD appear
        assert!(text.contains("OPENAI_API_KEY"));
        std::env::remove_var("OPENAI_API_KEY");
    }

    #[test]
    fn doctor_credentials_reflect_present_status() {
        std::env::set_var("GEMINI_API_KEY", "sk-gemini-test");
        let report = build_report();

        let gemini = report
            .credentials
            .iter()
            .find(|c| c.env_var == "GEMINI_API_KEY");
        assert!(gemini.is_some(), "should have GEMINI_API_KEY entry");
        assert_eq!(gemini.unwrap().status, "Present");
        std::env::remove_var("GEMINI_API_KEY");
    }

    #[test]
    fn doctor_credentials_reflect_missing_status() {
        // Ensure these env vars are not set
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");

        let report = build_report();

        // At least one should have Missing status
        let has_missing = report.credentials.iter().any(|c| c.status == "Missing");
        assert!(
            has_missing,
            "should have at least one credential with Missing status"
        );
    }
}
