// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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

    // Credentials: check router-relevant env vars via API model metadata.
    let aliases = lattice_api::list_models(false);
    let mut credential_entries: Vec<CredentialEntry> = Vec::new();
    for alias in &aliases {
        credential_entries.push(CredentialEntry {
            env_var: alias.api_key_env.clone(),
            status: alias.credential_status.clone(),
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

    output.push_str(&format!("{} LATTICE\n", "doctor".dimmed()));

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

/// Run CLI/API health diagnostics.
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
