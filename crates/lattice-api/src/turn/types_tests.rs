// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lattice_session::{TurnCommitStatus, TurnUsage, WorkspaceDisposition};

use super::types::{RunTurnResult, RunTurnStage, RunTurnStatus, TurnEvent, TurnUsageSummary};

#[test]
fn turn_usage_summary_from_turn_usage() {
    let usage = TurnUsage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
    };
    let summary = TurnUsageSummary::from(usage);
    assert_eq!(summary.prompt_tokens, 100);
    assert_eq!(summary.completion_tokens, 50);
    assert_eq!(summary.total_tokens, 150);
}

#[test]
fn status_error_maps_to_commit_error() {
    assert_eq!(
        RunTurnStatus::Error.to_commit_status(),
        TurnCommitStatus::Error
    );
}

#[test]
fn turn_event_serializes_with_type_discriminator() {
    let event = TurnEvent::TokenDelta {
        content: "hi".to_string(),
    };
    let json = serde_json::to_value(&event).expect("TurnEvent should serialize");
    assert_eq!(json["type"], "token_delta");
    assert_eq!(json["content"], "hi");
}

#[test]
fn turn_event_unit_variant_serializes_correctly() {
    let event = TurnEvent::TurnCommitting;
    let json = serde_json::to_value(&event).expect("unit variant should serialize");
    assert_eq!(json["type"], "turn_committing");
    let obj = json
        .as_object()
        .expect("unit variant should serialize as object with `type` only");
    assert_eq!(
        obj.len(),
        1,
        "TurnCommitting should serialize with only `type`, got {obj:?}"
    );
}

#[test]
fn turn_event_struct_variant_with_optional_none() {
    let event = TurnEvent::TurnCancelled { reason: None };
    let json = serde_json::to_value(&event).expect("TurnCancelled should serialize");
    assert_eq!(json["type"], "turn_cancelled");
    assert!(
        json["reason"].is_null(),
        "Optional reason field should serialize as null when None, got {:?}",
        json["reason"]
    );
}

#[test]
fn turn_event_llm_request_error_serializes_structured_fields() {
    let event = TurnEvent::LlmRequestError {
        provider: Some("deepseek".to_string()),
        model: "deepseek-v4-pro".to_string(),
        attempt: 1,
        max_attempts: 4,
        error_kind: "transport".to_string(),
        retryable: true,
        will_retry: true,
        delay_ms: Some(500),
        status_code: None,
        message: "connection reset".to_string(),
    };

    let json = serde_json::to_value(&event).expect("LlmRequestError should serialize");
    assert_eq!(json["type"], "llm_request_error");
    assert_eq!(json["provider"], "deepseek");
    assert_eq!(json["model"], "deepseek-v4-pro");
    assert_eq!(json["attempt"], 1);
    assert_eq!(json["max_attempts"], 4);
    assert_eq!(json["error_kind"], "transport");
    assert_eq!(json["retryable"], true);
    assert_eq!(json["will_retry"], true);
    assert_eq!(json["delay_ms"], 500);
    assert!(json["status_code"].is_null());
    assert_eq!(json["message"], "connection reset");
}

#[test]
fn turn_event_turn_started_serializes() {
    let event = TurnEvent::TurnStarted {
        session_id: "s1".to_string(),
        turn_id: "s1:root".to_string(),
        parent_commit_id: None,
        branch: None,
    };
    let json = serde_json::to_value(&event).expect("TurnStarted should serialize");
    assert_eq!(json["type"], "turn_started");
    assert_eq!(json["session_id"], "s1");
    assert_eq!(json["turn_id"], "s1:root");
}

#[test]
fn old_failed_variant_removed_compile_guard() {
    let status = RunTurnStatus::Error;
    assert_ne!(format!("{status:?}"), "Failed");
    assert_eq!(format!("{status:?}"), "Error");
}

/// TurnCommitted serializes with separate `"status"` and `"disposition"` fields.
/// Both are independent terminal dimensions.
#[test]
fn turn_event_disposition_turn_committed_has_status_and_disposition() {
    let event = TurnEvent::TurnCommitted {
        session_id: "sess-1".to_string(),
        commit_id: "commit-1".to_string(),
        status: RunTurnStatus::Completed,
        disposition: WorkspaceDisposition::Promoted,
    };
    let json = serde_json::to_value(&event).expect("TurnCommitted should serialize");
    assert_eq!(json["type"], "turn_committed");
    assert_eq!(json["session_id"], "sess-1");
    assert_eq!(json["commit_id"], "commit-1");
    assert_eq!(
        json["status"], "completed",
        "status must be present as separate field"
    );
    assert_eq!(
        json["disposition"], "promoted",
        "disposition must be present as separate field"
    );
}

/// TurnCommitted disposition round-trips all WorkspaceDisposition variants via snake_case.
#[test]
fn turn_event_disposition_all_variants_roundtrip() {
    let variants = [
        WorkspaceDisposition::Promoted,
        WorkspaceDisposition::PendingWorktree,
        WorkspaceDisposition::PendingVerification,
        WorkspaceDisposition::PendingPromotion,
        WorkspaceDisposition::PendingConflict,
        WorkspaceDisposition::Rejected,
        WorkspaceDisposition::Aborted,
        WorkspaceDisposition::NotApplicable,
    ];
    for disposition in &variants {
        let event = TurnEvent::TurnCommitted {
            session_id: "s".to_string(),
            commit_id: "c".to_string(),
            status: RunTurnStatus::Completed,
            disposition: disposition.clone(),
        };
        let json = serde_json::to_value(&event).expect("serialize");
        let deserialized: TurnEvent = serde_json::from_value(json).expect("deserialize round-trip");
        assert_eq!(event, deserialized, "round-trip failed for {disposition:?}");
    }
}

/// RunTurnResult has non-optional disposition that round-trips through JSON.
#[test]
fn turn_event_disposition_run_turn_result_has_disposition() {
    let result = RunTurnResult {
        session_id: "s1".to_string(),
        commit_id: "c1".to_string(),
        assistant_output: "output".to_string(),
        status: RunTurnStatus::Completed,
        disposition: WorkspaceDisposition::PendingWorktree,
        usage: None,
        tool_results: vec![],
        stages: vec![RunTurnStage::Profile],
    };
    let json = serde_json::to_value(&result).expect("RunTurnResult should serialize");
    assert_eq!(
        json["disposition"], "pending_worktree",
        "RunTurnResult.disposition must serialize"
    );
    let deserialized: RunTurnResult =
        serde_json::from_value(json).expect("RunTurnResult should deserialize");
    assert_eq!(
        deserialized.disposition,
        WorkspaceDisposition::PendingWorktree
    );
}

/// Deserialization of TurnCommitted with missing disposition must fail.
/// The field is non-optional; observers must receive the actual durable outcome.
#[test]
fn turn_event_disposition_missing_fails_deserialization() {
    let json = serde_json::json!({
        "type": "turn_committed",
        "session_id": "s1",
        "commit_id": "c1",
        "status": "completed"
        // "disposition" intentionally absent
    });
    let result: Result<TurnEvent, _> = serde_json::from_value(json);
    assert!(
        result.is_err(),
        "deserialization must fail when disposition is absent from TurnCommitted"
    );
}

/// Deserialization of TurnCommitted with unknown disposition variant must fail.
#[test]
fn turn_event_disposition_unknown_variant_fails() {
    let json = serde_json::json!({
        "type": "turn_committed",
        "session_id": "s1",
        "commit_id": "c1",
        "status": "completed",
        "disposition": "unknown_future_variant"
    });
    let result: Result<TurnEvent, _> = serde_json::from_value(json);
    assert!(
        result.is_err(),
        "deserialization must fail for unknown disposition variant"
    );
}
