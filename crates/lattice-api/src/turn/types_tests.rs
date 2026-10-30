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
        let end = match outcome {
            PublicTurnOutcome::Completed => StepEnd::Finish,
            PublicTurnOutcome::Failed => StepEnd::Stop {
                cause: StopCause::ProcessExit,
            },
            PublicTurnOutcome::Cancelled => StepEnd::Stop {
                cause: StopCause::Cancelled,
            },
        };
        let step = StepDetail {
            node: NodeId(2),
            key: StepKey {
                turn_id: turn.turn_id,
                ordinal: 0,
            },
            attempts: vec![],
            progress: StepProgress::Closed { end: end.clone() },
            end,
            slots: vec![],
            updated_at: chrono::Utc::now(),
        };
        let summary = TurnSummary {
            outcome: outcome.clone(),
            progress: step.progress.clone(),
            notice: match outcome {
                PublicTurnOutcome::Completed => None,
                _ => Some(Notice {
                    level: if outcome == PublicTurnOutcome::Failed {
                        NoticeLevel::Error
                    } else {
                        NoticeLevel::Info
                    },
                    text: "persistent notice".into(),
                }),
            },
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
    let summary = UsageSummary::from_attempts(
        [(
            Some(lattice_conversation::TokenUsage {
                input: Some(1),
                output: Some(2),
                ..Default::default()
            }),
            lattice_conversation::UsageCompleteness::Partial,
            None,
        )]
        .into_iter()
        .chain(std::iter::repeat_n(
            (None, lattice_conversation::UsageCompleteness::Partial, None),
            2,
        )),
    );
    let value = serde_json::to_value(&summary).unwrap();
    assert_eq!(value["input"]["known"], 1);
    assert_eq!(value["input"]["unknown_attempts"], 2);
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
