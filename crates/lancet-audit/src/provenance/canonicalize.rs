// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Canonicalization helpers for structural equality.

use lancet_protocol::provenance::{InTotoStatement, ProvGraphFragment, TurnExecutionTrace};
use serde_json::Value;

const MASKED: &str = "<masked>";

/// Keys stripped or normalized before comparing traces/statements.
const NONDETERMINISTIC_METADATA_KEYS: &[&str] = &[
    "started_at",
    "ended_at",
    "child_pid",
    "pid",
    "timestamp",
    "generated_at",
];

/// Normalize a PROV fragment for structural comparison (masks timestamps on activities).
pub fn canonicalize_prov_fragment(fragment: &ProvGraphFragment) -> ProvGraphFragment {
    let mut out = fragment.clone();
    for activity in &mut out.activities {
        activity.started_at = activity.started_at.as_ref().map(|_| MASKED.into());
        activity.ended_at = activity.ended_at.as_ref().map(|_| MASKED.into());
    }
    out
}

/// Normalize a turn trace (fragment + call summaries).
pub fn canonicalize_turn_trace(trace: &TurnExecutionTrace) -> TurnExecutionTrace {
    let mut out = trace.clone();
    out.fragment = canonicalize_prov_fragment(&out.fragment);
    if let Some(ref mut statement) = out.statement {
        *statement = canonicalize_in_toto_statement(statement);
    }
    out
}

/// Normalize an in-toto statement predicate (masks volatile predicate fields).
pub fn canonicalize_in_toto_statement(statement: &InTotoStatement) -> InTotoStatement {
    let mut out = statement.clone();
    mask_timestamps(&mut out.predicate);
    out
}

/// Compare two turn traces modulo timestamps and child PID metadata.
pub fn traces_equal_modulo_nondeterminism(a: &TurnExecutionTrace, b: &TurnExecutionTrace) -> bool {
    canonicalize_turn_trace(a) == canonicalize_turn_trace(b)
}

/// Mask volatile keys at every depth. Arrays are descended into as well: v2
/// predicates carry materials and per-call evidence as arrays of objects, and
/// a comparison that stopped at the array would silently ignore them.
fn mask_timestamps(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            for key in NONDETERMINISTIC_METADATA_KEYS {
                if obj.contains_key(*key) {
                    obj.insert((*key).to_string(), Value::String(MASKED.into()));
                }
            }
            for nested in obj.values_mut() {
                mask_timestamps(nested);
            }
        }
        Value::Array(items) => {
            for item in items {
                mask_timestamps(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::effect_types::ImpactKind;
    use lancet_protocol::provenance::{
        ProvActivity, ProvAgent, ProvGraphFragment, TurnCallSummary, TurnExecutionTrace,
    };

    #[test]
    fn canonicalize_strips_timestamps() {
        let fragment = ProvGraphFragment {
            activities: vec![ProvActivity {
                id: "activity:1".into(),
                method: "observe.workspace".into(),
                impact_kind: ImpactKind::WorkspaceObservation,
                started_at: Some("111".into()),
                ended_at: Some("222".into()),
                guard_stage: None,
                semantic_status: None,
                guard_verdict: None,
                outcome_summary: None,
                attributes: Default::default(),
            }],
            agents: vec![ProvAgent {
                id: "agent:sess:model".into(),
                runtime: "lattice".into(),
                session_id: "sess".into(),
                model_id: "model".into(),
            }],
            ..Default::default()
        };
        let canon = canonicalize_prov_fragment(&fragment);
        assert_eq!(canon.activities[0].started_at.as_deref(), Some(MASKED));
        assert_eq!(canon.activities[0].ended_at.as_deref(), Some(MASKED));

        let trace = TurnExecutionTrace {
            turn_id: "turn-1".into(),
            calls: vec![TurnCallSummary {
                audit_id: 1,
                call_id: "1".into(),
                method: "observe.workspace".into(),
                impact_kind: ImpactKind::WorkspaceObservation,
                outcome_summary: None,
            }],
            fragment: fragment.clone(),
            statement: None,
        };
        let other = TurnExecutionTrace {
            turn_id: "turn-1".into(),
            calls: trace.calls.clone(),
            fragment: ProvGraphFragment {
                activities: vec![ProvActivity {
                    started_at: Some("999".into()),
                    ended_at: Some("888".into()),
                    ..fragment.activities[0].clone()
                }],
                ..fragment
            },
            statement: None,
        };
        assert!(traces_equal_modulo_nondeterminism(&trace, &other));
    }

    #[test]
    fn masking_descends_into_predicate_arrays() {
        // v2 predicates carry materials and per-call evidence as arrays; a
        // comparison that stopped at the array would ignore their contents.
        let statement = InTotoStatement {
            subject: Vec::new(),
            predicate_type: "https://lattice.dev/attestation/cep-turn/v2".into(),
            predicate: serde_json::json!({
                "evidence": [{"audit_id": 1, "admission": {"generated_at": "111"}}]
            }),
            _type: None,
            signatures: None,
        };
        let other = InTotoStatement {
            predicate: serde_json::json!({
                "evidence": [{"audit_id": 1, "admission": {"generated_at": "999"}}]
            }),
            ..statement.clone()
        };

        let canon = canonicalize_in_toto_statement(&statement);
        assert_eq!(
            canon.predicate["evidence"][0]["admission"]["generated_at"],
            serde_json::json!(MASKED)
        );
        assert_eq!(canon, canonicalize_in_toto_statement(&other));
    }
}
