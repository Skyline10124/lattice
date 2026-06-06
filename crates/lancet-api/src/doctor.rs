// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET doctor API for frontends.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lancet_authority::CepTurnContext;
use lancet_catalog::catalog::catalog;
use lancet_catalog::methods::envelope::{
    Envelope, EnvelopeParams, EnvelopeResult, SubmitIntentParams,
};
use lancet_invariants::invariants::loader::load_cards_from_dir;
use lancet_invariants::resolve_invariants_dir;
use lancet_native::{
    probe_host_capabilities, register_production_method_registry, unavailable_required_boundaries,
    MethodRegistry, MissingRequiredBoundary, ServerRunConfig,
};
use lancet_protocol::envelope::BaseExecutionEnvelope;
use lancet_protocol::{BoundaryDimension, HostProbeEntry, HostProbeReport, HostProbeStatus};

use crate::error::{LancetError, LancetResult};
use crate::runtime::LancetServer;

/// Request shape for LANCET doctor checks.
#[derive(Clone, Debug)]
pub struct LancetDoctorRequest {
    pub project_root: PathBuf,
    pub invariants_dir: Option<PathBuf>,
    pub host_probe: Option<HostProbeReport>,
    pub gate: Option<LancetDoctorGate>,
}

impl LancetDoctorRequest {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            invariants_dir: None,
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
    pub card_ids: Vec<String>,
    pub host_probe: HostProbeReport,
    pub gate_profile: Option<String>,
    pub gate_missing: Vec<MissingRequiredBoundary>,
}

/// Run LANCET doctor checks and return structured frontend data.
pub fn run_doctor_checks(request: LancetDoctorRequest) -> LancetDoctorOutput {
    let invariants_dir = request
        .invariants_dir
        .unwrap_or_else(|| resolve_invariants_dir(&request.project_root, None));
    let host_probe = request.host_probe.unwrap_or_else(probe_host_capabilities);
    collect_doctor_output(
        &request.project_root,
        &invariants_dir,
        host_probe,
        request.gate,
    )
}

fn collect_doctor_output(
    project_root: &Path,
    invariants_dir: &Path,
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

    let card_ids: Vec<String> = match load_cards_from_dir(invariants_dir) {
        Ok(cards) if cards.is_empty() => {
            ok_items.push("invariants: ok_empty (no cards)".to_string());
            vec![]
        }
        Ok(cards) => {
            let ids: Vec<String> = cards.iter().map(|card| card.id.clone()).collect();
            ok_items.push(format!(
                "invariants: ok_loaded ({} card(s): {})",
                ids.len(),
                ids.join(", ")
            ));
            ids
        }
        Err(err) => {
            issues.push(format!("invariants: parse error: {err}"));
            return LancetDoctorOutput {
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

    match run_submit_intent_smoke_for_project_root(project_root) {
        Ok(()) => ok_items.push("submit_intent smoke: ok (unknowns == 0)".to_string()),
        Err(err) => issues.push(format!("submit_intent smoke: {err}")),
    }

    LancetDoctorOutput {
        ok_items,
        warnings,
        issues,
        card_ids,
        host_probe,
        gate_profile,
        gate_missing,
    }
}

/// Run an isolated submit_intent smoke using an in-memory LANCET runtime.
pub fn run_submit_intent_smoke_for_project_root(project_root: &Path) -> LancetResult<()> {
    let project_root = project_root.to_path_buf();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            tokio::task::block_in_place(|| handle.block_on(submit_intent_smoke_async(project_root)))
        }
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| {
                LancetError::Internal(format!("tokio runtime for submit_intent smoke: {err}"))
            })?
            .block_on(submit_intent_smoke_async(project_root)),
    }
}

async fn submit_intent_smoke_async(project_root: PathBuf) -> LancetResult<()> {
    let root_str = project_root
        .to_str()
        .ok_or_else(|| LancetError::Config("project root path is not UTF-8".to_string()))?;

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.base_envelope =
        BaseExecutionEnvelope::for_workspace_root(root_str, "lancet doctor smoke")
            .with_session_state();
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root_str));

    let (client, mut lifecycle) = LancetServer::spawn(config, 64)?;

    let result = client
        .call::<Envelope>(EnvelopeParams::DeriveIntent(SubmitIntentParams {
            action: "read".to_string(),
            target: "Cargo.toml".to_string(),
            params: None,
            supersede: false,
            reason: None,
        }))
        .await
        .map_err(|err| {
            LancetError::Internal(format!(
                "session.envelope derive_intent smoke failed: {}",
                err.message
            ))
        })?;
    let EnvelopeResult::DeriveIntent(result) = result else {
        lifecycle.shutdown().await;
        return Err(LancetError::Internal(
            "session.envelope derive_intent smoke returned declare result".to_string(),
        ));
    };

    lifecycle.shutdown().await;

    if !result.impact.unknowns.is_empty() {
        return Err(LancetError::Internal(format!(
            "{} unknown(s) in impact analysis",
            result.impact.unknowns.len()
        )));
    }

    Ok(())
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
