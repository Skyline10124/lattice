// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET doctor API for frontends.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lancet_authority::CepTurnContext;
use lancet_catalog::catalog::catalog;
use lancet_catalog::methods::envelope::{DeclareParams, Envelope, EnvelopeParams, EnvelopeResult};
use lancet_native::{
    MethodRegistry, MissingRequiredBoundary, ServerRunConfig, probe_host_capabilities,
    register_production_method_registry, unavailable_required_boundaries,
};
use lancet_protocol::envelope::{
    BaseExecutionEnvelope, Capability, Constraints, ExecutionEnvelope, Impact, ImpactKind,
    ImpactScope, Intent, Scope,
};
use lancet_protocol::{BoundaryDimension, HostProbeEntry, HostProbeReport, HostProbeStatus};

use crate::error::{LancetError, LancetResult};
use crate::runtime::LancetServer;

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
    project_root: &Path,
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
    if catalog_len == 10 {
        ok_items.push(format!("catalog: {catalog_len} methods (expected 10)"));
    } else {
        issues.push(format!("catalog: expected 10 methods, got {catalog_len}"));
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

    match run_envelope_declare_smoke_for_project_root(project_root) {
        Ok(()) => ok_items.push("envelope declare smoke: ok".to_string()),
        Err(err) => issues.push(format!("envelope declare smoke: {err}")),
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

/// Run an isolated explicit envelope declaration smoke using an in-memory LANCET runtime.
pub fn run_envelope_declare_smoke_for_project_root(project_root: &Path) -> LancetResult<()> {
    let project_root = project_root.to_path_buf();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| {
            handle.block_on(envelope_declare_smoke_async(project_root))
        }),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|err| {
                LancetError::Internal(format!("tokio runtime for envelope declare smoke: {err}"))
            })?
            .block_on(envelope_declare_smoke_async(project_root)),
    }
}

async fn envelope_declare_smoke_async(project_root: PathBuf) -> LancetResult<()> {
    let root_str = project_root
        .to_str()
        .ok_or_else(|| LancetError::Config("project root path is not UTF-8".to_string()))?;

    let mut config = ServerRunConfig::in_memory_for_tests();
    config.base_envelope =
        BaseExecutionEnvelope::for_workspace_root(root_str, "lancet doctor smoke")
            .with_session_state();
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root_str));

    let (client, mut lifecycle) = LancetServer::spawn(config, 64)?;

    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "lancet doctor explicit envelope smoke".to_string(),
        },
        capabilities: [Capability::ReadPath {
            path_pattern: "Cargo.toml".to_string(),
        }]
        .into_iter()
        .collect(),
        scope: Scope {
            workspace_root: Some("Cargo.toml".to_string()),
            extensions: serde_json::Map::new(),
        },
        impact: vec![Impact {
            kind: ImpactKind::WorkspaceObservation,
            target: "Cargo.toml".to_string(),
            scope: Some(ImpactScope::ExactFile(PathBuf::from("Cargo.toml"))),
            confidence: Default::default(),
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: None,
        extensions: serde_json::Map::new(),
    };

    let result = client
        .call::<Envelope>(EnvelopeParams::Declare(DeclareParams {
            envelope,
            supersede: false,
            reason: Some("lancet doctor smoke".to_string()),
        }))
        .await
        .map_err(|err| {
            LancetError::Internal(format!(
                "session.envelope declare smoke failed: {}",
                err.message
            ))
        })?;
    let EnvelopeResult::Declare(result) = result;
    if !result.accepted {
        lifecycle.shutdown().await;
        return Err(LancetError::Internal(
            "session.envelope declare smoke was not accepted".to_string(),
        ));
    };

    lifecycle.shutdown().await;

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
