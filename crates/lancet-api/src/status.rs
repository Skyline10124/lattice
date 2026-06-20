// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public LANCET status API for frontends.

use std::path::Path;

use lancet_audit::{AuditQueryFilters, CepAuditStore};
use lancet_authority::allowed_primitives_for;
use lancet_native::probe_host_capabilities;
use lancet_protocol::{
    BoundaryDimension, BoundaryPrimitive, EffectiveBoundaryStatus, HostProbeReport,
};
use serde_json::Value;

use crate::config::audit_db_path_for_project;
use crate::error::{LancetError, LancetResult};

/// Audit-derived LANCET status plus the current host primitive probe.
#[derive(Clone, Debug)]
pub struct LancetStatusOutput {
    pub host_probe: HostProbeReport,
    pub active_envelope: Option<Value>,
    pub no_active_envelope_observed: bool,
    pub recent_calls: Vec<LancetRecentCallEntry>,
    pub last_guard_verdict: Option<String>,
}

/// One recent CEP call for frontend status rendering.
#[derive(Clone, Debug)]
pub struct LancetRecentCallEntry {
    pub audit_id: i64,
    pub method: String,
    pub impact: String,
    pub verdict: Option<String>,
    pub duration_ms: Option<i64>,
    pub effective_boundary: Option<serde_json::Map<String, Value>>,
    pub boundary_setup_failed_closed: Option<bool>,
}

/// Build status data from the audit store without spawning a CEP server.
pub fn collect_status(project_root: &Path) -> LancetResult<LancetStatusOutput> {
    collect_status_with_probe(project_root, probe_host_capabilities())
}

/// Build status data with a caller-supplied host probe.
///
/// This is useful for frontends/tests that need deterministic host capability
/// reporting while still using the same status parser as production.
pub fn collect_status_with_probe(
    project_root: &Path,
    host_probe: HostProbeReport,
) -> LancetResult<LancetStatusOutput> {
    let db_path = audit_db_path_for_project(project_root)
        .map_err(|err| LancetError::Config(format!("resolve external audit path: {err}")))?;

    if !db_path.exists() {
        return Ok(LancetStatusOutput {
            host_probe,
            active_envelope: None,
            no_active_envelope_observed: true,
            recent_calls: vec![],
            last_guard_verdict: None,
        });
    }

    let store = CepAuditStore::open(&db_path)
        .map_err(|err| LancetError::Internal(format!("open audit store: {err}")))?;

    let declare_rows = store
        .query_rows(&AuditQueryFilters {
            method: Some("session.envelope".to_string()),
            ..Default::default()
        })
        .map_err(|err| LancetError::Internal(format!("query session.envelope rows: {err}")))?;

    let active_envelope = declare_rows.last().and_then(|row| {
        row.metadata_json
            .as_deref()
            .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
    });
    let no_active_envelope_observed = active_envelope.is_none();

    let all_rows = store
        .query_rows(&AuditQueryFilters::default())
        .map_err(|err| LancetError::Internal(format!("query all rows: {err}")))?;

    let last_guard_verdict = all_rows.last().map(|row| {
        row.semantic_status
            .clone()
            .unwrap_or_else(|| row.guard_stage.clone())
    });

    let recent_start = all_rows.len().saturating_sub(20);
    let recent_calls = all_rows[recent_start..]
        .iter()
        .rev()
        .map(|row| LancetRecentCallEntry {
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

    Ok(LancetStatusOutput {
        host_probe,
        active_envelope,
        no_active_envelope_observed,
        recent_calls,
        last_guard_verdict,
    })
}

fn compute_duration_ms(started_at: &str, ended_at: Option<&str>) -> Option<i64> {
    let ended = ended_at?;
    let start = started_at.parse::<i64>().ok()?;
    let end = ended.parse::<i64>().ok()?;
    Some(end.saturating_sub(start))
}

fn parse_effective_boundary_metadata(
    method: &str,
    metadata_json: Option<&str>,
) -> Option<serde_json::Map<String, Value>> {
    if method != "exec.command_run" {
        return None;
    }
    let metadata_json = metadata_json?;
    let metadata = match serde_json::from_str::<Value>(metadata_json) {
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
    let metadata = serde_json::from_str::<Value>(metadata_json?).ok()?;
    metadata
        .get("boundary_setup_failed_closed")
        .and_then(|value| value.as_bool())
}

fn normalize_effective_boundary_entry(boundary: BoundaryDimension, entry: &Value) -> Value {
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
    if status == "physically_bounded"
        && let Err(reason) = validate_physically_bounded_attribution(boundary, object)
    {
        return unknown_effective_boundary_entry(boundary, reason);
    }
    let mut normalized = object.clone();
    normalized
        .entry("boundary".to_string())
        .or_insert_with(|| Value::String(boundary.label().to_string()));
    if !normalized.get("observed").is_some_and(Value::is_boolean) {
        normalized.insert("observed".to_string(), Value::Bool(false));
    }
    Value::Object(normalized)
}

fn validate_physically_bounded_attribution(
    boundary: BoundaryDimension,
    object: &serde_json::Map<String, Value>,
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

fn unknown_effective_boundary(reason: String) -> serde_json::Map<String, Value> {
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

fn unknown_effective_boundary_entry(boundary: BoundaryDimension, reason: String) -> Value {
    serde_json::json!({
        "boundary": boundary,
        "status": EffectiveBoundaryStatus::Unknown,
        "observed": false,
        "reason": reason,
    })
}
