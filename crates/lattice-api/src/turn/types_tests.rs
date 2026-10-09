// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::types::{TurnEvent, TurnRef};
use lancet_protocol::{SessionId, TurnId};
use lattice_session::store::*;

#[test]
fn terminal_events_round_trip_the_persistent_summary_and_step() {
    let turn = TurnRef {
        session_id: SessionId::parse("00000000000000000000000000000001").unwrap(),
        turn_id: TurnId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
    };
    for outcome in [
        PublicTurnOutcome::Completed,
        PublicTurnOutcome::Failed,
        PublicTurnOutcome::Cancelled,
    ] {
        let step = StepDetail {
            key: StepKey {
                turn_id: turn.turn_id,
                ordinal: 0,
            },
            attempts: vec![],
            state: StepDetailState::Closed {
                end: match outcome {
                    PublicTurnOutcome::Completed => StepEnd::Finish { node: NodeId(2) },
                    PublicTurnOutcome::Failed => StepEnd::Stop {
                        node: None,
                        cause: StopCause::ProcessExit,
                    },
                    PublicTurnOutcome::Cancelled => StepEnd::Stop {
                        node: None,
                        cause: StopCause::Cancelled,
                    },
                },
                slots: vec![],
            },
        };
        let summary = TurnSummary {
            outcome: outcome.clone(),
            notice: Some(Notice {
                level: if outcome == PublicTurnOutcome::Failed {
                    NoticeLevel::Error
                } else {
                    NoticeLevel::Info
                },
                text: "persistent notice".into(),
            }),
        };
        let event = match outcome {
            PublicTurnOutcome::Completed => TurnEvent::TurnCompleted {
                turn,
                step: step.clone(),
                summary: summary.clone(),
            },
            PublicTurnOutcome::Failed => TurnEvent::TurnFailed {
                turn,
                step: step.clone(),
                summary: summary.clone(),
            },
            PublicTurnOutcome::Cancelled => TurnEvent::TurnCancelled {
                turn,
                step: step.clone(),
                summary: summary.clone(),
            },
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["summary"], serde_json::to_value(summary).unwrap());
        assert_eq!(value["step"], serde_json::to_value(step).unwrap());
        assert_eq!(serde_json::from_value::<TurnEvent>(value).unwrap(), event);
    }
}

#[test]
fn usage_summary_preserves_totals_and_unknown_attempts() {
    let summary = UsageSummary::Partial {
        known: lattice_conversation::TokenUsage {
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
        },
        unknown_attempts: 2,
    };
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(value["known"]["total_tokens"], 3);
    assert_eq!(value["unknown_attempts"], 2);
    assert_eq!(
        serde_json::from_value::<UsageSummary>(value).unwrap(),
        summary
    );
}

#[test]
fn retry_event_round_trips_a_warn_notice_and_required_delay() {
    let value = serde_json::json!({
        "type": "llm_retry",
        "key": {
            "turn_id": "00000000-0000-4000-8000-000000000001",
            "model_step_ordinal": 2,
            "attempt_ordinal": 1
        },
        "delay_ms": 40,
        "notice": {"level": "warn", "text": "provider diagnostic"}
    });
    let event: TurnEvent = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(&event, TurnEvent::LlmRetry { notice, .. }
        if notice.level == NoticeLevel::Warn && notice.text == "provider diagnostic"));
    assert_eq!(serde_json::to_value(event).unwrap(), value);
    let mut missing_delay = value.clone();
    missing_delay.as_object_mut().unwrap().remove("delay_ms");
    assert!(serde_json::from_value::<TurnEvent>(missing_delay).is_err());
    let mut null_delay = value;
    null_delay["delay_ms"] = serde_json::Value::Null;
    assert!(serde_json::from_value::<TurnEvent>(null_delay).is_err());
}
