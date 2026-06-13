// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Unsigned in-toto Statement builder from audit rows.

use crate::audit::AuditRow;
use lancet_protocol::provenance::{
    CEP_TURN_ATTESTATION_PREDICATE_TYPE, InTotoStatement, InTotoSubjectDescriptor,
    TurnAttestationPredicate,
};
use serde_json::json;

/// Rows that contribute to a turn attestation (excludes meta audit observation calls).
fn substantive_turn_rows(rows: &[AuditRow]) -> Vec<&AuditRow> {
    rows.iter()
        .filter(|r| !is_meta_audit_method(&r.method))
        .collect()
}

fn is_meta_audit_method(method: &str) -> bool {
    method.starts_with("audit.") || method == "observe.audit" || method == "observe.audit_replay"
}

/// Blake3 hex digest over ordered audit ids for `source_audit_range_or_hash`.
pub fn source_audit_range_hash(rows: &[AuditRow]) -> String {
    let substantive = substantive_turn_rows(rows);
    let mut ids: Vec<i64> = substantive.iter().map(|r| r.audit_id).collect();
    ids.sort_unstable();
    let payload: String = ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    blake3::hash(payload.as_bytes()).to_hex().to_string()
}

/// Build an unsigned in-toto Statement for a turn from audit rows alone.
pub fn build_in_toto_statement(
    turn_id: &str,
    rows: &[AuditRow],
    runtime_version: &str,
) -> InTotoStatement {
    let session_id = rows
        .first()
        .map(|r| r.session_id.clone())
        .unwrap_or_default();

    let mut subject_digests = serde_json::Map::new();
    subject_digests.insert("audit_range".into(), json!(source_audit_range_hash(rows)));

    let substantive = substantive_turn_rows(rows);

    let mut materials = Vec::new();
    let mut capabilities = std::collections::BTreeSet::new();
    for row in &substantive {
        let mut material = json!({
            "audit_id": row.audit_id,
            "method": row.method,
            "impact_kind": row.impact_kind,
        });
        if let Some(meta) = row
            .metadata_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        {
            if let Some(cap) = meta.get("required_capability") {
                material["required_capability"] = cap.clone();
                if let Some(s) = cap.as_str() {
                    capabilities.insert(s.to_string());
                }
            }
            if let Some(hash) = meta.get("content_hash") {
                material["content_hash"] = hash.clone();
                subject_digests.insert(
                    format!("file:{}", hash.as_str().unwrap_or("")),
                    hash.clone(),
                );
            }
            if let Some(boundary) = meta.get("effective_boundary") {
                material["effective_boundary"] = boundary.clone();
            }
        }
        materials.push(material);
    }

    let effect_summary = json!({
        "call_count": substantive.len(),
        "impact_kinds": substantive.iter().map(|r| &r.impact_kind).collect::<Vec<_>>(),
    });

    let predicate = TurnAttestationPredicate {
        turn_id: turn_id.to_string(),
        session_id: if session_id.is_empty() {
            None
        } else {
            Some(session_id)
        },
        materials,
        capabilities: capabilities.into_iter().collect(),
        effect_summary: Some(effect_summary),
    };

    let predicate_value = serde_json::to_value(&predicate)
        .unwrap_or_else(|_| json!({ "turn_id": turn_id, "runtime_version": runtime_version }));

    InTotoStatement {
        subject: vec![InTotoSubjectDescriptor {
            name: format!("turn:{turn_id}"),
            digest: Some(json!(subject_digests)),
        }],
        predicate_type: CEP_TURN_ATTESTATION_PREDICATE_TYPE.to_string(),
        predicate: predicate_value,
        _type: Some("https://in-toto.io/Statement/v1".into()),
        signatures: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditRow, impact_kind_label};
    use lancet_protocol::envelope::ImpactKind;

    fn row(audit_id: i64, method: &str, meta: &str) -> AuditRow {
        AuditRow {
            audit_id,
            call_id: audit_id as u64,
            method: method.into(),
            started_at: "1".into(),
            ended_at: Some("2".into()),
            result_or_error: Some("success:ok".into()),
            impact_kind: impact_kind_label(&ImpactKind::WorkspaceObservation).into(),
            turn_id: "turn-x".into(),
            session_id: "sess".into(),
            semantic_stage: "cep_native".into(),
            semantic_status: Some("native_handler".into()),
            guard_stage: "cep_policy_lifecycle".into(),
            policy_enforced: true,
            metadata_json: Some(meta.into()),
            model_id: Some("m".into()),
            runtime_version: Some("lattice/1".into()),
        }
    }

    #[test]
    fn build_statement_lists_materials() {
        let rows = vec![
            row(
                1,
                "observe.workspace",
                r#"{
                    "required_capability": "read_path",
                    "content_hash": "sha256:abc",
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
            ),
            row(
                2,
                "observe.workspace",
                r#"{"required_capability":"inspect"}"#,
            ),
        ];
        let stmt = build_in_toto_statement("turn-x", &rows, "lattice/1");
        assert_eq!(stmt.predicate_type, CEP_TURN_ATTESTATION_PREDICATE_TYPE);
        let materials = stmt.predicate["materials"].as_array().unwrap();
        assert_eq!(materials.len(), 2);
        assert_eq!(
            materials[0]["effective_boundary"]["network"]["observed"],
            serde_json::json!(false)
        );
        assert_eq!(
            materials[0]["effective_boundary"]["filesystem"]["observed"],
            serde_json::json!(true)
        );
        let caps = stmt.predicate["capabilities"].as_array().unwrap();
        assert!(caps.iter().any(|c| c == "read_path"));
        assert!(caps.iter().any(|c| c == "inspect"));
    }

    #[test]
    fn source_audit_range_hash_is_stable() {
        let rows = vec![row(2, "a", "{}"), row(1, "b", "{}")];
        let h1 = source_audit_range_hash(&rows);
        let h2 = source_audit_range_hash(&[rows[1].clone(), rows[0].clone()]);
        assert_eq!(h1, h2);
        assert!(!h1.is_empty());
    }
}
