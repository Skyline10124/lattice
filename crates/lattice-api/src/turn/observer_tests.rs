// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use super::*;
use lattice_session::store::{AttemptOrigin, AttemptProtocol};
fn key(attempt_ordinal: u32) -> lancet_protocol::ModelAttemptKey {
    lancet_protocol::ModelAttemptKey {
        turn_id: crate::local::new_turn_id(),
        model_step_ordinal: 0,
        attempt_ordinal,
    }
}
#[test]
fn adjacent_text_merges_and_each_attempt_replaces_the_previous_segments() {
    let observer = ObserverSink::default();
    let first = key(0);
    observer.emit(TurnEvent::ModelStarted {
        key: first,
        origin: AttemptOrigin {
            provider_id: "local".into(),
            protocol: AttemptProtocol::ChatCompletions,
        },
        model: "test".into(),
    });
    observer.emit(TurnEvent::TokenDelta {
        key: first,
        content: "你".into(),
    });
    observer.emit(TurnEvent::TokenDelta {
        key: first,
        content: "好".into(),
    });
    observer.emit(TurnEvent::ReasoningDelta {
        key: first,
        content: "想".into(),
    });
    let state = observer.state.lock().unwrap();
    let segments = &state.attempt.as_ref().unwrap().segments;
    assert_eq!(
        segments,
        &[
            TextSegment::Text {
                text: "你好".into()
            },
            TextSegment::Reasoning { text: "想".into() }
        ]
    );
    assert_eq!(
        suffix(segments, 3),
        (
            vec![
                TextSegment::Text { text: "好".into() },
                TextSegment::Reasoning { text: "想".into() }
            ],
            9
        )
    );
    drop(state);
    let second = lancet_protocol::ModelAttemptKey {
        attempt_ordinal: 1,
        ..first
    };
    observer.emit(TurnEvent::ModelStarted {
        key: second,
        origin: AttemptOrigin {
            provider_id: "local".into(),
            protocol: AttemptProtocol::ChatCompletions,
        },
        model: "test".into(),
    });
    observer.emit(TurnEvent::TokenDelta {
        key: first,
        content: "迟到".into(),
    });
    observer.emit(TurnEvent::TokenDelta {
        key: second,
        content: "新".into(),
    });
    assert_eq!(
        observer
            .state
            .lock()
            .unwrap()
            .attempt
            .as_ref()
            .unwrap()
            .segments,
        vec![TextSegment::Text { text: "新".into() }]
    );
    observer.release();
    assert!(observer.state.lock().unwrap().attempt.is_none());
}
