// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Unsigned in-toto Statement builder from audit rows.

use crate::audit::AuditRow;
use lancet_protocol::canonical_json;
use lancet_protocol::provenance::{
    CEP_TURN_ATTESTATION_PREDICATE_TYPE, CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2, InTotoStatement,
    InTotoSubjectDescriptor, TurnAttestationPredicate,
};
use serde_json::{Value, json};

/// Statement shape for a turn (#28 D8). v1 is the shape every stored row was
/// written with; v2 adds per-call evidence entries that keep "why authorized"
/// apart from "what happened".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttestationVersion {
    V1,
    V2,
}

impl AttestationVersion {
    pub fn predicate_type(self) -> &'static str {
        match self {
            Self::V1 => CEP_TURN_ATTESTATION_PREDICATE_TYPE,
            Self::V2 => CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2,
        }
    }

    pub fn from_predicate_type(predicate_type: &str) -> Option<Self> {
        match predicate_type {
            CEP_TURN_ATTESTATION_PREDICATE_TYPE => Some(Self::V1),
            CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2 => Some(Self::V2),
            _ => None,
        }
    }
}

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

/// Build an unsigned in-toto Statement for a newly attested turn.
///
/// The version is chosen per turn (D8): v2 when any substantive row carries
/// versioned #28 evidence metadata, else v1 — so a turn made only of native
/// calls keeps emitting v1 until its path migrates.
pub fn build_in_toto_statement(
    turn_id: &str,
    rows: &[AuditRow],
    runtime_version: &str,
) -> InTotoStatement {
    build_statement(turn_id, rows, runtime_version, version_from_rows(rows))
}

/// Rebuild the statement of an already-attested turn for replay comparison.
///
/// The stored `predicate_type` is the authority: a turn attested as v1
/// re-projects as v1 even once its rows carry evidence, so stored bytes are
/// never re-interpreted under a newer shape (D8). An unrecognized stored type
/// falls back to row-evidence selection.
pub fn rebuild_in_toto_statement(
    turn_id: &str,
    rows: &[AuditRow],
    runtime_version: &str,
    stored_predicate_type: Option<&str>,
) -> InTotoStatement {
    let version = match stored_predicate_type {
        Some(stored) => AttestationVersion::from_predicate_type(stored).unwrap_or_else(|| {
            tracing::warn!(
                stored_predicate_type = stored,
                "unrecognized stored attestation predicate type; replay falls back to \
                 row-evidence version selection"
            );
            version_from_rows(rows)
        }),
        None => version_from_rows(rows),
    };
    build_statement(turn_id, rows, runtime_version, version)
}

/// Persisted bytes for a statement.
///
/// v2 statements are canonical JSON so stored and re-projected bytes compare
/// byte for byte (D8/B6). v1 statements keep serde field order: their stored
/// bytes are the authority and must never be re-serialized canonically.
pub fn statement_bytes(statement: &InTotoStatement) -> Result<String, serde_json::Error> {
    if statement.predicate_type == CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2 {
        Ok(canonical_json(&serde_json::to_value(statement)?))
    } else {
        serde_json::to_string(statement)
    }
}

fn build_statement(
    turn_id: &str,
    rows: &[AuditRow],
    runtime_version: &str,
    version: AttestationVersion,
) -> InTotoStatement {
    let session_id = rows
        .first()
        .map(|r| r.session_id.clone())
        .unwrap_or_default();

    let mut subject_digests = serde_json::Map::new();
    subject_digests.insert("audit_range".into(), json!(source_audit_range_hash(rows)));

    let substantive = substantive_turn_rows(rows);

    let mut materials = Vec::new();
    let mut evidence = Vec::new();
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
            // v1 materials stay exactly as they were; a mixed turn adds
            // evidence entries only for the calls that produced them.
            if version == AttestationVersion::V2
                && let Some(entry) = evidence_entry(row.audit_id, &meta)
            {
                evidence.push(entry);
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

    let mut predicate_value = serde_json::to_value(&predicate)
        .unwrap_or_else(|_| json!({ "turn_id": turn_id, "runtime_version": runtime_version }));

    if version == AttestationVersion::V2
        && let Some(obj) = predicate_value.as_object_mut()
    {
        obj.insert("evidence".into(), Value::Array(evidence));
    }

    InTotoStatement {
        subject: vec![InTotoSubjectDescriptor {
            name: format!("turn:{turn_id}"),
            digest: Some(json!(subject_digests)),
        }],
        predicate_type: version.predicate_type().to_string(),
        predicate: predicate_value,
        _type: Some("https://in-toto.io/Statement/v1".into()),
        signatures: None,
    }
}

/// v2 iff some substantive row carries versioned #28 evidence metadata.
fn version_from_rows(rows: &[AuditRow]) -> AttestationVersion {
    let carries_evidence = substantive_turn_rows(rows).iter().any(|row| {
        row.metadata_json
            .as_ref()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
            .is_some_and(|meta| versioned_evidence(&meta, "admission_evidence").is_some())
    });
    if carries_evidence {
        AttestationVersion::V2
    } else {
        AttestationVersion::V1
    }
}

/// One call's evidence as a v2 predicate entry: admission ("why") stays a
/// field of its own beside the observed effects and denials ("what").
fn evidence_entry(audit_id: i64, meta: &Value) -> Option<Value> {
    let admission = versioned_evidence(meta, "admission_evidence")?;
    let mut entry = serde_json::Map::new();
    entry.insert("audit_id".into(), json!(audit_id));
    entry.insert("admission".into(), admission.clone());
    if let Some(effects) = versioned_evidence(meta, "observed_effects") {
        entry.insert("observed_effects".into(), effects.clone());
    }
    if let Some(denied) = meta.get("denied_operations").filter(|v| v.is_array()) {
        entry.insert("denied_operations".into(), denied.clone());
    }
    Some(Value::Object(entry))
}

/// A persisted evidence record is only interpretable once its `version` is
/// present (protocol persisted-root convention).
fn versioned_evidence<'a>(meta: &'a Value, key: &str) -> Option<&'a Value> {
    meta.get(key)
        .filter(|value| value.get("version").and_then(Value::as_u64).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{AuditRow, impact_kind_label};
    use lancet_protocol::effect_types::ImpactKind;

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

    /// Durable metadata of one Component Tool call, as
    /// `merge_finalize_metadata` writes it.
    const COMPONENT_META: &str = r#"{
        "admission_evidence": {
            "version": 1,
            "tool": "noop.echo",
            "approved_capabilities": ["session.events", "session.result"],
            "denied_capabilities": [],
            "boundary_excluded": [],
            "max_boundary": "in"
        },
        "observed_effects": {
            "version": 1,
            "effects": [{
                "seq": 0,
                "cell": {"ctx_op": "mutate", "ext_impact": "none", "scope": "session"},
                "scope": {"scope": "session", "trust": "in", "channel": "session"},
                "target": {"kind": "session", "turn_id": "turn-x"},
                "source": "operation"
            }]
        },
        "denied_operations": [{
            "seq": 1,
            "cell": {"ctx_op": "mutate", "ext_impact": "none", "scope": "session"},
            "scope": {"scope": "session", "trust": "in", "channel": "session"},
            "target": {"kind": "none"},
            "reason": "uncovered operation"
        }],
        "component_cells": ["mutate:none:session:in"]
    }"#;

    #[test]
    fn mixed_turn_emits_v2_with_evidence_only_for_component_calls() {
        let rows = vec![
            row(
                1,
                "observe.workspace",
                r#"{"required_capability":"inspect"}"#,
            ),
            row(2, "noop.echo", COMPONENT_META),
        ];
        let stmt = build_in_toto_statement("turn-x", &rows, "lattice/1");

        assert_eq!(stmt.predicate_type, CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2);
        assert_eq!(
            stmt.predicate["materials"].as_array().unwrap().len(),
            2,
            "legacy materials stay one per substantive row"
        );

        let evidence = stmt.predicate["evidence"].as_array().unwrap();
        assert_eq!(
            evidence.len(),
            1,
            "only the component call produced evidence"
        );
        assert_eq!(evidence[0]["audit_id"], json!(2));
        assert_eq!(
            evidence[0]["admission"]["approved_capabilities"],
            json!(["session.events", "session.result"])
        );
        assert_eq!(
            evidence[0]["observed_effects"]["effects"][0]["cell"]["ctx_op"],
            json!("mutate")
        );
        assert_eq!(
            evidence[0]["denied_operations"][0]["reason"],
            json!("uncovered operation")
        );
        assert!(
            evidence[0]["admission"].get("effects").is_none(),
            "'why authorized' must stay a field of its own"
        );
    }

    #[test]
    fn unversioned_evidence_metadata_does_not_select_v2() {
        // `version` gates interpretation; an unversioned key is not evidence.
        let rows = vec![row(
            1,
            "noop.echo",
            r#"{"admission_evidence":{"tool":"n"}}"#,
        )];
        assert_eq!(
            build_in_toto_statement("turn-x", &rows, "lattice/1").predicate_type,
            CEP_TURN_ATTESTATION_PREDICATE_TYPE
        );
    }

    #[test]
    fn stored_predicate_type_wins_on_replay() {
        let rows = vec![row(1, "noop.echo", COMPONENT_META)];
        let replayed = rebuild_in_toto_statement(
            "turn-x",
            &rows,
            "lattice/1",
            Some(CEP_TURN_ATTESTATION_PREDICATE_TYPE),
        );
        assert_eq!(replayed.predicate_type, CEP_TURN_ATTESTATION_PREDICATE_TYPE);
        assert!(
            replayed.predicate.get("evidence").is_none(),
            "a turn attested as v1 must not gain v2 fields on replay"
        );
        assert_eq!(
            statement_bytes(&replayed).unwrap(),
            serde_json::to_string(&replayed).unwrap(),
            "v1 bytes are never re-serialized canonically"
        );
    }

    #[test]
    fn unknown_stored_predicate_type_falls_back_to_row_evidence() {
        let rows = vec![row(1, "noop.echo", COMPONENT_META)];
        let replayed = rebuild_in_toto_statement(
            "turn-x",
            &rows,
            "lattice/1",
            Some("https://example.invalid/attestation/unknown"),
        );
        assert_eq!(
            replayed.predicate_type,
            CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2
        );
    }

    #[test]
    fn v2_statement_bytes_match_between_store_and_replay() {
        let rows = vec![
            row(
                1,
                "observe.workspace",
                r#"{"required_capability":"inspect"}"#,
            ),
            row(2, "noop.echo", COMPONENT_META),
        ];
        let stored = build_in_toto_statement("turn-x", &rows, "lattice/1");
        let stored_bytes = statement_bytes(&stored).unwrap();

        let replayed =
            rebuild_in_toto_statement("turn-x", &rows, "lattice/1", Some(&stored.predicate_type));
        assert_eq!(statement_bytes(&replayed).unwrap(), stored_bytes);
        assert_eq!(
            stored_bytes,
            canonical_json(&serde_json::to_value(&stored).unwrap()),
            "v2 bytes are the canonical form"
        );
        assert!(
            !stored_bytes.contains('\n'),
            "no pretty-printing on the wire"
        );
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
