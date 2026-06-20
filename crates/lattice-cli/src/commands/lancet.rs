// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice lancet` — LANCET inspection and self-check subcommands.
//!
//! - `lancet status`: reads the external project-keyed audit store via
//!   `audit_db_path_for_project` (no CEP server) and shows the latest
//!   session.envelope fields, recent calls, and last guard verdict.
//! - `lancet doctor`: catalog count, transport smoke, invariant-card parse, and an
//!   isolated explicit `session.envelope` declaration smoke.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use colored::Colorize;
#[cfg(test)]
use lancet_api::collect_status_with_probe;
use lancet_api::{
    BoundaryDimension, HostProbeEntry, HostProbeReport, HostProbeStatus, LancetDoctorGate,
    LancetDoctorOutput, LancetDoctorRequest, LancetStatusOutput, collect_status, run_doctor_checks,
};

const BOUNDARY_NON_CLAIM_NOTE: &str =
    "Physical bounds only. No read-set observation, network rollback, or sandbox guarantee.";

/// `lattice lancet` subcommand group.
#[derive(Subcommand)]
pub enum LancetAction {
    /// Show audit-derived CEP status: active envelope, recent calls, last guard verdict.
    Status {
        /// Emit JSON instead of human-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Run LANCET self-check. Default is informational; --enforce-profile gates by profile-required command boundaries.
    Doctor {
        /// Enforce the named or path profile's [lancet.isolation.command] required_boundaries.
        #[arg(long, value_name = "PROFILE")]
        enforce_profile: Option<String>,
    },
}

/// Dispatch the `lancet` subcommand group. `None` action defaults to `Status`.
pub fn run(action: Option<LancetAction>, global_json: bool) -> Result<()> {
    match action.unwrap_or(LancetAction::Status { json: false }) {
        LancetAction::Status { json } => run_status(json || global_json),
        LancetAction::Doctor { enforce_profile } => run_doctor(global_json, enforce_profile),
    }
}

// ---------------------------------------------------------------------------
// lancet status
// ---------------------------------------------------------------------------

fn run_status(json: bool) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    let output = collect_status(&project_root)?;

    if json {
        let value = status_json_value(&output);
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        render_status_text(&output);
    }
    Ok(())
}

fn status_json_value(output: &LancetStatusOutput) -> serde_json::Value {
    serde_json::json!({
        "host_probe": host_probe_json_value(&output.host_probe),
        "active_envelope": output.active_envelope,
        "no_active_envelope_observed": output.no_active_envelope_observed,
        "recent_calls": output.recent_calls.iter().map(|c| serde_json::json!({
            "audit_id": c.audit_id,
            "method": c.method,
            "impact": c.impact,
            "verdict": c.verdict,
            "duration_ms": c.duration_ms,
            "effective_boundary": c.effective_boundary,
            "boundary_setup_failed_closed": c.boundary_setup_failed_closed,
        })).collect::<Vec<_>>(),
        "last_guard_verdict": output.last_guard_verdict,
    })
}

fn host_probe_json_value(host_probe: &HostProbeReport) -> serde_json::Value {
    serde_json::to_value(host_probe).unwrap_or_else(|_| serde_json::json!({}))
}

fn host_probe_reason(entry: &HostProbeEntry) -> &str {
    entry.reason.as_deref().unwrap_or("no_reason")
}

fn render_status_text(output: &LancetStatusOutput) {
    for line in status_text_lines(output) {
        println!("{line}");
    }
}

fn status_text_lines(output: &LancetStatusOutput) -> Vec<String> {
    let mut lines = vec!["lancet status".dimmed().to_string()];
    lines.push("Host Probe (current primitives):".bold().to_string());
    for entry in output.host_probe.entries() {
        lines.push(format!(
            "  {}={} ({})",
            entry.primitive.label(),
            host_probe_status_label(entry.status),
            host_probe_reason(entry)
        ));
    }
    match &output.active_envelope {
        Some(env) => {
            lines.push(format!("{}: {}", "Active Envelope".bold(), "found".green()));
            for key in &["intent", "capabilities", "scope", "impact"] {
                if let Some(val) = env.get(*key) {
                    lines.push(format!("  {} {}", format!("{key}:").green(), val));
                }
            }
        }
        None => lines.push(format!(
            "{}: {}",
            "Active Envelope".bold(),
            "no active envelope observed".yellow()
        )),
    }
    lines.push(String::new());
    lines.push("Recent CEP Calls:".bold().to_string());
    if output.recent_calls.is_empty() {
        lines.push(format!("  {}", "none".dimmed()));
    } else {
        for call in &output.recent_calls {
            let verdict = call.verdict.as_deref().unwrap_or("?");
            let dur = call
                .duration_ms
                .map(|d| format!("{}ms", d))
                .unwrap_or_default();
            lines.push(format!(
                "  [{}] {} {} {} {}",
                call.audit_id,
                call.method.cyan(),
                call.impact.dimmed(),
                verdict,
                dur
            ));
            if let Some(effective_boundary) = &call.effective_boundary {
                lines.push(format!(
                    "    {}",
                    format_effective_boundary_boundaries_line(effective_boundary)
                ));
                lines.push(format!(
                    "    {}",
                    format_effective_boundary_observed_line(effective_boundary)
                ));
                lines.push(format!("    Note:       {BOUNDARY_NON_CLAIM_NOTE}"));
                lines.push(format!(
                    "    effective_boundary: {}",
                    format_effective_boundary_compact(effective_boundary)
                ));
            }
        }
    }
    let verdict_str = output.last_guard_verdict.as_deref().unwrap_or("none");
    lines.push(String::new());
    lines.push(format!(
        "{}: {}",
        "Last Guard Verdict".bold(),
        verdict_str.dimmed()
    ));
    lines
}

fn format_effective_boundary_boundaries_line(
    effective_boundary: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let parts = BoundaryDimension::ALL
        .iter()
        .filter_map(|boundary| {
            let label = boundary.label();
            let entry = effective_boundary.get(label)?;
            Some(format!(
                "{label}={}",
                effective_boundary_text_status(*boundary, entry)
            ))
        })
        .collect::<Vec<_>>();

    if parts.is_empty() {
        "Boundaries: unknown".to_string()
    } else {
        format!("Boundaries: {}", parts.join(" "))
    }
}

fn format_effective_boundary_observed_line(
    effective_boundary: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let ordered = [
        BoundaryDimension::Filesystem,
        BoundaryDimension::Network,
        BoundaryDimension::Landlock,
    ];
    let parts = ordered
        .iter()
        .filter_map(|boundary| {
            let label = boundary.label();
            let observed = effective_boundary
                .get(label)?
                .get("observed")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            Some(format!("{label}={}", if observed { "yes" } else { "no" }))
        })
        .collect::<Vec<_>>();

    if parts.is_empty() {
        "Observed:   unknown".to_string()
    } else {
        format!("Observed:   {}", parts.join(" "))
    }
}

fn effective_boundary_text_status(boundary: BoundaryDimension, entry: &serde_json::Value) -> &str {
    if boundary == BoundaryDimension::Landlock
        && let Some(status) = entry
            .get("diagnostics")
            .and_then(|value| value.as_object())
            .and_then(|diagnostics| diagnostics.get("landlock_status"))
            .and_then(|value| value.as_str())
    {
        return status;
    }
    entry
        .get("status")
        .and_then(|status| status.as_str())
        .unwrap_or("unknown")
}

fn format_effective_boundary_compact(
    effective_boundary: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut parts = Vec::new();
    for boundary in BoundaryDimension::ALL {
        let label = boundary.label();
        if let Some(status) = effective_boundary
            .get(label)
            .and_then(|entry| entry.get("status"))
            .and_then(|status| status.as_str())
        {
            let details = format_effective_boundary_details(
                effective_boundary.get(label).expect("entry checked above"),
            );
            if details.is_empty() {
                parts.push(format!("{label}={status}"));
            } else {
                parts.push(format!("{label}={status}[{details}]"));
            }
        }
    }
    if parts.is_empty() {
        "unknown".to_string()
    } else {
        parts.join(", ")
    }
}

fn format_effective_boundary_details(entry: &serde_json::Value) -> String {
    let mut details = Vec::new();
    if let Some(sources) = entry
        .get("source_primitives")
        .and_then(|value| value.as_array())
    {
        let labels = sources
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>();
        if !labels.is_empty() {
            details.push(format!("source_primitives={}", labels.join("+")));
        }
    }

    let diagnostic_keys = [
        "namespace_status",
        "mount_view_status",
        "env_policy",
        "fd_cleanup_status",
        "fd_cleanup_method",
        "landlock_status",
        "landlock_abi_version",
        "landlock_rights_mask",
        "boundary_setup_failed_closed",
    ];
    if let Some(diagnostics) = entry.get("diagnostics").and_then(|value| value.as_object()) {
        for key in diagnostic_keys {
            if let Some(value) = diagnostics.get(key).and_then(|value| value.as_str()) {
                details.push(format!("{key}={value}"));
            }
        }
    }

    details.join(";")
}

// ---------------------------------------------------------------------------
// lancet doctor
// ---------------------------------------------------------------------------

/// Public entry point for LANCET runtime diagnostics.
pub fn run_doctor(json: bool, enforce_profile: Option<String>) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    let gate = enforce_profile
        .as_deref()
        .map(|profile| load_doctor_gate(profile, &project_root))
        .transpose()?;
    let mut request = LancetDoctorRequest::new(project_root);
    request.gate = gate;
    let output = run_doctor_checks(request);
    emit_doctor_result(&output, json)
}

#[cfg(test)]
pub(crate) fn run_doctor_inner_for_project_root(project_root: &Path, json: bool) -> Result<()> {
    let request = LancetDoctorRequest::new(project_root);
    let output = run_doctor_checks(request);
    emit_doctor_result(&output, json)
}

#[cfg(test)]
pub(crate) fn run_doctor_inner_with_probe(
    project_root: &Path,
    json: bool,
    host_probe: HostProbeReport,
    gate: Option<LancetDoctorGate>,
) -> Result<()> {
    let mut request = LancetDoctorRequest::new(project_root);
    request.host_probe = Some(host_probe);
    request.gate = gate;
    let output = run_doctor_checks(request);
    emit_doctor_result(&output, json)
}

fn load_doctor_gate(profile_spec: &str, project_root: &Path) -> Result<LancetDoctorGate> {
    let profile_path = PathBuf::from(profile_spec);
    let loaded =
        if profile_path.exists() || profile_spec.ends_with(".toml") || profile_spec.contains('/') {
            lattice_api::load_agent_profile_path(&profile_path)
                .with_context(|| format!("load profile {}", profile_path.display()))?
        } else {
            crate::commands::run::load_named_profile(profile_spec, project_root)
                .with_context(|| format!("load profile {profile_spec}"))?
        };

    let required_boundaries = loaded
        .profile
        .lancet
        .isolation
        .as_ref()
        .and_then(|isolation| isolation.command.as_ref())
        .map(|command| command.required_boundaries.clone())
        .unwrap_or_default();

    Ok(LancetDoctorGate {
        profile_label: profile_spec.to_string(),
        required_boundaries,
    })
}

fn host_probe_status_label(status: HostProbeStatus) -> &'static str {
    match status {
        HostProbeStatus::Available => "available",
        HostProbeStatus::Unavailable => "unavailable",
        HostProbeStatus::Disabled => "disabled",
        HostProbeStatus::Unknown => "unknown",
    }
}

fn emit_doctor_result(output: &LancetDoctorOutput, json: bool) -> Result<()> {
    let all_ok = output.issues.is_empty();

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&doctor_json_value(output))?
        );
    } else {
        println!("{}", "lancet doctor".dimmed());
        for item in &output.ok_items {
            println!("  {} {}", "\u{2713}".green(), item);
        }
        for warning in &output.warnings {
            println!("  {} {}", "!".yellow(), warning.yellow());
        }
        for issue in &output.issues {
            println!("  {} {}", "\u{2717}".red(), issue.red());
        }
    }

    if !all_ok {
        anyhow::bail!("lancet doctor: {} check(s) failed", output.issues.len());
    }
    Ok(())
}

fn doctor_json_value(output: &LancetDoctorOutput) -> serde_json::Value {
    let all_ok = output.issues.is_empty();
    let checks: Vec<_> = output
        .ok_items
        .iter()
        .map(|s| serde_json::json!({"status": "ok", "message": s}))
        .chain(
            output
                .warnings
                .iter()
                .map(|s| serde_json::json!({"status": "warn", "message": s})),
        )
        .chain(
            output
                .issues
                .iter()
                .map(|s| serde_json::json!({"status": "fail", "message": s})),
        )
        .collect();
    serde_json::json!({
        "ok": all_ok,
        "checks": checks,
        "doctor_mode": if output.gate_profile.is_some() { "profile_gate" } else { "informational" },
        "gate_profile": &output.gate_profile,
        "host_probe": host_probe_json_value(&output.host_probe),
        "gate_missing": output.gate_missing.iter().map(|item| serde_json::json!({
            "boundary": item.boundary,
            "primitive_reasons": item.primitive_reasons.iter().map(|entry| {
                serde_json::to_value(entry).unwrap_or_else(|_| serde_json::json!({}))
            }).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_env::ScopedXdgStateHome;
    use clap::Parser;
    use lancet_native::audit::{AuditOutcome, CallAuditMetadata};
    use lancet_native::{CepAuditContext, CepAuditStore};
    use tempfile::TempDir;

    // -- Clap surface tests --

    /// Test wrapper that wraps LancetAction as a top-level subcommand.
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: LancetAction,
    }

    fn try_parse_action(args: &[&str]) -> std::result::Result<LancetAction, clap::Error> {
        let mut full = vec!["lancet"];
        full.extend_from_slice(args);
        TestCli::try_parse_from(full).map(|c| c.action)
    }

    #[test]
    fn lancet_status_json_flag_parses() {
        let action = try_parse_action(&["status", "--json"]);
        assert!(action.is_ok(), "should parse `lancet status --json`");
        match action.unwrap() {
            LancetAction::Status { json } => assert!(json),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn lancet_status_no_args_parses() {
        let action = try_parse_action(&["status"]);
        assert!(action.is_ok());
        match action.unwrap() {
            LancetAction::Status { json } => assert!(!json),
            _ => panic!("expected Status"),
        }
    }

    #[test]
    fn lancet_doctor_parses() {
        let action = try_parse_action(&["doctor"]);
        assert!(action.is_ok());
        assert!(matches!(
            action.unwrap(),
            LancetAction::Doctor {
                enforce_profile: None
            }
        ));
    }

    #[test]
    fn lancet_doctor_enforce_profile_parses() {
        let action = try_parse_action(&["doctor", "--enforce-profile", "dev"]);
        assert!(action.is_ok());
        match action.unwrap() {
            LancetAction::Doctor { enforce_profile } => {
                assert_eq!(enforce_profile.as_deref(), Some("dev"));
            }
            _ => panic!("expected Doctor"),
        }
    }

    // -- Status tests --

    fn seed_audit_db(dir: &Path) {
        let db_path =
            lancet_api::audit_db_path_for_project(dir).expect("resolve external audit path");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let audit = CepAuditStore::open(&db_path).expect("open audit db");
        let ctx = CepAuditContext {
            turn_id: "test-turn".into(),
            session_id: "test-session".into(),
            model_id: "test-model".into(),
            runtime_version: "test".into(),
        };
        // session.envelope row with intent/capabilities in metadata_json.
        let start = audit
            .start_call(
                &ctx,
                1,
                "session.envelope",
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "declared".into(),
                },
                Some("native_handler"),
                Some(r#"{"intent":"smoke test","capabilities":["read"],"scope":"."}"#),
            )
            .unwrap();
        // A regular call row
        let start2 = audit
            .start_call(
                &ctx,
                2,
                "observe.workspace",
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start2,
                AuditOutcome::Success {
                    summary: "read ok".into(),
                },
                Some("native_handler"),
                None,
            )
            .unwrap();
    }

    #[test]
    fn cep_status_missing_db_returns_empty_state() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        let output = collect_status(dir.path()).unwrap();
        assert!(output.no_active_envelope_observed);
        assert!(output.active_envelope.is_none());
        assert!(output.recent_calls.is_empty());
        assert!(output.last_guard_verdict.is_none());
    }

    #[test]
    fn cep_status_seeded_audit_json() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_audit_db(dir.path());

        let output = collect_status(dir.path()).unwrap();

        // Should have parsed the declare row's metadata_json
        assert!(
            output.active_envelope.is_some() || !output.no_active_envelope_observed,
            "should observe envelope from declare row"
        );
        // Should have at least the two seeded rows
        assert!(!output.recent_calls.is_empty(), "should have recent calls");
        // Last guard verdict derived from newest row
        assert!(
            output.last_guard_verdict.is_some(),
            "should have last guard verdict"
        );
        // Check JSON schema keys
        let json = serde_json::json!({
            "active_envelope": output.active_envelope,
            "no_active_envelope_observed": output.no_active_envelope_observed,
            "recent_calls": output.recent_calls.iter().map(|c| serde_json::json!({
                "audit_id": c.audit_id,
                "method": c.method,
                "impact": c.impact,
                "verdict": c.verdict,
                "duration_ms": c.duration_ms,
            })).collect::<Vec<_>>(),
            "last_guard_verdict": output.last_guard_verdict,
        });
        assert!(
            json.get("active_envelope").is_some(),
            "JSON must have active_envelope"
        );
        assert!(
            json.get("no_active_envelope_observed").is_some(),
            "JSON must have no_active_envelope_observed"
        );
        assert!(
            json.get("recent_calls").is_some(),
            "JSON must have recent_calls"
        );
        assert!(
            json.get("last_guard_verdict").is_some(),
            "JSON must have last_guard_verdict"
        );

        // Verify method values in recent_calls
        let calls = json["recent_calls"].as_array().unwrap();
        let has_declare = calls
            .iter()
            .any(|c| c["method"].as_str() == Some("session.envelope"));
        assert!(
            has_declare,
            "recent_calls should include session.envelope row"
        );
    }

    // -- Doctor tests --

    #[test]
    fn cep_doctor_passes_on_repo_root() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");

        let result = run_doctor_inner_for_project_root(workspace_root, false);
        assert!(
            result.is_ok(),
            "doctor should pass on repo root: {:?}",
            result
        );
    }

    #[test]
    fn cep_doctor_envelope_declare_smoke_succeeds() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");
        let result = lancet_api::run_envelope_declare_smoke_for_project_root(workspace_root);
        assert!(
            result.is_ok(),
            "envelope declare smoke should succeed: {:?}",
            result
        );
    }

    fn unavailable_probe_for_tests() -> HostProbeReport {
        HostProbeReport::from_entries(lancet_api::BoundaryPrimitive::ALL.iter().map(|primitive| {
            HostProbeEntry::new(*primitive, HostProbeStatus::Unavailable, "test_unavailable")
        }))
    }

    fn mixed_probe_for_tests() -> HostProbeReport {
        HostProbeReport::from_entries([
            HostProbeEntry::new(
                lancet_api::BoundaryPrimitive::NetworkNamespace,
                HostProbeStatus::Unavailable,
                "netns requires privileges",
            )
            .with_diagnostic("helper.unshare", "/usr/bin/unshare"),
            HostProbeEntry::new(
                lancet_api::BoundaryPrimitive::MountNamespace,
                HostProbeStatus::Available,
                "mount namespace smoke passed",
            )
            .with_diagnostic("helper.unshare", "/usr/bin/unshare"),
            HostProbeEntry::new(
                lancet_api::BoundaryPrimitive::Overlayfs,
                HostProbeStatus::Disabled,
                "overlayfs disabled",
            )
            .with_diagnostic("helper.sh", "/usr/bin/sh"),
            HostProbeEntry::new(
                lancet_api::BoundaryPrimitive::Landlock,
                HostProbeStatus::Unknown,
                "landlock ABI probe inconclusive",
            ),
        ])
    }

    fn seed_command_boundary_audit_db(dir: &Path, method: &str, metadata_json: Option<&str>) {
        let db_path =
            lancet_api::audit_db_path_for_project(dir).expect("resolve external audit path");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let audit = CepAuditStore::open(&db_path).expect("open audit db");
        let ctx = CepAuditContext {
            turn_id: "boundary-turn".into(),
            session_id: "boundary-session".into(),
            model_id: "test-model".into(),
            runtime_version: "test".into(),
        };
        let start = audit
            .start_call(
                &ctx,
                1,
                method,
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        audit
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "command done".into(),
                },
                Some("native_handler"),
                metadata_json,
            )
            .unwrap();
    }

    #[test]
    fn lancet_doctor_json_includes_all_host_probe_primitives() {
        let dir = TempDir::new().unwrap();
        let mut request = LancetDoctorRequest::new(dir.path());
        request.host_probe = Some(mixed_probe_for_tests());
        let output = run_doctor_checks(request);
        let json = doctor_json_value(&output);
        let host_probe = json["host_probe"]["primitives"]
            .as_object()
            .expect("primitive-keyed host_probe");
        assert_eq!(host_probe.len(), lancet_api::BoundaryPrimitive::ALL.len());
        for primitive in lancet_api::BoundaryPrimitive::ALL {
            let entry = host_probe
                .get(primitive.label())
                .unwrap_or_else(|| panic!("missing primitive {}", primitive.label()));
            assert!(
                entry.get("status").is_some(),
                "status for {}",
                primitive.label()
            );
            assert!(
                entry.get("reason").is_some(),
                "reason for {}",
                primitive.label()
            );
        }
    }

    #[test]
    fn lancet_status_json_includes_host_probe_and_command_effective_boundary() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            dir.path(),
            "exec.command_run",
            Some(
                r#"{
                    "effective_boundary": {
                        "network": {"boundary":"network","status":"unbounded","reason":"netns unavailable"},
                        "filesystem": {"boundary":"filesystem","status":"failed_closed","reason":"overlay missing"},
                        "landlock": {"boundary":"landlock","status":"unsupported","reason":"ABI disabled"}
                    },
                    "boundary_setup_failed_closed": true
                }"#,
            ),
        );

        let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
        let json = status_json_value(&output);

        assert_eq!(
            json["host_probe"]["primitives"]["network_namespace"]["primitive"].as_str(),
            Some("network_namespace")
        );
        assert_eq!(
            json["recent_calls"][0]["effective_boundary"]["network"]["status"].as_str(),
            Some("unbounded")
        );
        assert_eq!(
            json["recent_calls"][0]["effective_boundary"]["network"]["observed"].as_bool(),
            Some(false)
        );
        assert!(
            json.get("observed_dimensions").is_none(),
            "status JSON must not add top-level observed summary lists"
        );
        assert!(
            json.get("unobserved_dimensions").is_none(),
            "status JSON must not add top-level unobserved summary lists"
        );
        let recent_call = json["recent_calls"][0]
            .as_object()
            .expect("recent call object");
        assert!(
            !recent_call.contains_key("observed_dimensions"),
            "recent call JSON must not add observed summary lists"
        );
        assert!(
            !recent_call.contains_key("unobserved_dimensions"),
            "recent call JSON must not add unobserved summary lists"
        );
        assert_eq!(
            output.recent_calls[0]
                .effective_boundary
                .as_ref()
                .unwrap()
                .get("filesystem")
                .and_then(|entry| entry.get("status"))
                .and_then(|status| status.as_str()),
            Some("failed_closed")
        );
        assert_eq!(
            json["recent_calls"][0]["boundary_setup_failed_closed"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn lancet_status_text_distinguishes_probe_from_effective_boundary() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            dir.path(),
            "exec.command_run",
            Some(
                r#"{
                    "effective_boundary": {
                        "network": {"boundary":"network","status":"unbounded","reason":"netns unavailable"}
                    }
                }"#,
            ),
        );
        let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
        let lines = status_text_lines(&output);

        assert!(
            lines
                .iter()
                .any(|line| line.contains("Host Probe (current primitives)")),
            "status text must label host probe as current primitive capability"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("effective_boundary: network=unbounded")),
            "status text must label per-command effective boundary"
        );
    }

    #[test]
    fn observed_non_claim() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            dir.path(),
            "exec.command_run",
            Some(
                r#"{
                    "effective_boundary": {
                        "network": {
                            "boundary": "network",
                            "status": "physically_bounded",
                            "observed": false,
                            "source_primitives": ["network_namespace"],
                            "diagnostics": {
                                "namespace_status": "applied",
                                "env_policy": "allowlist",
                                "fd_cleanup_status": "applied"
                            }
                        },
                        "filesystem": {
                            "boundary": "filesystem",
                            "status": "physically_bounded",
                            "observed": true,
                            "source_primitives": ["mount_namespace"],
                            "diagnostics": {
                                "namespace_status": "applied",
                                "mount_view_status": "applied",
                                "env_policy": "allowlist",
                                "fd_cleanup_status": "applied"
                            }
                        },
                        "landlock": {
                            "boundary": "landlock",
                            "status": "physically_bounded",
                            "observed": false,
                            "source_primitives": ["landlock"],
                            "diagnostics": {
                                "landlock_status": "applied",
                                "landlock_abi_version": "5",
                                "landlock_rights_mask": "0xffff"
                            }
                        }
                    }
                }"#,
            ),
        );

        let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
        let text = status_text_lines(&output).join("\n");

        assert!(
            text.contains(
                "Boundaries: network=physically_bounded filesystem=physically_bounded landlock=applied"
            ),
            "status text should render concise physical boundary statuses: {text}"
        );
        assert!(
            text.contains("Observed:   filesystem=yes network=no landlock=no"),
            "status text should render observed effects per boundary: {text}"
        );
        assert!(
            text.contains(
                "Note:       Physical bounds only. No read-set observation, network rollback, or sandbox guarantee."
            ),
            "status text should render the exact non-claim note: {text}"
        );

        let json = status_json_value(&output);
        assert_eq!(
            json["recent_calls"][0]["effective_boundary"]["network"]["observed"].as_bool(),
            Some(false)
        );
        assert!(
            json.get("observed_dimensions").is_none(),
            "status JSON must not add top-level observed summary lists"
        );
        assert!(
            json.get("unobserved_dimensions").is_none(),
            "status JSON must not add top-level unobserved summary lists"
        );
        let recent_call = json["recent_calls"][0]
            .as_object()
            .expect("recent call object");
        assert!(
            !recent_call.contains_key("observed_dimensions"),
            "recent call JSON must not add observed summary lists"
        );
        assert!(
            !recent_call.contains_key("unobserved_dimensions"),
            "recent call JSON must not add unobserved summary lists"
        );
    }

    #[test]
    fn lancet_status_ignores_effective_boundary_on_non_command_rows() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            dir.path(),
            "observe.workspace",
            Some(
                r#"{
                    "effective_boundary": {
                        "network": {"boundary":"network","status":"unbounded","reason":"should be ignored"}
                    }
                }"#,
            ),
        );

        let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
        assert!(output.recent_calls[0].effective_boundary.is_none());
    }

    #[test]
    fn lancet_status_reports_malformed_command_boundary_metadata_as_unknown() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            dir.path(),
            "exec.command_run",
            Some(r#"{"effective_boundary":{"network":{"status":"not-a-status"}}}"#),
        );

        let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
        let boundary = output.recent_calls[0]
            .effective_boundary
            .as_ref()
            .expect("effective_boundary");
        assert_eq!(
            boundary["network"]["status"].as_str(),
            Some("unknown"),
            "malformed boundary status must be reported as unknown"
        );
        assert!(
            boundary["network"]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("unknown effective_boundary status"),
            "unknown boundary must keep diagnostic reason"
        );
    }

    #[test]
    fn lancet_status_rejects_network_credit_from_non_netns_source() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            dir.path(),
            "exec.command_run",
            Some(
                r#"{
                    "effective_boundary": {
                        "network": {
                            "boundary": "network",
                            "status": "physically_bounded",
                            "source_primitives": ["landlock"],
                            "reason": "invalid seeded metadata"
                        },
                        "filesystem": {
                            "boundary": "filesystem",
                            "status": "physically_bounded",
                            "source_primitives": ["overlayfs"]
                        }
                    }
                }"#,
            ),
        );

        let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
        let boundary = output.recent_calls[0]
            .effective_boundary
            .as_ref()
            .expect("effective_boundary");
        assert_eq!(
            boundary["network"]["status"].as_str(),
            Some("unknown"),
            "network must not receive physical credit from Landlock"
        );
        assert_eq!(
            boundary["filesystem"]["status"].as_str(),
            Some("physically_bounded"),
            "filesystem may preserve overlay-backed physical credit"
        );
        assert!(
            boundary["network"]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("network_namespace"),
            "network downgrade should explain netns requirement"
        );
    }

    #[test]
    fn lancet_status_preserves_phase29_applied_boundary_diagnostics() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            dir.path(),
            "exec.command_run",
            Some(
                r#"{
                    "effective_boundary": {
                        "network": {
                            "boundary": "network",
                            "status": "physically_bounded",
                            "source_primitives": ["network_namespace"],
                            "diagnostics": {
                                "namespace_status": "applied",
                                "env_policy": "allowlist",
                                "fd_cleanup_status": "applied",
                                "fd_cleanup_method": "close_range",
                                "boundary_setup_failed_closed": "false"
                            }
                        },
                        "filesystem": {
                            "boundary": "filesystem",
                            "status": "physically_bounded",
                            "source_primitives": ["mount_namespace"],
                            "diagnostics": {
                                "namespace_status": "applied",
                                "mount_view_status": "applied",
                                "env_policy": "allowlist",
                                "fd_cleanup_status": "applied"
                            }
                        },
                        "landlock": {
                            "boundary": "landlock",
                            "status": "physically_bounded",
                            "source_primitives": ["landlock"],
                            "diagnostics": {
                                "landlock_status": "applied",
                                "landlock_abi_version": "5",
                                "landlock_rights_mask": "0xffff",
                                "env_policy": "allowlist",
                                "fd_cleanup_status": "applied"
                            }
                        }
                    },
                    "boundary_setup_failed_closed": false
                }"#,
            ),
        );

        let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
        let json = status_json_value(&output);
        let boundary = &json["recent_calls"][0]["effective_boundary"];

        assert_eq!(boundary["network"]["status"], "physically_bounded");
        assert_eq!(
            boundary["network"]["source_primitives"][0],
            "network_namespace"
        );
        assert_eq!(
            boundary["network"]["diagnostics"]["namespace_status"],
            "applied"
        );
        assert_eq!(
            boundary["network"]["diagnostics"]["env_policy"],
            "allowlist"
        );
        assert_eq!(
            boundary["network"]["diagnostics"]["fd_cleanup_status"],
            "applied"
        );
        assert_eq!(
            boundary["network"]["diagnostics"]["fd_cleanup_method"],
            "close_range"
        );
        assert_eq!(
            boundary["filesystem"]["diagnostics"]["mount_view_status"],
            "applied"
        );
        assert_eq!(
            boundary["landlock"]["diagnostics"]["landlock_status"],
            "applied"
        );
        assert_eq!(
            boundary["landlock"]["diagnostics"]["landlock_abi_version"],
            "5"
        );
        assert_eq!(
            boundary["landlock"]["diagnostics"]["landlock_rights_mask"],
            "0xffff"
        );
        assert_eq!(
            json["recent_calls"][0]["boundary_setup_failed_closed"].as_bool(),
            Some(false)
        );

        let text = status_text_lines(&output).join("\n");
        assert!(
            text.contains("network=physically_bounded[source_primitives=network_namespace;namespace_status=applied;env_policy=allowlist;fd_cleanup_status=applied;fd_cleanup_method=close_range")
        );
        assert!(text.contains("landlock_status=applied"));
        assert!(text.contains("landlock_rights_mask=0xffff"));
    }

    #[test]
    fn lancet_status_preserves_all_landlock_status_diagnostics() {
        let _xdg = ScopedXdgStateHome::new();
        for (status, expected_boundary_status) in [
            ("unsupported", "unsupported"),
            ("not_applied", "unbounded"),
            ("applied", "physically_bounded"),
        ] {
            let dir = TempDir::new().unwrap();
            let source = if status == "applied" {
                r#","source_primitives":["landlock"]"#
            } else {
                ""
            };
            let metadata = format!(
                r#"{{
                    "effective_boundary": {{
                        "landlock": {{
                            "boundary": "landlock",
                            "status": "{expected_boundary_status}",
                            "diagnostics": {{
                                "landlock_status": "{status}",
                                "landlock_abi_version": "3",
                                "landlock_rights_mask": "0x7fff"
                            }}
                            {source}
                        }}
                    }}
                }}"#
            );
            seed_command_boundary_audit_db(dir.path(), "exec.command_run", Some(&metadata));

            let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
            let boundary = output.recent_calls[0]
                .effective_boundary
                .as_ref()
                .expect("effective_boundary");

            assert_eq!(
                boundary["landlock"]["diagnostics"]["landlock_status"].as_str(),
                Some(status)
            );
            assert_eq!(
                boundary["landlock"]["diagnostics"]["landlock_abi_version"].as_str(),
                Some("3")
            );
            assert_eq!(
                boundary["landlock"]["diagnostics"]["landlock_rights_mask"].as_str(),
                Some("0x7fff")
            );
            assert_eq!(
                boundary["landlock"]["status"].as_str(),
                Some(expected_boundary_status)
            );
        }
    }

    #[test]
    fn lancet_status_preserves_landlock_applied_with_mount_view_failure() {
        let _xdg = ScopedXdgStateHome::new();
        let dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            dir.path(),
            "exec.command_run",
            Some(
                r#"{
                    "effective_boundary": {
                        "filesystem": {
                            "boundary": "filesystem",
                            "status": "failed_closed",
                            "diagnostics": {
                                "namespace_status": "applied",
                                "mount_view_status": "failed_closed",
                                "env_policy": "allowlist",
                                "fd_cleanup_status": "applied"
                            }
                        },
                        "landlock": {
                            "boundary": "landlock",
                            "status": "physically_bounded",
                            "source_primitives": ["landlock"],
                            "diagnostics": {
                                "landlock_status": "applied",
                                "landlock_abi_version": "4",
                                "landlock_rights_mask": "0xffff"
                            }
                        }
                    }
                }"#,
            ),
        );

        let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
        let boundary = output.recent_calls[0]
            .effective_boundary
            .as_ref()
            .expect("effective_boundary");

        assert_eq!(
            boundary["filesystem"]["diagnostics"]["mount_view_status"].as_str(),
            Some("failed_closed")
        );
        assert_eq!(
            boundary["filesystem"]["status"].as_str(),
            Some("failed_closed")
        );
        assert_eq!(
            boundary["landlock"]["diagnostics"]["landlock_status"].as_str(),
            Some("applied")
        );
        assert_eq!(
            boundary["landlock"]["status"].as_str(),
            Some("physically_bounded")
        );
    }

    #[test]
    fn lancet_status_accepts_only_netns_for_network_physical_credit() {
        let _xdg = ScopedXdgStateHome::new();
        let invalid_cases = [
            (
                "landlock",
                r#""source_primitives": ["landlock"]"#,
                "network_namespace",
            ),
            (
                "mount namespace",
                r#""source_primitives": ["mount_namespace"]"#,
                "network_namespace",
            ),
            (
                "overlayfs",
                r#""source_primitives": ["overlayfs"]"#,
                "network_namespace",
            ),
            (
                "missing source primitives",
                r#""reason": "stale preflight""#,
                "source_primitives",
            ),
            (
                "malformed source primitives",
                r#""source_primitives": ["network_namespace", 7]"#,
                "source_primitives must contain only primitive labels",
            ),
            (
                "stale preflight-only metadata",
                r#""diagnostics": {"host_probe_network_namespace": "available"}"#,
                "source_primitives",
            ),
        ];

        let valid_dir = TempDir::new().unwrap();
        seed_command_boundary_audit_db(
            valid_dir.path(),
            "exec.command_run",
            Some(
                r#"{
                    "effective_boundary": {
                        "network": {
                            "boundary": "network",
                            "status": "physically_bounded",
                            "source_primitives": ["network_namespace"]
                        }
                    }
                }"#,
            ),
        );
        let valid = collect_status_with_probe(valid_dir.path(), mixed_probe_for_tests()).unwrap();
        assert_eq!(
            valid.recent_calls[0].effective_boundary.as_ref().unwrap()["network"]["status"]
                .as_str(),
            Some("physically_bounded")
        );

        for (name, source_fragment, expected_reason) in invalid_cases {
            let dir = TempDir::new().unwrap();
            let metadata = format!(
                r#"{{
                    "effective_boundary": {{
                        "network": {{
                            "boundary": "network",
                            "status": "physically_bounded",
                            {source_fragment}
                        }}
                    }}
                }}"#
            );
            seed_command_boundary_audit_db(dir.path(), "exec.command_run", Some(&metadata));
            let output = collect_status_with_probe(dir.path(), mixed_probe_for_tests()).unwrap();
            let network = &output.recent_calls[0].effective_boundary.as_ref().unwrap()["network"];

            assert_eq!(
                network["status"].as_str(),
                Some("unknown"),
                "{name} must not earn network physical credit"
            );
            assert!(
                network["reason"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(expected_reason),
                "{name} reason should mention {expected_reason}: {network}"
            );
        }
    }

    #[test]
    fn cep_doctor_default_mode_is_informational_for_unavailable_boundaries() {
        let dir = TempDir::new().unwrap();
        let result =
            run_doctor_inner_with_probe(dir.path(), false, unavailable_probe_for_tests(), None);
        assert!(
            result.is_ok(),
            "default doctor should report unavailable primitives without failing"
        );
    }

    #[test]
    fn cep_doctor_profile_gate_fails_for_missing_required_boundary() {
        let dir = TempDir::new().unwrap();
        let result = run_doctor_inner_with_probe(
            dir.path(),
            false,
            unavailable_probe_for_tests(),
            Some(LancetDoctorGate {
                profile_label: "test-profile".into(),
                required_boundaries: vec![BoundaryDimension::Network],
            }),
        );
        assert!(
            result.is_err(),
            "profile gate should fail when required network boundary is unavailable"
        );
    }

    #[test]
    fn cep_doctor_profile_gate_lists_exact_missing_boundary_reason() {
        let dir = TempDir::new().unwrap();
        let mut request = LancetDoctorRequest::new(dir.path());
        request.host_probe = Some(unavailable_probe_for_tests());
        request.gate = Some(LancetDoctorGate {
            profile_label: "test-profile".into(),
            required_boundaries: vec![BoundaryDimension::Network],
        });
        let output = run_doctor_checks(request);
        assert_eq!(output.gate_missing.len(), 1);
        assert_eq!(output.gate_missing[0].boundary, BoundaryDimension::Network);
        let rendered = doctor_json_value(&output);
        let reasons = rendered["gate_missing"][0]["primitive_reasons"]
            .as_array()
            .expect("primitive reasons");
        assert!(
            reasons
                .iter()
                .any(|entry| entry["primitive"].as_str() == Some("network_namespace")),
            "got: {rendered}"
        );
        assert!(
            reasons
                .iter()
                .any(|entry| entry["reason"].as_str() == Some("test_unavailable")),
            "got: {rendered}"
        );
    }
}
