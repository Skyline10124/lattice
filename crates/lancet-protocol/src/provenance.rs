//! PROV-DM–shaped provenance fragments and unsigned in-toto attestation wire types.
//!
//! Graph structure is projected at query/replay time from flat audit rows — these
//! types are the stable serialization surface (PROV-01 forward-compat: no
//! `deny_unknown_fields` on fragment roots).

use crate::envelope::ImpactKind;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Stable in-toto `predicateType` URI for per-turn CEP attestations (unsigned in alpha).
pub const CEP_TURN_ATTESTATION_PREDICATE_TYPE: &str = "https://lattice.dev/attestation/cep-turn/v1";

/// PROV Activity node — one CEP call / audit row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvActivity {
    pub id: String,
    pub method: String,
    pub impact_kind: ImpactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_summary: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, serde_json::Value>,
}

/// PROV Entity node — digest or URI for an artifact referenced by a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvEntity {
    pub id: String,
    pub kind: String,
    pub digest_or_uri: String,
}

/// PROV Agent node — runtime + session + model triple.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvAgent {
    pub id: String,
    pub runtime: String,
    pub session_id: String,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvUsed {
    pub activity_id: String,
    pub entity_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvWasGeneratedBy {
    pub entity_id: String,
    pub activity_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvWasAssociatedWith {
    pub activity_id: String,
    pub agent_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvWasInformedBy {
    pub informed_activity_id: String,
    pub informing_activity_id: String,
}

/// Projected PROV graph fragment returned by `observe.audit` / embedded in replay.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProvGraphFragment {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activities: Vec<ProvActivity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<ProvEntity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<ProvAgent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub used: Vec<ProvUsed>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub was_generated_by: Vec<ProvWasGeneratedBy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub was_associated_with: Vec<ProvWasAssociatedWith>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub was_informed_by: Vec<ProvWasInformedBy>,
}

/// in-toto v1 subject descriptor (unsigned Statement wire shape).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InTotoSubjectDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<serde_json::Value>,
}

/// Unsigned in-toto Statement. Signing fields are optional for forward compat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InTotoStatement {
    pub subject: Vec<InTotoSubjectDescriptor>,
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
    pub predicate: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signatures: Option<serde_json::Value>,
}

/// Typed turn attestation predicate (alternative to raw `serde_json::Value`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TurnAttestationPredicate {
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materials: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_summary: Option<serde_json::Value>,
}

/// Ordered call summary within a turn replay trace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TurnCallSummary {
    pub audit_id: i64,
    pub call_id: String,
    pub method: String,
    pub impact_kind: ImpactKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_summary: Option<String>,
}

/// Structural turn replay output (PROV-04) — projection only, no re-execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TurnExecutionTrace {
    pub turn_id: String,
    pub calls: Vec<TurnCallSummary>,
    pub fragment: ProvGraphFragment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<InTotoStatement>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prov_graph_fragment_round_trips() {
        let fragment = ProvGraphFragment {
            activities: vec![ProvActivity {
                id: "act-1".into(),
                method: "observe.workspace".into(),
                impact_kind: ImpactKind::WorkspaceObservation,
                started_at: Some("2026-05-31T00:00:00Z".into()),
                ended_at: None,
                guard_stage: Some("cep_policy_lifecycle".into()),
                semantic_status: Some("ok".into()),
                guard_verdict: Some("allowed".into()),
                outcome_summary: Some("read 42 lines".into()),
                attributes: BTreeMap::new(),
            }],
            entities: vec![ProvEntity {
                id: "ent-1".into(),
                kind: "file".into(),
                digest_or_uri: "sha256:abc".into(),
            }],
            agents: vec![ProvAgent {
                id: "agent-1".into(),
                runtime: "lattice/1.3".into(),
                session_id: "sess-1".into(),
                model_id: "sonnet".into(),
            }],
            used: vec![ProvUsed {
                activity_id: "act-1".into(),
                entity_id: "ent-1".into(),
            }],
            was_generated_by: vec![],
            was_associated_with: vec![ProvWasAssociatedWith {
                activity_id: "act-1".into(),
                agent_id: "agent-1".into(),
            }],
            was_informed_by: vec![],
        };
        let serialized = serde_json::to_string(&fragment).unwrap();
        let parsed: ProvGraphFragment = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, fragment);
    }

    #[test]
    fn prov_activity_attributes_default_for_legacy_fragments() {
        let fragment: ProvGraphFragment = serde_json::from_value(json!({
            "activities": [{
                "id": "act-legacy",
                "method": "exec.command_run",
                "impact_kind": "workspace_mutation",
                "outcome_summary": "success"
            }]
        }))
        .unwrap();

        assert!(fragment.activities[0].attributes.is_empty());
    }

    #[test]
    fn in_toto_statement_deserializes_with_unknown_fields() {
        let json = json!({
            "subject": [{"name": "turn-abc", "digest": {"sha256": "deadbeef"}}],
            "predicateType": CEP_TURN_ATTESTATION_PREDICATE_TYPE,
            "predicate": {"turn_id": "turn-abc", "materials": []},
            "futureSigningField": {"alg": "ed25519"},
            "signatures": null
        });
        let statement: InTotoStatement = serde_json::from_value(json).unwrap();
        assert_eq!(statement.subject.len(), 1);
        assert_eq!(
            statement.predicate_type,
            CEP_TURN_ATTESTATION_PREDICATE_TYPE
        );
        assert_eq!(statement.predicate["turn_id"], "turn-abc");
    }
}
