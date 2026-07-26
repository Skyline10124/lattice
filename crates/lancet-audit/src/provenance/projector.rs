// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Project flat [`AuditRow`] records into PROV-DM fragments.

use crate::audit::{AuditRow, SEMANTIC_STAGE_COMPONENT, impact_kind_from_label};
use lancet_protocol::effect_types::ImpactKind;
use lancet_protocol::provenance::{
    ProvActivity, ProvAgent, ProvEntity, ProvGraphFragment, ProvUsed, ProvWasAssociatedWith,
    ProvWasGeneratedBy, ProvWasInformedBy, TurnCallSummary, TurnExecutionTrace,
};
use serde_json::Value;
use std::collections::BTreeMap;

const RUNTIME_FALLBACK: &str = "lattice";

/// Activity attribute that marks [`ProvActivity::impact_kind`] as a placeholder
/// rather than an attribution (#28 B6). A Cell-governed Component row has no
/// legacy `ImpactKind` at all — its authorization vocabulary is the Cell — but
/// the PROV wire type makes the field mandatory, so the projection states the
/// fact instead of letting the fallback read as a real impact claim. Dropping
/// the field is gated on the legacy deletion (design §11).
const IMPACT_KIND_ATTRIBUTED: &str = "impact_kind_attributed";

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
        let (impact_kind, attributed) = impact_kind_for_row(row);
        let mut attributes = activity_attributes_from_metadata(meta.as_ref());
        if !attributed {
            attributes.insert(IMPACT_KIND_ATTRIBUTED.into(), Value::Bool(false));
        }
        fragment.activities.push(ProvActivity {
            id: activity_id.clone(),
            method: row.method.clone(),
            impact_kind,
            started_at: Some(row.started_at.clone()),
            ended_at: row.ended_at.clone(),
            guard_stage: Some(row.guard_stage.clone()),
            semantic_status: row.semantic_status.clone(),
            guard_verdict: None,
            outcome_summary: row.result_or_error.clone(),
            attributes,
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

/// Build a turn-level execution trace (structural replay).
pub fn project_turn_trace(turn_id: &str, rows: &[AuditRow]) -> TurnExecutionTrace {
    let runtime = rows
        .first()
        .and_then(|r| r.runtime_version.as_deref())
        .filter(|s| !s.is_empty())
        .unwrap_or(RUNTIME_FALLBACK)
        .to_string();

    let calls: Vec<TurnCallSummary> = rows
        .iter()
        .map(|row| TurnCallSummary {
            audit_id: row.audit_id,
            call_id: row.call_id.to_string(),
            method: row.method.clone(),
            impact_kind: parse_impact_kind(&row.impact_kind),
            outcome_summary: row.result_or_error.clone(),
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

/// A row's legacy impact kind and whether it is a real attribution.
///
/// A Cell-governed Component row never carried one, so it is not run through
/// the label parser at all: coercing its `unknown` column into a concrete
/// legacy kind would attribute an impact the row never claimed. The placeholder
/// travels with [`IMPACT_KIND_ATTRIBUTED`]` = false`.
fn impact_kind_for_row(row: &AuditRow) -> (ImpactKind, bool) {
    if row.semantic_stage == SEMANTIC_STAGE_COMPONENT {
        (ImpactKind::WorkspaceObservation, false)
    } else {
        (parse_impact_kind(&row.impact_kind), true)
    }
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
        // #28 B6: "why authorized" and "what happened" reach the PROV
        // fragment as separate activity attributes.
        "admission_evidence",
        "observed_effects",
        "denied_operations",
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
    let is_mutation = matches!(method, "mutate.create" | "mutate.edit");

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

    /// #28 b8: a Cell-governed row's `impact_kind` column is `unknown` because
    /// the row never had a legacy impact kind. Projecting it as a concrete
    /// `ImpactKind` without saying so attributes an impact the call never made.
    #[test]
    fn cell_governed_row_marks_its_impact_kind_as_unattributed() {
        let component = AuditRow {
            impact_kind: "unknown".into(),
            semantic_stage: crate::audit::SEMANTIC_STAGE_COMPONENT.into(),
            semantic_status: Some("component_handler".into()),
            guard_stage: crate::audit::GUARD_STAGE_COMPONENT.into(),
            ..fixture_row(1, "noop.echo", r#"{}"#)
        };
        let native = fixture_row(2, "observe.workspace", r#"{}"#);
        let fragment = project_rows(&[component, native]);

        assert_eq!(
            fragment.activities[0].attributes["impact_kind_attributed"],
            serde_json::json!(false)
        );
        assert!(
            !fragment.activities[1]
                .attributes
                .contains_key("impact_kind_attributed"),
            "a native row's impact kind is a real attribution and stays unmarked"
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
            "observe.workspace",
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

    /// Pins the activity-attribute whitelist: exactly these metadata keys
    /// reach the PROV fragment, and nothing else does.
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
                    "local_revision": "blake3:abc"
                },
                "approval_request": {
                    "status": "required"
                },
                "effective_boundary": {},
                "admission_evidence": {
                    "version": 1,
                    "tool": "noop.echo",
                    "approved_capabilities": ["session.events"]
                },
                "observed_effects": {
                    "version": 1,
                    "effects": [{"seq": 0, "source": "operation"}]
                },
                "denied_operations": [{"seq": 1, "reason": "uncovered operation"}],
                "component_cells": ["mutate:none:session:in"]
            }"#,
        )];

        let fragment = project_rows(&rows);
        let attrs = &fragment.activities[0].attributes;

        assert_eq!(
            attrs["cep_policy"]["decision"],
            serde_json::json!("allowed")
        );
        assert_eq!(
            attrs["cep_preflight"]["local_revision"],
            serde_json::json!("blake3:abc")
        );
        assert_eq!(
            attrs["approval_request"]["status"],
            serde_json::json!("required")
        );
        // B6: "why authorized" and "what happened" project as separate
        // attributes, never folded into one another.
        assert_eq!(
            attrs["admission_evidence"]["approved_capabilities"],
            serde_json::json!(["session.events"])
        );
        assert_eq!(
            attrs["observed_effects"]["effects"][0]["source"],
            serde_json::json!("operation")
        );
        assert_eq!(
            attrs["denied_operations"][0]["reason"],
            serde_json::json!("uncovered operation")
        );

        assert_eq!(
            attrs.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "admission_evidence",
                "approval_request",
                "cep_policy",
                "cep_preflight",
                "denied_operations",
                "effective_boundary",
                "observed_effects",
            ],
            "activity attributes are whitelisted; component_cells stays a query key"
        );
    }
}
