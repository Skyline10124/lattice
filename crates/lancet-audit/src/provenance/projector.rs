// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Project flat [`AuditRow`] records into PROV-DM fragments.

use crate::audit::{AuditRow, impact_kind_from_label};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::provenance::{
    ProvActivity, ProvAgent, ProvEntity, ProvGraphFragment, ProvUsed, ProvWasAssociatedWith,
    ProvWasGeneratedBy, ProvWasInformedBy, TurnCallSummary, TurnExecutionTrace,
};
use serde_json::Value;
use std::collections::BTreeMap;

const RUNTIME_FALLBACK: &str = "lattice";

/// Project audit rows into a PROV graph fragment (one Activity per row).
pub fn project_rows(rows: &[AuditRow]) -> ProvGraphFragment {
    if rows.is_empty() {
        return ProvGraphFragment::default();
    }

    let mut fragment = ProvGraphFragment::default();
    let agent_id = build_agent(rows, &mut fragment);

    for row in rows {
        let activity_id = activity_id_for(row.audit_id);
        let meta = parse_metadata(row);
        fragment.activities.push(ProvActivity {
            id: activity_id.clone(),
            method: row.method.clone(),
            impact_kind: parse_impact_kind(&row.impact_kind),
            started_at: Some(row.started_at.clone()),
            ended_at: row.ended_at.clone(),
            guard_stage: Some(row.guard_stage.clone()),
            semantic_status: row.semantic_status.clone(),
            guard_verdict: None,
            outcome_summary: row.result_or_error.clone(),
            attributes: activity_attributes_from_metadata(meta.as_ref()),
        });

        if let Some(agent) = &agent_id {
            fragment.was_associated_with.push(ProvWasAssociatedWith {
                activity_id: activity_id.clone(),
                agent_id: agent.clone(),
            });
        }

        if let Some(meta) = meta {
            project_entities_from_metadata(row.audit_id, &row.method, &meta, &mut fragment);
        }
    }

    chain_was_informed_by(rows, &mut fragment);
    fragment
}

/// Build a turn-level execution trace (PROV-04 structural replay).
pub fn project_turn_trace(turn_id: &str, rows: &[AuditRow]) -> TurnExecutionTrace {
    let runtime = rows
        .first()
        .and_then(|r| r.runtime_version.as_deref())
        .filter(|s| !s.is_empty())
        .unwrap_or(RUNTIME_FALLBACK)
        .to_string();

    let calls: Vec<TurnCallSummary> = rows
        .iter()
        .map(|row| {
            let envelope_id = parse_metadata(row).and_then(|m| {
                m.get("envelope_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            });
            TurnCallSummary {
                audit_id: row.audit_id,
                call_id: row.call_id.to_string(),
                method: row.method.clone(),
                impact_kind: parse_impact_kind(&row.impact_kind),
                envelope_id,
                outcome_summary: row.result_or_error.clone(),
            }
        })
        .collect();

    let fragment = project_rows(rows);
    let statement = if rows.is_empty() {
        None
    } else {
        Some(crate::provenance::attestation::build_in_toto_statement(
            turn_id, rows, &runtime,
        ))
    };

    TurnExecutionTrace {
        turn_id: turn_id.to_string(),
        calls,
        fragment,
        statement,
    }
}

fn activity_id_for(audit_id: i64) -> String {
    format!("activity:{audit_id}")
}

fn entity_id_for(kind: &str, digest: &str) -> String {
    format!("entity:{kind}:{digest}")
}

fn build_agent(rows: &[AuditRow], fragment: &mut ProvGraphFragment) -> Option<String> {
    let first = rows.first()?;
    let session_id = first.session_id.clone();
    let model_id = rows
        .iter()
        .find_map(|r| r.model_id.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or("unknown")
        .to_string();
    let runtime = rows
        .iter()
        .find_map(|r| r.runtime_version.as_deref().filter(|s| !s.is_empty()))
        .unwrap_or(RUNTIME_FALLBACK)
        .to_string();
    let id = format!("agent:{session_id}:{model_id}");
    fragment.agents.push(ProvAgent {
        id: id.clone(),
        runtime,
        session_id,
        model_id,
    });
    Some(id)
}

fn parse_metadata(row: &AuditRow) -> Option<Value> {
    row.metadata_json
        .as_ref()
        .and_then(|s| serde_json::from_str(s).ok())
}

fn parse_impact_kind(label: &str) -> ImpactKind {
    impact_kind_from_label(label).unwrap_or_else(|| {
        tracing::warn!(
            impact_kind_label = label,
            "unrecognized audit row impact_kind; PROV projection uses WorkspaceObservation as fallback"
        );
        ImpactKind::WorkspaceObservation
    })
}

fn activity_attributes_from_metadata(meta: Option<&Value>) -> BTreeMap<String, Value> {
    let mut attributes = BTreeMap::new();
    for key in [
        "effective_boundary",
        "cep_policy",
        "cep_preflight",
        "approval_request",
    ] {
        if let Some(value) = meta.and_then(|m| m.get(key)) {
            attributes.insert(key.into(), value.clone());
        }
    }
    attributes
}

fn project_entities_from_metadata(
    audit_id: i64,
    method: &str,
    meta: &Value,
    fragment: &mut ProvGraphFragment,
) {
    let activity_id = activity_id_for(audit_id);
    let is_mutation = matches!(method, "mutate.create" | "mutate.edit" | "mutate.batch");

    if let Some(hash) = meta.get("content_hash").and_then(|v| v.as_str()) {
        let entity_id = push_entity(fragment, "file", hash);
        if is_mutation {
            fragment.was_generated_by.push(ProvWasGeneratedBy {
                entity_id,
                activity_id: activity_id.clone(),
            });
        } else {
            fragment.used.push(ProvUsed {
                activity_id: activity_id.clone(),
                entity_id,
            });
        }
    }

    if let Some(hash) = meta.get("envelope_hash").and_then(|v| v.as_str()) {
        let entity_id = push_entity(fragment, "envelope", hash);
        fragment.used.push(ProvUsed {
            activity_id: activity_id.clone(),
            entity_id: entity_id.clone(),
        });
    }

    if let Some(url) = meta.get("request_url").and_then(|v| v.as_str()) {
        let entity_id = push_entity(fragment, "url", url);
        fragment.used.push(ProvUsed {
            activity_id: activity_id.clone(),
            entity_id,
        });
    }

    if let Some(target) = meta
        .get("canonical_target")
        .or_else(|| meta.get("request_target"))
        .and_then(|v| v.as_str())
    {
        let entity_id = push_entity(fragment, "path", target);
        fragment.used.push(ProvUsed {
            activity_id: activity_id.clone(),
            entity_id,
        });
    }

    if let Some(files) = meta.get("changed_files").and_then(|v| v.as_array()) {
        for file in files {
            let path = file
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let digest = file
                .get("content_hash")
                .and_then(|v| v.as_str())
                .unwrap_or(path);
            let entity_id = push_entity(fragment, "file", digest);
            fragment.was_generated_by.push(ProvWasGeneratedBy {
                entity_id,
                activity_id: activity_id.clone(),
            });
        }
    }
}

fn push_entity(fragment: &mut ProvGraphFragment, kind: &str, digest_or_uri: &str) -> String {
    let id = entity_id_for(kind, digest_or_uri);
    if !fragment.entities.iter().any(|e| e.id == id) {
        fragment.entities.push(ProvEntity {
            id: id.clone(),
            kind: kind.to_string(),
            digest_or_uri: digest_or_uri.to_string(),
        });
    }
    id
}

fn chain_was_informed_by(rows: &[AuditRow], fragment: &mut ProvGraphFragment) {
    let mut by_turn: std::collections::BTreeMap<String, Vec<i64>> =
        std::collections::BTreeMap::new();
    for row in rows {
        by_turn
            .entry(row.turn_id.clone())
            .or_default()
            .push(row.audit_id);
    }
    for ids in by_turn.values_mut() {
        ids.sort_unstable();
        for window in ids.windows(2) {
            fragment.was_informed_by.push(ProvWasInformedBy {
                informed_activity_id: activity_id_for(window[1]),
                informing_activity_id: activity_id_for(window[0]),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditRow, impact_kind_label};

    fn fixture_row(audit_id: i64, method: &str, meta: &str) -> AuditRow {
        AuditRow {
            audit_id,
            call_id: audit_id as u64,
            method: method.into(),
            started_at: "1000".into(),
            ended_at: Some("1001".into()),
            result_or_error: Some("success:ok".into()),
            impact_kind: impact_kind_label(&ImpactKind::WorkspaceObservation).into(),
            turn_id: "turn-a".into(),
            session_id: "sess-1".into(),
            semantic_stage: "cep_native".into(),
            semantic_status: Some("native_handler".into()),
            guard_stage: "cep_policy_lifecycle".into(),
            policy_enforced: true,
            metadata_json: Some(meta.into()),
            model_id: Some("sonnet".into()),
            runtime_version: Some("lattice/1.3".into()),
        }
    }

    #[test]
    fn project_rows_builds_activity_per_row() {
        let rows = vec![
            fixture_row(
                1,
                "observe.workspace",
                r#"{"content_hash":"sha256:aaa","canonical_target":"src/a.rs"}"#,
            ),
            fixture_row(2, "session.todo_write", r#"{"op_count":1}"#),
        ];
        let fragment = project_rows(&rows);
        assert_eq!(fragment.activities.len(), 2);
        assert_eq!(fragment.activities[0].id, "activity:1");
        assert_eq!(fragment.activities[1].id, "activity:2");
        assert!(!fragment.agents.is_empty());
        assert_eq!(fragment.agents[0].model_id, "sonnet");
        assert!(!fragment.was_associated_with.is_empty());
    }

    #[test]
    fn parse_impact_kind_warns_on_unknown_label() {
        let rows = vec![AuditRow {
            impact_kind: "future_impact_kind".into(),
            ..fixture_row(1, "observe.workspace", r#"{}"#)
        }];
        let fragment = project_rows(&rows);
        assert_eq!(
            fragment.activities[0].impact_kind,
            ImpactKind::WorkspaceObservation
        );
    }

    #[test]
    fn project_turn_trace_orders_calls() {
        let rows = vec![
            fixture_row(1, "observe.workspace", r#"{}"#),
            fixture_row(2, "observe.workspace", r#"{}"#),
        ];
        let trace = project_turn_trace("turn-a", &rows);
        assert_eq!(trace.turn_id, "turn-a");
        assert_eq!(trace.calls.len(), 2);
        assert_eq!(trace.calls[0].audit_id, 1);
        assert!(trace.statement.is_some());
    }

    #[test]
    fn boundary_observed_projection() {
        let rows = vec![fixture_row(
            1,
            "exec.command_run",
            r#"{
                "effective_boundary": {
                    "network": {
                        "boundary": "network",
                        "status": "physically_bounded",
                        "observed": false
                    },
                    "filesystem": {
                        "boundary": "filesystem",
                        "status": "physically_bounded",
                        "observed": true
                    }
                }
            }"#,
        )];

        let fragment = project_rows(&rows);
        let effective_boundary = &fragment.activities[0].attributes["effective_boundary"];

        assert_eq!(
            effective_boundary["network"]["observed"],
            serde_json::json!(false)
        );
        assert_eq!(
            effective_boundary["filesystem"]["observed"],
            serde_json::json!(true)
        );
        assert!(
            fragment.entities.is_empty(),
            "boundary metadata must remain on the per-call activity"
        );
    }

    #[test]
    fn preflight_and_policy_metadata_project_to_activity_attributes() {
        let rows = vec![fixture_row(
            1,
            "mutate.edit",
            r#"{
                "cep_policy": {
                    "decision": "allowed",
                    "capability_match": true
                },
                "cep_preflight": {
                    "kind": "workspace_mutation",
                    "expected_hash_status": "stale"
                },
                "approval_request": {
                    "status": "required"
                }
            }"#,
        )];

        let fragment = project_rows(&rows);
        let attrs = &fragment.activities[0].attributes;

        assert_eq!(
            attrs["cep_policy"]["decision"],
            serde_json::json!("allowed")
        );
        assert_eq!(
            attrs["cep_preflight"]["expected_hash_status"],
            serde_json::json!("stale")
        );
        assert_eq!(
            attrs["approval_request"]["status"],
            serde_json::json!("required")
        );
    }
}
