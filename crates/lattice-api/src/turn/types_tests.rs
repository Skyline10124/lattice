// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::types::{TurnEvent, TurnUsageSummary};
use lattice_session::pending::{
    ClosureOrigin, TurnFinishReason, TurnOutcome, WorkspaceHistoryCheckpoint,
};

#[test]
fn typed_turn_committed_round_trips_without_legacy_fields() {
    let event = TurnEvent::TurnCommitted {
        session_id: "s".into(),
        commit_id: "c".into(),
        outcome: TurnOutcome::Completed {
            finish_reason: TurnFinishReason::Stop,
        },
        origin: ClosureOrigin::Live,
        workspace_history_checkpoint: WorkspaceHistoryCheckpoint::Empty,
    };
    let value = serde_json::to_value(&event).unwrap();
    for retired in ["status", "disposition", "terminal_reason", "lifecycle"] {
        assert!(value.get(retired).is_none());
    }
    let back: TurnEvent = serde_json::from_value(value).unwrap();
    assert_eq!(back, event);
}

#[test]
fn evidence_durability_failure_is_typed_on_the_wire() {
    let event = TurnEvent::EvidenceDurabilityFailed { attempts: 4 };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "evidence_durability_failed");
    assert_eq!(value["attempts"], 4);
    assert!(value.get("reason").is_none());
    assert_eq!(serde_json::from_value::<TurnEvent>(value).unwrap(), event);
}

#[test]
fn invocation_reconciliation_requirement_is_typed_on_the_wire() {
    let invocation_id =
        lancet_protocol::InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap();
    let event = TurnEvent::RecoveryRequired {
        reason: crate::session::RecoveryRequired::InvocationReconciliationRequired {
            invocation_id,
        },
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "recovery_required");
    assert_eq!(
        value["reason"]["InvocationReconciliationRequired"]["invocation_id"],
        invocation_id.to_string()
    );
    assert_eq!(serde_json::from_value::<TurnEvent>(value).unwrap(), event);
}

#[test]
fn usage_summary_preserves_totals() {
    let summary = TurnUsageSummary::from(lattice_session::TurnUsage {
        prompt_tokens: 1,
        completion_tokens: 2,
        total_tokens: 3,
    });
    assert_eq!(summary.total_tokens, 3);
}
