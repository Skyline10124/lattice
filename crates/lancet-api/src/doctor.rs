// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET doctor API for frontends.

use std::path::{Path, PathBuf};

use lancet_catalog::catalog::catalog;
use lancet_native::{
    MethodRegistry, MissingRequiredBoundary, probe_host_capabilities,
    register_production_method_registry, unavailable_required_boundaries,
};
use lancet_protocol::{BoundaryDimension, HostProbeEntry, HostProbeReport, HostProbeStatus};

/// Request shape for LANCET doctor checks.
#[derive(Clone, Debug)]
pub struct LancetDoctorRequest {
    pub project_root: PathBuf,
    pub host_probe: Option<HostProbeReport>,
    pub gate: Option<LancetDoctorGate>,
}

impl LancetDoctorRequest {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            host_probe: None,
            gate: None,
        }
    }
}

/// Optional profile-derived boundary gate.
#[derive(Clone, Debug)]
pub struct LancetDoctorGate {
    pub profile_label: String,
    pub required_boundaries: Vec<BoundaryDimension>,
}

/// Structured doctor result for CLI/TUI/GUI frontends.
#[derive(Clone, Debug)]
pub struct LancetDoctorOutput {
    pub ok_items: Vec<String>,
    pub warnings: Vec<String>,
    pub issues: Vec<String>,
    pub host_probe: HostProbeReport,
    pub gate_profile: Option<String>,
    pub gate_missing: Vec<MissingRequiredBoundary>,
}

/// Run LANCET doctor checks and return structured frontend data.
pub fn run_doctor_checks(request: LancetDoctorRequest) -> LancetDoctorOutput {
    let host_probe = request.host_probe.unwrap_or_else(probe_host_capabilities);
    collect_doctor_output(&request.project_root, host_probe, request.gate)
}

fn collect_doctor_output(
    _project_root: &Path,
    host_probe: HostProbeReport,
    gate: Option<LancetDoctorGate>,
) -> LancetDoctorOutput {
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

    let catalog_len = catalog().len();
    if catalog_len == 9 {
        ok_items.push(format!("catalog: {catalog_len} methods (expected 9)"));
    } else {
        issues.push(format!("catalog: expected 9 methods, got {catalog_len}"));
    }

    {
        let mut registry = MethodRegistry::new();
        match register_production_method_registry(&mut registry) {
            Ok(()) => ok_items.push("registry: production methods registered".to_string()),
            Err(err) => issues.push(format!("registry: {}", err.message)),
        }
    }

    for entry in host_probe.entries() {
        match entry.status {
            HostProbeStatus::Available => {
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

    LancetDoctorOutput {
        ok_items,
        warnings,
        issues,
        host_probe,
        gate_profile,
        gate_missing,
    }
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

fn host_probe_status_label(status: HostProbeStatus) -> &'static str {
    match status {
        HostProbeStatus::Available => "available",
        HostProbeStatus::Unavailable => "unavailable",
        HostProbeStatus::Disabled => "disabled",
        HostProbeStatus::Unknown => "unknown",
    }
}

fn host_probe_reason(entry: &HostProbeEntry) -> &str {
    entry.reason.as_deref().unwrap_or("no_reason")
}
