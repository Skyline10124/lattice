// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use anyhow::Result;
use colored::Colorize;
use std::path::PathBuf;

use crate::env_boundary::{RejectReason, ValueSource};

/// Report structure for doctor diagnostics.
pub struct DoctorReport {
    pub config_loaded: bool,
    pub config_path: String,
    pub credentials: Vec<CredentialEntry>,
    pub endpoints: Vec<EndpointEntry>,
    pub dotenv: DotenvSummary,
    pub session_store_available: bool,
    pub session_dir: String,
    pub alias_count: usize,
}

/// A credential entry showing env var name, status, and where the value came from.
pub struct CredentialEntry {
    pub env_var: String,
    pub status: String,
    pub source: ValueSource,
}

/// An endpoint override entry: which env var can move a provider endpoint,
/// whether it is set, and where that value came from.
///
/// An endpoint URL itself is not a secret, so the effective value is printed —
/// a credential's value never is. Proxy URLs are the exception: they routinely
/// carry `user:password@`, so `value` is stored already passed through
/// [`redact_userinfo`].
pub struct EndpointEntry {
    pub env_var: String,
    pub value: Option<String>,
    pub source: ValueSource,
}

/// What the workspace `.env` load did, projected for display.
pub struct DotenvSummary {
    pub path: Option<String>,
    pub applied: Vec<String>,
    pub shadowed: Vec<String>,
    pub ignored: Vec<IgnoredEntry>,
    pub error: Option<String>,
}

/// A `.env` key the trust boundary refused, with the reason.
pub struct IgnoredEntry {
    pub key: String,
    pub reason: RejectReason,
}

/// Every provider credential env var the router can read
/// (`crates/lattice-llm/src/router.rs`). Reported unconditionally so a
/// credential's source is visible even when its alias does not currently
/// resolve.
const CREDENTIAL_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "OPENAI_COMPATIBLE_API_KEY",
];

/// Env vars that can move a provider endpoint. Kept beside the router's
/// `OPENAI_COMPATIBLE_BASE_URL` read (`crates/lattice-llm/src/router.rs`).
const ENDPOINT_ENV_VARS: &[&str] = &["OPENAI_COMPATIBLE_BASE_URL"];

/// Proxy vars `reqwest` honours by default, which reroute provider traffic just
/// as an endpoint override does. Listed only when set, so the common no-proxy
/// case stays quiet.
const PROXY_ENV_VARS: &[&str] = &[
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// Replace a URL's `user:password@` userinfo with `***@`, leaving the rest of
/// the URL readable.
///
/// Proxy URLs commonly embed credentials, and this report is the one place that
/// prints a URL value, so the value is redacted at the point it enters the
/// report rather than at each renderer. A string that is not a URL, or a URL
/// with no userinfo, is returned unchanged.
fn redact_userinfo(value: &str) -> String {
    let direct = url::Url::parse(value).ok().filter(|url| {
        matches!(
            url.scheme(),
            "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h"
        )
    });
    let (mut parsed, added_http) = match direct {
        Some(url) => (url, false),
        None => match url::Url::parse(&format!("http://{value}")) {
            Ok(url) => (url, true),
            Err(_) => return redact_unparsed_userinfo(value),
        },
    };
    if parsed.username().is_empty() && parsed.password().is_none() {
        return value.to_string();
    }
    let _ = parsed.set_username("***");
    let _ = parsed.set_password(None);
    let root_only = parsed.path() == "/" && parsed.query().is_none() && parsed.fragment().is_none();
    let mut redacted = parsed.to_string();
    if root_only && !value.ends_with('/') {
        redacted.pop();
    }
    if added_http {
        redacted
            .strip_prefix("http://")
            .unwrap_or(&redacted)
            .to_string()
    } else {
        redacted
    }
}

/// Fail closed for an invalid URL-like value that still visibly carries an
/// authority separator. Valid reqwest proxy inputs take the URL parser path;
/// this fallback keeps malformed diagnostics from becoming a secret sink.
fn redact_unparsed_userinfo(value: &str) -> String {
    match value.rsplit_once('@') {
        Some((_userinfo, host)) => format!("***@{host}"),
        None => value.to_string(),
    }
}

fn default_config_path() -> PathBuf {
    dirs::config_dir()
        .map(|dir| dir.join("lattice").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("lattice.toml"))
}

/// Build a diagnostic report.
pub fn build_report() -> Result<DoctorReport> {
    // Config
    let config_path = default_config_path();
    let config_loaded = config_path.exists();
    let config_path = config_path.display().to_string();

    let dotenv = crate::env_boundary::report();

    // Credentials: check router-relevant env vars via API model metadata.
    let aliases = lattice_api::list_models(false);
    let mut credential_entries: Vec<CredentialEntry> = Vec::new();
    for alias in &aliases {
        credential_entries.push(CredentialEntry {
            env_var: alias.api_key_env.clone(),
            status: alias.credential_status.clone(),
            source: dotenv.source_of(&alias.api_key_env),
        });
    }
    // Alias metadata only covers aliases that resolve, and the bare
    // `openai-compatible` alias needs `OPENAI_COMPATIBLE_MODEL` to resolve at
    // all — so without this its key would have no row, and the operator could
    // not see where that credential came from. Diagnostics should report every
    // credential the product can read, not only the currently reachable ones.
    for env_var in CREDENTIAL_ENV_VARS {
        credential_entries.push(CredentialEntry {
            env_var: (*env_var).to_string(),
            status: match std::env::var(env_var) {
                Ok(value) if !value.is_empty() => "Present".to_string(),
                _ => "Missing".to_string(),
            },
            source: dotenv.source_of(env_var),
        });
    }
    // Deduplicate by env var name
    credential_entries.sort_by(|a, b| a.env_var.cmp(&b.env_var));
    credential_entries.dedup_by(|a, b| a.env_var == b.env_var);

    // Endpoint overrides: always list the known provider endpoint vars so an
    // absent override is visible as `unset`, plus any proxy var in effect.
    let mut endpoints: Vec<EndpointEntry> = ENDPOINT_ENV_VARS
        .iter()
        .map(|env_var| EndpointEntry {
            env_var: (*env_var).to_string(),
            value: std::env::var(env_var).ok().map(|v| redact_userinfo(&v)),
            source: dotenv.source_of(env_var),
        })
        .collect();
    for env_var in PROXY_ENV_VARS {
        if let Ok(value) = std::env::var(env_var) {
            endpoints.push(EndpointEntry {
                env_var: (*env_var).to_string(),
                value: Some(redact_userinfo(&value)),
                source: dotenv.source_of(env_var),
            });
        }
    }

    // Session store availability
    // #143: the durable session store lives under the XDG *state* root.
    let session_dir = crate::commands::session::session_store_path()?;
    let session_store_available = session_dir.exists();

    // Alias table state
    let alias_count = aliases.len();

    Ok(DoctorReport {
        config_loaded,
        config_path,
        credentials: credential_entries,
        endpoints,
        dotenv: DotenvSummary {
            path: dotenv.path_display(),
            applied: dotenv.applied.clone(),
            shadowed: dotenv.shadowed.clone(),
            ignored: dotenv
                .ignored
                .iter()
                .map(|ignored| IgnoredEntry {
                    key: ignored.key.clone(),
                    reason: ignored.reason,
                })
                .collect(),
            error: dotenv.error.clone(),
        },
        session_store_available,
        session_dir: session_dir.display().to_string(),
        alias_count,
    })
}

/// Render report as JSON.
pub fn render_json(report: &DoctorReport) -> Result<String> {
    let out = serde_json::json!({
        "config_loaded": report.config_loaded,
        "config_path": report.config_path,
        "credentials": report.credentials.iter().map(|c| {
            serde_json::json!({ "env_var": c.env_var, "status": c.status, "source": c.source.as_str() })
        }).collect::<Vec<_>>(),
        "endpoints": report.endpoints.iter().map(|e| {
            serde_json::json!({ "env_var": e.env_var, "value": e.value, "source": e.source.as_str() })
        }).collect::<Vec<_>>(),
        "dotenv": {
            "path": report.dotenv.path,
            "applied": report.dotenv.applied,
            "shadowed": report.dotenv.shadowed,
            "ignored": report.dotenv.ignored.iter().map(|i| {
                serde_json::json!({
                    "key": i.key,
                    "reason": i.reason.as_str(),
                    "detail": i.reason.describe(),
                })
            }).collect::<Vec<_>>(),
            "error": report.dotenv.error,
        },
        "session_store_available": report.session_store_available,
        "session_dir": report.session_dir,
        "alias_count": report.alias_count,
    });
    Ok(serde_json::to_string_pretty(&out)?)
}

/// Human label for a `ValueSource` tag, so `process` and `dotenv` read as a
/// provenance statement rather than a bare word.
fn source_label(source: ValueSource) -> &'static str {
    match source {
        ValueSource::Dotenv => "workspace .env",
        ValueSource::Process => "process env",
        ValueSource::Unset => "unset",
    }
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
        output.push_str(&format!(
            "  {} {}: {} (source: {})\n",
            icon,
            cred.env_var,
            label,
            source_label(cred.source)
        ));
    }

    // Endpoints
    output.push_str(&format!("\n{}\n", "Endpoints:".bold()));
    for endpoint in &report.endpoints {
        match &endpoint.value {
            Some(value) => output.push_str(&format!(
                "  {} {}: {} (source: {})\n",
                "\u{2713}",
                endpoint.env_var,
                value,
                source_label(endpoint.source)
            )),
            None => output.push_str(&format!(
                "  {} {}: {} (source: {})\n",
                "\u{2717}",
                endpoint.env_var,
                "default".dimmed(),
                source_label(endpoint.source)
            )),
        }
    }

    // Workspace .env trust boundary
    output.push_str(&format!("\n{}\n", "Workspace .env:".bold()));
    match &report.dotenv.path {
        Some(path) => {
            output.push_str(&format!("  {} {}\n", "Path:".bold(), path));
            output.push_str(&format!(
                "  {} {}\n",
                "Applied:".bold(),
                if report.dotenv.applied.is_empty() {
                    "(none)".to_string()
                } else {
                    report.dotenv.applied.join(", ")
                }
            ));
            if !report.dotenv.shadowed.is_empty() {
                output.push_str(&format!(
                    "  {} {} (process environment wins)\n",
                    "Shadowed:".bold(),
                    report.dotenv.shadowed.join(", ")
                ));
            }
            if report.dotenv.ignored.is_empty() {
                output.push_str(&format!("  {} (none)\n", "Ignored:".bold()));
            } else {
                output.push_str(&format!(
                    "  {} untrusted keys dropped by the .env trust boundary\n",
                    "Ignored:".bold()
                ));
                for ignored in &report.dotenv.ignored {
                    output.push_str(&format!(
                        "    {} {}: {}\n",
                        "\u{2717}".yellow(),
                        ignored.key,
                        ignored.reason.describe()
                    ));
                }
            }
            if let Some(error) = &report.dotenv.error {
                output.push_str(&format!("  {} {}\n", "Error:".bold(), error.red()));
            }
        }
        None => output.push_str(&format!("  {} not found\n", "\u{2717}")),
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
    let report = build_report()?;
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
    fn userinfo_is_redacted_from_proxy_urls() {
        assert_eq!(
            redact_userinfo("http://puser:s3cr3t@proxy.internal:8080"),
            "http://***@proxy.internal:8080"
        );
        // A password containing `@` must not leave part of itself behind: the
        // split takes the last `@` in the authority.
        assert_eq!(
            redact_userinfo("http://user:pa@ss@proxy.internal:8080/path"),
            "http://***@proxy.internal:8080/path"
        );
        // A bare username is still userinfo.
        assert_eq!(
            redact_userinfo("socks5://user@127.0.0.1:1080"),
            "socks5://***@127.0.0.1:1080"
        );
        // reqwest accepts a proxy without a scheme by prepending `http://`,
        // so the diagnostic must treat this as an authority too.
        assert_eq!(
            redact_userinfo("user:s3cr3t@proxy.internal:8080"),
            "***@proxy.internal:8080"
        );
        assert_eq!(
            redact_userinfo("http:/user:s3cr3t@proxy.internal:8080"),
            "http://***@proxy.internal:8080"
        );
        assert_eq!(
            redact_userinfo("http:///user:s3cr3t@proxy.internal:8080"),
            "http://***@proxy.internal:8080"
        );
    }

    #[test]
    fn urls_without_userinfo_are_unchanged() {
        for url in [
            "http://localhost:11434/v1",
            "https://api.anthropic.com",
            "http://proxy.internal:8080",
            // An `@` after the authority is not userinfo.
            "https://gateway.example/v1/path@version",
            "not-a-url",
            "",
        ] {
            assert_eq!(redact_userinfo(url), url, "{url} must pass through");
        }
    }
}
