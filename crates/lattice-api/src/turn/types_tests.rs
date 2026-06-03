use lattice_session::{TurnCommitStatus, TurnUsage};

use super::types::{RunTurnStatus, TurnEvent, TurnUsageSummary};

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
        parent_commit_id: None,
        branch: None,
    };
    let json = serde_json::to_value(&event).expect("TurnStarted should serialize");
    assert_eq!(json["type"], "turn_started");
    assert_eq!(json["session_id"], "s1");
}

#[test]
fn old_failed_variant_removed_compile_guard() {
    let status = RunTurnStatus::Error;
    assert_ne!(format!("{status:?}"), "Failed");
    assert_eq!(format!("{status:?}"), "Error");
}
