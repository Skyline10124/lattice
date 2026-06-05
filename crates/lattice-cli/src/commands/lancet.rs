// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `lattice lancet` — LANCET inspection and self-check subcommands.
//!
//! - `lancet status`: reads `.lattice/cep_audit.db` directly (no CEP server) and shows
//!   the latest session.envelope fields, recent calls, and last guard verdict.
//! - `lancet doctor`: catalog count, transport smoke, invariant-card parse, and an
//!   isolated `session.envelope` derive-intent smoke that asserts `unknowns == 0`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Subcommand;
use colored::Colorize;
use lancet_authority::{allowed_primitives_for, CepTurnContext};
use lancet_catalog::catalog::catalog;
use lancet_catalog::methods::envelope::{
    Envelope, EnvelopeParams, EnvelopeResult, SubmitIntentParams,
};
use lancet_invariants::invariants::loader::load_cards_from_dir;
use lancet_invariants::resolve_invariants_dir;
use lancet_native::{
    probe_host_capabilities, unavailable_required_boundaries, HostProbeReport,
    MissingRequiredBoundary,
};
use lancet_native::{
    register_production_method_registry, AuditQueryFilters, CepAuditStore, CepTransport,
    MethodRegistry, ServerRunConfig,
};
use lancet_protocol::envelope::BaseExecutionEnvelope;
use lancet_protocol::{BoundaryDimension, BoundaryPrimitive, EffectiveBoundaryStatus};

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
// Shared helpers
// ---------------------------------------------------------------------------

fn audit_db_path(project_root: &Path) -> PathBuf {
    project_root.join(".lattice").join("cep_audit.db")
}

fn compute_duration_ms(started_at: &str, ended_at: Option<&str>) -> Option<i64> {
    let ended = ended_at?;
    let start = chrono::DateTime::parse_from_rfc3339(started_at).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(ended).ok()?;
    Some(end.signed_duration_since(start).num_milliseconds())
}

// ---------------------------------------------------------------------------
// lancet status
// ---------------------------------------------------------------------------

pub(crate) struct CepStatusOutput {
    pub host_probe: HostProbeReport,
    pub active_envelope: Option<serde_json::Value>,
    pub no_active_envelope_observed: bool,
    pub recent_calls: Vec<RecentCallEntry>,
    pub last_guard_verdict: Option<String>,
}

pub(crate) struct RecentCallEntry {
    pub audit_id: i64,
    pub method: String,
    pub impact: String,
    pub verdict: Option<String>,
    pub duration_ms: Option<i64>,
    pub effective_boundary: Option<serde_json::Map<String, serde_json::Value>>,
    pub boundary_setup_failed_closed: Option<bool>,
}

/// Build status data from the audit store without spawning a CEP server.
pub(crate) fn collect_status(project_root: &Path) -> Result<CepStatusOutput> {
    collect_status_with_probe(project_root, probe_host_capabilities())
}

fn collect_status_with_probe(
    project_root: &Path,
    host_probe: HostProbeReport,
) -> Result<CepStatusOutput> {
    let db_path = audit_db_path(project_root);

    if !db_path.exists() {
        return Ok(CepStatusOutput {
            host_probe,
            active_envelope: None,
            no_active_envelope_observed: true,
            recent_calls: vec![],
            last_guard_verdict: None,
        });
    }

    let store =
        CepAuditStore::open(&db_path).map_err(|e| anyhow::anyhow!("open audit store: {e}"))?;

    // Latest session.envelope row — raw metadata_json only.
    let declare_rows = store
        .query_rows(&AuditQueryFilters {
            method: Some("session.envelope".to_string()),
            ..Default::default()
        })
        .map_err(|e| anyhow::anyhow!("query session.envelope rows: {e}"))?;

    let active_envelope = declare_rows.last().and_then(|row| {
        row.metadata_json
            .as_deref()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
    });
    let no_active_envelope_observed = active_envelope.is_none();

    // All rows for recent-calls window and last guard verdict.
    let all_rows = store
        .query_rows(&AuditQueryFilters::default())
        .map_err(|e| anyhow::anyhow!("query all rows: {e}"))?;

    let last_guard_verdict = all_rows.last().map(|r| {
        r.semantic_status
            .clone()
            .unwrap_or_else(|| r.guard_stage.clone())
    });

    let recent_start = all_rows.len().saturating_sub(20);
    let recent_calls = all_rows[recent_start..]
        .iter()
        .rev()
        .map(|row| RecentCallEntry {
            audit_id: row.audit_id,
            method: row.method.clone(),
            impact: row.impact_kind.clone(),
            verdict: row.semantic_status.clone(),
            duration_ms: compute_duration_ms(row.started_at.as_str(), row.ended_at.as_deref()),
            effective_boundary: parse_effective_boundary_metadata(
                &row.method,
                row.metadata_json.as_deref(),
            ),
            boundary_setup_failed_closed: parse_boundary_setup_failed_closed_metadata(
                &row.method,
                row.metadata_json.as_deref(),
            ),
        })
        .collect();

    Ok(CepStatusOutput {
        host_probe,
        active_envelope,
        no_active_envelope_observed,
        recent_calls,
        last_guard_verdict,
    })
}

fn parse_effective_boundary_metadata(
    method: &str,
    metadata_json: Option<&str>,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if method != "exec.command_run" {
        return None;
    }
    let metadata_json = metadata_json?;
    let metadata = match serde_json::from_str::<serde_json::Value>(metadata_json) {
        Ok(value) => value,
        Err(err) => {
            return Some(unknown_effective_boundary(format!(
                "malformed metadata_json: {err}"
            )));
        }
    };
    let boundary_value = metadata.get("effective_boundary")?;
    let Some(boundary_map) = boundary_value.as_object() else {
        return Some(unknown_effective_boundary(
            "effective_boundary metadata is not an object".to_string(),
        ));
    };
    if boundary_map.is_empty() {
        return None;
    }

    let mut parsed = serde_json::Map::new();
    for boundary in BoundaryDimension::ALL {
        let label = boundary.label();
        if let Some(entry) = boundary_map.get(label) {
            parsed.insert(
                label.to_string(),
                normalize_effective_boundary_entry(*boundary, entry),
            );
        }
    }
    if parsed.is_empty() {
        Some(unknown_effective_boundary(
            "effective_boundary metadata has no known boundary keys".to_string(),
        ))
    } else {
        Some(parsed)
    }
}

fn parse_boundary_setup_failed_closed_metadata(
    method: &str,
    metadata_json: Option<&str>,
) -> Option<bool> {
    if method != "exec.command_run" {
        return None;
    }
    let metadata = serde_json::from_str::<serde_json::Value>(metadata_json?).ok()?;
    metadata
        .get("boundary_setup_failed_closed")
        .and_then(|value| value.as_bool())
}

fn normalize_effective_boundary_entry(
    boundary: BoundaryDimension,
    entry: &serde_json::Value,
) -> serde_json::Value {
    let Some(object) = entry.as_object() else {
        return unknown_effective_boundary_entry(
            boundary,
            "effective_boundary entry is not an object".to_string(),
        );
    };
    let Some(status) = object.get("status").and_then(|value| value.as_str()) else {
        return unknown_effective_boundary_entry(
            boundary,
            "effective_boundary entry missing status".to_string(),
        );
    };
    if !is_known_effective_boundary_status(status) {
        return unknown_effective_boundary_entry(
            boundary,
            format!("unknown effective_boundary status `{status}`"),
        );
    }
    if status == "physically_bounded" {
        if let Err(reason) = validate_physically_bounded_attribution(boundary, object) {
            return unknown_effective_boundary_entry(boundary, reason);
        }
    }
    let mut normalized = object.clone();
    normalized
        .entry("boundary".to_string())
        .or_insert_with(|| serde_json::Value::String(boundary.label().to_string()));
    if !normalized
        .get("observed")
        .is_some_and(serde_json::Value::is_boolean)
    {
        normalized.insert("observed".to_string(), serde_json::Value::Bool(false));
    }
    serde_json::Value::Object(normalized)
}

fn validate_physically_bounded_attribution(
    boundary: BoundaryDimension,
    object: &serde_json::Map<String, serde_json::Value>,
) -> std::result::Result<(), String> {
    let allowed = allowed_primitives_for(boundary);
    let allowed_labels = allowed
        .iter()
        .map(|primitive| primitive.label())
        .collect::<Vec<_>>()
        .join(", ");
    let source_primitives = object
        .get("source_primitives")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            format!(
                "physically_bounded {} boundary requires source_primitives containing one of: {allowed_labels}",
                boundary.label()
            )
        })?;

    let mut parsed_sources = Vec::new();
    for value in source_primitives {
        let Some(label) = value.as_str() else {
            return Err(format!(
                "physically_bounded {} boundary source_primitives must contain only primitive labels",
                boundary.label()
            ));
        };
        let Some(primitive) = boundary_primitive_from_label(label) else {
            return Err(format!(
                "physically_bounded {} boundary source_primitives contains unknown primitive `{label}`",
                boundary.label()
            ));
        };
        parsed_sources.push(primitive);
    }

    if parsed_sources
        .iter()
        .any(|primitive| allowed.contains(primitive))
    {
        Ok(())
    } else {
        Err(format!(
            "physically_bounded {} boundary requires source_primitives containing one of: {allowed_labels}",
            boundary.label()
        ))
    }
}

fn boundary_primitive_from_label(label: &str) -> Option<BoundaryPrimitive> {
    BoundaryPrimitive::ALL
        .iter()
        .copied()
        .find(|primitive| primitive.label() == label)
}

fn is_known_effective_boundary_status(status: &str) -> bool {
    matches!(
        status,
        "physically_bounded"
            | "unbounded"
            | "not_requested"
            | "failed_closed"
            | "unsupported"
            | "unknown"
    )
}

fn unknown_effective_boundary(reason: String) -> serde_json::Map<String, serde_json::Value> {
    BoundaryDimension::ALL
        .iter()
        .map(|boundary| {
            (
                boundary.label().to_string(),
                unknown_effective_boundary_entry(*boundary, reason.clone()),
            )
        })
        .collect()
}

fn unknown_effective_boundary_entry(
    boundary: BoundaryDimension,
    reason: String,
) -> serde_json::Value {
    serde_json::json!({
        "boundary": boundary,
        "status": EffectiveBoundaryStatus::Unknown,
        "observed": false,
        "reason": reason,
    })
}

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

fn status_json_value(output: &CepStatusOutput) -> serde_json::Value {
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

fn host_probe_reason(entry: &lancet_protocol::HostProbeEntry) -> &str {
    entry.reason.as_deref().unwrap_or("no_reason")
}

fn render_status_text(output: &CepStatusOutput) {
    for line in status_text_lines(output) {
        println!("{line}");
    }
}

fn status_text_lines(output: &CepStatusOutput) -> Vec<String> {
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
    if boundary == BoundaryDimension::Landlock {
        if let Some(status) = entry
            .get("diagnostics")
            .and_then(|value| value.as_object())
            .and_then(|diagnostics| diagnostics.get("landlock_status"))
            .and_then(|value| value.as_str())
        {
            return status;
        }
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

/// Public entry point — derives invariants dir from `$PWD/.lancet/invariants/`.
pub fn run_doctor(json: bool, enforce_profile: Option<String>) -> Result<()> {
    let project_root = std::env::current_dir().context("current working directory")?;
    let invariants_dir = resolve_invariants_dir(&project_root, None);
    let gate = enforce_profile
        .as_deref()
        .map(|profile| load_doctor_gate(profile, &project_root))
        .transpose()?;
    run_doctor_inner_with_probe(
        &project_root,
        &invariants_dir,
        json,
        probe_host_capabilities(),
        gate,
    )
}

/// Testable core — accepts an explicit invariants directory.
#[cfg(test)]
pub(crate) fn run_doctor_inner(invariants_dir: &Path, json: bool) -> Result<()> {
    let project_root = invariants_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or(invariants_dir);
    run_doctor_inner_with_probe(
        project_root,
        invariants_dir,
        json,
        probe_host_capabilities(),
        None,
    )
}

#[cfg(test)]
pub(crate) fn run_doctor_inner_for_project_root(
    project_root: &Path,
    invariants_dir: &Path,
    json: bool,
) -> Result<()> {
    run_doctor_inner_with_probe(
        project_root,
        invariants_dir,
        json,
        probe_host_capabilities(),
        None,
    )
}

#[derive(Clone, Debug)]
pub(crate) struct DoctorGateInput {
    profile_label: String,
    required_boundaries: Vec<BoundaryDimension>,
}

#[derive(Clone, Debug)]
struct DoctorOutput {
    ok_items: Vec<String>,
    warnings: Vec<String>,
    issues: Vec<String>,
    card_ids: Vec<String>,
    host_probe: HostProbeReport,
    gate_profile: Option<String>,
    gate_missing: Vec<MissingRequiredBoundary>,
}

pub(crate) fn run_doctor_inner_with_probe(
    project_root: &Path,
    invariants_dir: &Path,
    json: bool,
    host_probe: HostProbeReport,
    gate: Option<DoctorGateInput>,
) -> Result<()> {
    let output = collect_doctor_output(project_root, invariants_dir, host_probe, gate);
    emit_doctor_result(&output, json)
}

fn collect_doctor_output(
    project_root: &Path,
    invariants_dir: &Path,
    host_probe: HostProbeReport,
    gate: Option<DoctorGateInput>,
) -> DoctorOutput {
    let mut ok_items: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut issues: Vec<String> = Vec::new();
    let mut gate_profile = None;
    let mut gate_missing = Vec::new();

    if gate.is_none() {
        ok_items.push(
            "doctor mode: informational (default exit 0; use --enforce-profile <profile> for a profile gate)"
                .to_string(),
        );
    }

    // 1. CATALOG method count (must be exactly 9).
    let catalog_len = catalog().len();
    if catalog_len == 9 {
        ok_items.push(format!("catalog: {catalog_len} methods (expected 9)"));
    } else {
        issues.push(format!("catalog: expected 9 methods, got {catalog_len}"));
    }

    // 2. Production method registry registers without error.
    {
        let mut registry = MethodRegistry::new();
        match register_production_method_registry(&mut registry) {
            Ok(()) => ok_items.push("registry: production methods registered".to_string()),
            Err(e) => issues.push(format!("registry: {}", e.message)),
        }
    }

    // 3. Invariant cards — parse errors fail doctor.
    let card_ids: Vec<String> = match load_cards_from_dir(invariants_dir) {
        Ok(cards) if cards.is_empty() => {
            ok_items.push("invariants: ok_empty (no cards)".to_string());
            vec![]
        }
        Ok(cards) => {
            let ids: Vec<String> = cards.iter().map(|c| c.id.clone()).collect();
            ok_items.push(format!(
                "invariants: ok_loaded ({} card(s): {})",
                ids.len(),
                ids.join(", ")
            ));
            ids
        }
        Err(e) => {
            issues.push(format!("invariants: parse error: {e}"));
            return DoctorOutput {
                ok_items,
                warnings,
                issues,
                card_ids: vec![],
                host_probe,
                gate_profile,
                gate_missing,
            };
        }
    };

    // 4. host capability probe summary. Unavailable primitives are warnings in
    // default informational mode, not failures.
    for entry in host_probe.entries() {
        match entry.status {
            lancet_protocol::HostProbeStatus::Available => {
                ok_items.push(format!("host_probe: {} available", entry.primitive.label()));
            }
            _ => warnings.push(format!(
                "host_probe: {} {} ({})",
                entry.primitive.label(),
                host_probe_status_label(entry.status),
                host_probe_reason(entry)
            )),
        }
    }

    // 5. Optional profile-bound gate for release/dogfood checks.
    if let Some(gate) = gate {
        gate_profile = Some(gate.profile_label.clone());
        ok_items.push(format!(
            "doctor mode: profile gate for {}",
            gate.profile_label
        ));
        if gate.required_boundaries.is_empty() {
            warnings.push(format!(
                "isolation gate: profile {} declares no required command boundaries",
                gate.profile_label
            ));
        } else {
            let missing = unavailable_required_boundaries(&host_probe, &gate.required_boundaries);
            if missing.is_empty() {
                ok_items.push(format!(
                    "isolation gate: profile {} required boundaries available",
                    gate.profile_label
                ));
            } else {
                for item in &missing {
                    let details = format_missing_boundary_details(item);
                    issues.push(format!(
                        "isolation gate: required boundary {} unavailable for profile {} ({details})",
                        item.boundary.label(),
                        gate.profile_label
                    ));
                }
                gate_missing = missing;
            }
        }
    }

    // 6. submit_intent smoke in isolated temp workspace.
    match run_submit_intent_smoke_for_project_root(project_root) {
        Ok(()) => ok_items.push("submit_intent smoke: ok (unknowns == 0)".to_string()),
        Err(e) => issues.push(format!("submit_intent smoke: {e}")),
    }

    DoctorOutput {
        ok_items,
        warnings,
        issues,
        card_ids,
        host_probe,
        gate_profile,
        gate_missing,
    }
}

fn load_doctor_gate(profile_spec: &str, project_root: &Path) -> Result<DoctorGateInput> {
    let profile_path = PathBuf::from(profile_spec);
    let loaded =
        if profile_path.exists() || profile_spec.ends_with(".toml") || profile_spec.contains('/') {
            lattice_profile::load_agent_profile(&profile_path)
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

    Ok(DoctorGateInput {
        profile_label: profile_spec.to_string(),
        required_boundaries,
    })
}

fn format_missing_boundary_details(item: &MissingRequiredBoundary) -> String {
    if item.primitive_reasons.is_empty() {
        return "no matching primitive probe result".to_string();
    }
    item.primitive_reasons
        .iter()
        .map(|entry| {
            format!(
                "{}={}:{}",
                entry.primitive.label(),
                host_probe_status_label(entry.status),
                host_probe_reason(entry)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn host_probe_status_label(status: lancet_protocol::HostProbeStatus) -> &'static str {
    match status {
        lancet_protocol::HostProbeStatus::Available => "available",
        lancet_protocol::HostProbeStatus::Unavailable => "unavailable",
        lancet_protocol::HostProbeStatus::Disabled => "disabled",
        lancet_protocol::HostProbeStatus::Unknown => "unknown",
    }
}

fn emit_doctor_result(output: &DoctorOutput, json: bool) -> Result<()> {
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

fn doctor_json_value(output: &DoctorOutput) -> serde_json::Value {
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
        "card_ids": &output.card_ids,
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

/// Run an isolated submit_intent smoke using an in-memory LANCET runtime.
///
/// Uses an explicit project root so test callers do not depend on mutable process cwd.
/// The submit is for a "read" action on `Cargo.toml` — a file that exists in every
/// workspace/crate directory. Asserts `impact.unknowns` is empty.
///
/// Handles two call contexts:
/// - From a synchronous caller without a tokio runtime (tests, benchmarks): creates a
///   fresh `current_thread` runtime.
/// - From within `#[tokio::main]` (the production CLI path): uses `block_in_place` +
///   `Handle::current().block_on()` to avoid panicking with "nested runtime" errors.
pub(crate) fn run_submit_intent_smoke_for_project_root(project_root: &Path) -> Result<()> {
    let project_root = project_root.to_path_buf();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // Inside an existing tokio runtime — block_in_place prevents nesting panic.
            tokio::task::block_in_place(|| handle.block_on(submit_intent_smoke_async(project_root)))
        }
        Err(_) => {
            // No runtime active — create a lightweight current-thread runtime.
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("tokio runtime for submit_intent smoke")?
                .block_on(submit_intent_smoke_async(project_root))
        }
    }
}

async fn submit_intent_smoke_async(project_root: PathBuf) -> Result<()> {
    let root_str = project_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("project root path is not UTF-8"))?;

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry)
        .map_err(|e| anyhow::anyhow!("register CEP methods: {}", e.message))?;

    // session.envelope derive_intent requires MutateSessionState capability to store the derived envelope.
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.base_envelope =
        BaseExecutionEnvelope::for_workspace_root(root_str, "lancet doctor smoke")
            .with_session_state();
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root_str));

    let (client, server) = CepTransport::pair(64);
    let server_task = tokio::spawn(async move {
        server
            .run(registry, config)
            .await
            .map_err(|e| format!("CEP server: {}", e.message))
            .expect("CEP server");
    });

    let result = client
        .call::<Envelope>(EnvelopeParams::DeriveIntent(SubmitIntentParams {
            action: "read".to_string(),
            target: "Cargo.toml".to_string(),
            params: None,
            supersede: false,
            reason: None,
        }))
        .await
        .map_err(|e| {
            anyhow::anyhow!("session.envelope derive_intent smoke failed: {}", e.message)
        })?;
    let EnvelopeResult::DeriveIntent(result) = result else {
        anyhow::bail!("session.envelope derive_intent smoke returned declare result");
    };

    server_task.abort();

    if !result.impact.unknowns.is_empty() {
        anyhow::bail!(
            "submit_intent smoke: {} unknown(s) in impact analysis",
            result.impact.unknowns.len()
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use lancet_invariants::invariants::types::CardAnchor;
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
        let db_path = dir.join(".lattice").join("cep_audit.db");
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
        let dir = TempDir::new().unwrap();
        let output = collect_status(dir.path()).unwrap();
        assert!(output.no_active_envelope_observed);
        assert!(output.active_envelope.is_none());
        assert!(output.recent_calls.is_empty());
        assert!(output.last_guard_verdict.is_none());
    }

    #[test]
    fn cep_status_seeded_audit_json() {
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
        let invariants_dir = workspace_root.join(".lancet").join("invariants");

        let result = run_doctor_inner_for_project_root(workspace_root, &invariants_dir, false);
        assert!(
            result.is_ok(),
            "doctor should pass on repo root: {:?}",
            result
        );
    }

    #[test]
    fn cep_doctor_reports_invariant_cards() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");
        let invariants_dir = workspace_root.join(".lancet").join("invariants");

        let cards =
            load_cards_from_dir(&invariants_dir).expect("cards should load without parse errors");

        assert!(
            cards.len() >= 2,
            "expected at least 2 invariant cards, got {}",
            cards.len()
        );

        // alpha-wire-freeze card
        let wire_freeze = cards
            .iter()
            .find(|c| c.id == "ALPHA-WIRE-FREEZE")
            .expect("ALPHA-WIRE-FREEZE card must exist");
        assert!(
            wire_freeze.title.contains("wire freeze") || wire_freeze.title.contains("Wire"),
            "wire-freeze title: {}",
            wire_freeze.title
        );
        assert!(
            !wire_freeze.forbidden.is_empty(),
            "wire-freeze card should have forbidden entries"
        );
        assert!(matches!(wire_freeze.anchor, CardAnchor::Glob(_)));

        // no-legacy-public-expansion card
        let legacy_card = cards
            .iter()
            .find(|c| c.id == "NO-LEGACY-EXPANSION")
            .expect("NO-LEGACY-EXPANSION card must exist");
        assert!(
            !legacy_card.forbidden.is_empty(),
            "no-legacy-expansion card should have forbidden entries"
        );
        assert!(matches!(legacy_card.anchor, CardAnchor::Glob(_)));
    }

    #[test]
    fn cep_doctor_submit_intent_unknowns_zero() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/")
            .parent()
            .expect("workspace root");
        let result = run_submit_intent_smoke_for_project_root(workspace_root);
        assert!(
            result.is_ok(),
            "submit_intent smoke should succeed: {:?}",
            result
        );
    }

    #[test]
    fn cep_doctor_fails_on_broken_card() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("bad.toml"), "this is not valid toml {{{").unwrap();

        let result = run_doctor_inner(dir.path(), false);
        assert!(
            result.is_err(),
            "doctor should fail when invariants dir has an unparseable card"
        );
    }

    fn unavailable_probe_for_tests() -> HostProbeReport {
        HostProbeReport::from_entries(lancet_protocol::BoundaryPrimitive::ALL.iter().map(
            |primitive| {
                lancet_native::HostProbeEntry::new(
                    *primitive,
                    lancet_protocol::HostProbeStatus::Unavailable,
                    "test_unavailable",
                )
            },
        ))
    }

    fn mixed_probe_for_tests() -> HostProbeReport {
        HostProbeReport::from_entries([
            lancet_native::HostProbeEntry::new(
                lancet_protocol::BoundaryPrimitive::NetworkNamespace,
                lancet_protocol::HostProbeStatus::Unavailable,
                "netns requires privileges",
            )
            .with_diagnostic("helper.unshare", "/usr/bin/unshare"),
            lancet_native::HostProbeEntry::new(
                lancet_protocol::BoundaryPrimitive::MountNamespace,
                lancet_protocol::HostProbeStatus::Available,
                "mount namespace smoke passed",
            )
            .with_diagnostic("helper.unshare", "/usr/bin/unshare"),
            lancet_native::HostProbeEntry::new(
                lancet_protocol::BoundaryPrimitive::Overlayfs,
                lancet_protocol::HostProbeStatus::Disabled,
                "overlayfs disabled",
            )
            .with_diagnostic("helper.sh", "/usr/bin/sh"),
            lancet_native::HostProbeEntry::new(
                lancet_protocol::BoundaryPrimitive::Landlock,
                lancet_protocol::HostProbeStatus::Unknown,
                "landlock ABI probe inconclusive",
            ),
        ])
    }

    fn seed_command_boundary_audit_db(dir: &Path, method: &str, metadata_json: Option<&str>) {
        let db_path = dir.join(".lattice").join("cep_audit.db");
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
        let output = collect_doctor_output(dir.path(), dir.path(), mixed_probe_for_tests(), None);
        let json = doctor_json_value(&output);
        let host_probe = json["host_probe"]["primitives"]
            .as_object()
            .expect("primitive-keyed host_probe");
        assert_eq!(
            host_probe.len(),
            lancet_protocol::BoundaryPrimitive::ALL.len()
        );
        for primitive in lancet_protocol::BoundaryPrimitive::ALL {
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
        let result = run_doctor_inner_with_probe(
            dir.path(),
            dir.path(),
            false,
            unavailable_probe_for_tests(),
            None,
        );
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
            dir.path(),
            false,
            unavailable_probe_for_tests(),
            Some(DoctorGateInput {
                profile_label: "test-profile".into(),
                required_boundaries: vec![lancet_protocol::BoundaryDimension::Network],
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
        let output = collect_doctor_output(
            dir.path(),
            dir.path(),
            unavailable_probe_for_tests(),
            Some(DoctorGateInput {
                profile_label: "test-profile".into(),
                required_boundaries: vec![lancet_protocol::BoundaryDimension::Network],
            }),
        );
        assert_eq!(output.gate_missing.len(), 1);
        assert_eq!(
            output.gate_missing[0].boundary,
            lancet_protocol::BoundaryDimension::Network
        );
        let details = format_missing_boundary_details(&output.gate_missing[0]);
        assert!(details.contains("network_namespace"), "got: {details}");
        assert!(details.contains("test_unavailable"), "got: {details}");
    }
}
