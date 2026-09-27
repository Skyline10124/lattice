// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
use lattice_conversation::ProviderOrigin;

fn key(attempt_ordinal: u32) -> lancet_protocol::ModelAttemptKey {
    lancet_protocol::ModelAttemptKey {
        turn_id: lancet_protocol::TurnId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
        model_step_ordinal: 0,
        attempt_ordinal,
    }
}
fn start(observer: &ObserverSink, attempt: u32) {
    observer.emit(TurnEvent::ModelStarted {
        key: key(attempt),
        provider: ProviderOrigin::OpenAiCompatible,
        model: "model".into(),
    });
}
#[test]
fn slow_observer_keeps_all_segments_and_resynchronizes_outside_window() {
    let observer = ObserverSink::default();
    start(&observer, 0);
    for i in 0..70 {
        let event = if i % 2 == 0 {
            TurnEvent::TokenDelta {
                key: key(0),
                content: format!("文{i}"),
            }
        } else {
            TurnEvent::ReasoningDelta {
                key: key(0),
                content: format!("理{i}"),
            }
        };
        observer.emit(event);
    }
    let state = observer.state.lock().unwrap();
    assert_eq!(state.seq, 71);
    assert_eq!(state.events.len(), 64);
    assert_eq!(state.attempt.as_ref().unwrap().segments.len(), 70);
    assert_eq!(
        state.attempt.as_ref().unwrap().segments[0],
        TextSegment::Text {
            text: "文0".into()
        }
    );
    assert_eq!(
        state.attempt.as_ref().unwrap().segments[69],
        TextSegment::Reasoning {
            text: "理69".into()
        }
    );
    drop(state);
    assert!(observer.events_after(0).is_none());
    assert!(observer.events_after(72).is_none());
    let events = observer.events_after(7).unwrap();
    assert_eq!(events.len(), 64);
    assert_eq!(events[0].sequence, 8);
    assert_eq!(events[63].sequence, 71);
    start(&observer, 1);
    observer.emit(TurnEvent::TokenDelta {
        key: key(0),
        content: "迟到旧段".into(),
    });
    observer.emit(TurnEvent::TokenDelta {
        key: key(1),
        content: "新".into(),
    });
    observer.emit(TurnEvent::TokenDelta {
        key: key(1),
        content: "段".into(),
    });
    let state = observer.state.lock().unwrap();
    assert_eq!(state.seq, 74);
    assert_eq!(
        state.attempt.as_ref().unwrap().segments,
        vec![TextSegment::Text {
            text: "新段".into()
        }]
    );
}
