// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;

use lancet_audit::invocation_store::{
    BindingIdentity, IndeterminateReason, InvocationRecord, InvocationState, LedgerSnapshot,
    NotExecutedReason, PlanReservation, ReservationBinding, TerminalEvidence, TerminalMaterial,
    TerminalOutcome, UnavailableMaterialReason,
};
use lancet_protocol::{
    ADMISSION_EVIDENCE_VERSION, AdmissionEvidence, DENIED_OPERATIONS_RECORD_VERSION,
    DeniedOperationsRecord, InvocationId, OBSERVED_EFFECTS_RECORD_VERSION, ObservedEffectsRecord,
    PlanKey, SessionId, Trust, TurnId,
};

#[test]
fn invocation_ledger_wire_shapes_are_exact() {
    assert_eq!(
        serde_json::to_value(InvocationState::Reserved).unwrap(),
        serde_json::json!("reserved")
    );
    assert_eq!(
        serde_json::to_value(InvocationState::DispatchStarted).unwrap(),
        serde_json::json!("dispatch_started")
    );
    assert_eq!(
        serde_json::to_value(TerminalOutcome::Succeeded).unwrap(),
        serde_json::json!("succeeded")
    );
    assert_eq!(
        serde_json::to_value(TerminalOutcome::NotExecuted {
            reason: NotExecutedReason::BeforeDispatch,
        })
        .unwrap(),
        serde_json::json!({"not_executed": {"reason": "before_dispatch"}})
    );
    assert_eq!(
        serde_json::to_value(TerminalOutcome::Indeterminate {
            reason: IndeterminateReason::MayHaveOccurred,
        })
        .unwrap(),
        serde_json::json!({"indeterminate": {"reason": "may_have_occurred"}})
    );
    assert_eq!(
        serde_json::to_value(TerminalMaterial::Available {
            digest: [0xaa; 32],
            byte_length: 3,
        })
        .unwrap(),
        serde_json::json!({
            "available": {
                "digest": "aa".repeat(32),
                "byte_length": 3,
            }
        })
    );
    assert_eq!(
        serde_json::to_value(TerminalMaterial::Unavailable {
            reason: UnavailableMaterialReason::DraftPublicationFailed,
            digest: None,
            byte_length: None,
        })
        .unwrap(),
        serde_json::json!({
            "unavailable": {
                "reason": "draft_publication_failed",
                "digest": null,
                "byte_length": null,
            }
        })
    );

    let evidence = TerminalEvidence {
        admission: AdmissionEvidence {
            version: ADMISSION_EVIDENCE_VERSION,
            tool: "test.echo".to_string(),
            approved_capabilities: Vec::new(),
            denied_capabilities: Vec::new(),
            boundary_excluded: Vec::new(),
            max_boundary: Trust::In,
        },
        effects: ObservedEffectsRecord {
            version: OBSERVED_EFFECTS_RECORD_VERSION,
            effects: Vec::new(),
        },
        denied: DeniedOperationsRecord {
            version: DENIED_OPERATIONS_RECORD_VERSION,
            denied: Vec::new(),
        },
    };
    let expected_evidence = serde_json::json!({
        "admission": {
            "version": ADMISSION_EVIDENCE_VERSION,
            "tool": "test.echo",
            "approved_capabilities": [],
            "denied_capabilities": [],
            "boundary_excluded": [],
            "max_boundary": "in",
        },
        "effects": {
            "version": OBSERVED_EFFECTS_RECORD_VERSION,
            "effects": [],
        },
        "denied": {
            "version": DENIED_OPERATIONS_RECORD_VERSION,
            "denied": [],
        },
    });
    assert_eq!(serde_json::to_value(&evidence).unwrap(), expected_evidence);
    assert_eq!(
        serde_json::to_value(InvocationState::Terminal {
            outcome: TerminalOutcome::Succeeded,
            error: None,
            material: TerminalMaterial::Available {
                digest: [0xaa; 32],
                byte_length: 3,
            },
            evidence: Box::new(evidence),
        })
        .unwrap(),
        serde_json::json!({
            "terminal": {
                "outcome": "succeeded",
                "error": null,
                "material": {
                    "available": {
                        "digest": "aa".repeat(32),
                        "byte_length": 3,
                    }
                },
                "evidence": expected_evidence,
            }
        })
    );

    let session_id = SessionId::parse("11111111111111111111111111111111").unwrap();
    let turn_id = TurnId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    let invocation_id = InvocationId::parse("33333333-3333-4333-8333-333333333333").unwrap();
    let plan_key = PlanKey {
        turn_id,
        model_step_ordinal: 7,
    };
    let identity = BindingIdentity {
        session_id,
        turn_id,
        model_step_ordinal: 7,
        part_ordinal: 2,
        method: "test.echo".to_string(),
        descriptor_identity: "descriptor-v1".to_string(),
        arguments_digest: [0xbb; 32],
    };
    let mut invocations = BTreeMap::new();
    invocations.insert(
        invocation_id,
        InvocationRecord {
            owner: plan_key,
            state: InvocationState::Reserved,
            binding: identity.clone(),
        },
    );
    let snapshot = LedgerSnapshot {
        plans: vec![PlanReservation {
            plan_key,
            bindings: vec![ReservationBinding {
                invocation_id,
                identity,
            }],
        }],
        invocations,
    };
    let expected_identity = serde_json::json!({
        "session_id": "11111111111111111111111111111111",
        "turn_id": "22222222-2222-4222-8222-222222222222",
        "model_step_ordinal": 7,
        "part_ordinal": 2,
        "method": "test.echo",
        "descriptor_identity": "descriptor-v1",
        "arguments_digest": "bb".repeat(32),
    });
    let expected_plan_key = serde_json::json!({
        "turn_id": "22222222-2222-4222-8222-222222222222",
        "model_step_ordinal": 7,
    });
    assert_eq!(
        serde_json::to_value(snapshot).unwrap(),
        serde_json::json!({
            "plans": [{
                "plan_key": expected_plan_key,
                "bindings": [{
                    "invocation_id": "33333333-3333-4333-8333-333333333333",
                    "identity": expected_identity,
                }],
            }],
            "invocations": {
                "33333333-3333-4333-8333-333333333333": {
                    "owner": expected_plan_key,
                    "state": "reserved",
                    "binding": expected_identity,
                },
            },
        })
    );
}
