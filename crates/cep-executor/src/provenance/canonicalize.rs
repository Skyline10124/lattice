//! Canonicalization helpers for structural equality (PROV-06 / D-15).

use cep_protocol::provenance::{InTotoStatement, ProvGraphFragment, TurnExecutionTrace};
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
    if let Some(obj) = out.predicate.as_object_mut() {
        mask_object_timestamps(obj);
    }
    out
}

/// Compare two turn traces modulo timestamps and child PID metadata.
pub fn traces_equal_modulo_nondeterminism(a: &TurnExecutionTrace, b: &TurnExecutionTrace) -> bool {
    canonicalize_turn_trace(a) == canonicalize_turn_trace(b)
}

fn mask_object_timestamps(obj: &mut serde_json::Map<String, Value>) {
    for key in NONDETERMINISTIC_METADATA_KEYS {
        if obj.contains_key(*key) {
            obj.insert(key.to_string(), Value::String(MASKED.into()));
        }
    }
    for value in obj.values_mut() {
        if let Some(nested) = value.as_object_mut() {
            mask_object_timestamps(nested);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cep_protocol::envelope::ImpactKind;
    use cep_protocol::provenance::{
        ProvActivity, ProvAgent, ProvGraphFragment, TurnCallSummary, TurnExecutionTrace,
    };

    #[test]
    fn canonicalize_strips_timestamps() {
        let fragment = ProvGraphFragment {
            activities: vec![ProvActivity {
                id: "activity:1".into(),
                method: "workspace.read_file".into(),
                impact_kind: ImpactKind::WorkspaceObservation,
                started_at: Some("111".into()),
                ended_at: Some("222".into()),
                guard_stage: None,
                semantic_status: None,
                guard_verdict: None,
                outcome_summary: None,
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
                method: "workspace.read_file".into(),
                impact_kind: ImpactKind::WorkspaceObservation,
                envelope_id: None,
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
}
