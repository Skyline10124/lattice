// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use futures::StreamExt;
use lattice_llm::LlmEvent;

use super::execute::stream_turn;
use super::test_support::*;
use super::types::TurnEvent;
#[tokio::test]
async fn stream_turn_error_after_partial_commits_error() {
    let events = vec![
        LlmEvent::Token {
            content: "partial output".to_string(),
        },
        LlmEvent::Token {
            content: " before error".to_string(),
        },
        LlmEvent::Error {
            message: "LLM error: connection reset".to_string(),
        },
    ];
    let fixture = setup_with_events(events, &[]);

    let events: Vec<TurnEvent> = stream_turn(
        fixture.session_api,
        with_test_llm(
            make_request("hello"),
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap()
    .collect()
    .await;

    let failed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnFailed { .. }))
        .expect("should have TurnFailed event");
    if let TurnEvent::TurnFailed { reason } = failed {
        assert!(reason.contains("connection reset"));
    }

    let committed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted event");
    if let TurnEvent::TurnCommitted {
        outcome,
        session_id,
        ..
    } = committed
    {
        assert!(matches!(
            outcome,
            lattice_session::TurnOutcome::Interrupted { .. }
        ));
        let transcript = fixture.store.as_ref().head_transcript(session_id).unwrap();
        // ADR 0005 §7 (#139-B Commit 4): partial assistant output from an
        // interrupted attempt is diagnostic state, never committed
        // conversation. The turn still commits with the typed error reason.
        assert!(
            !transcript
                .messages
                .iter()
                .any(|m| m.role == lattice_conversation::MessageRole::Assistant),
            "interrupted partial output must not be committed"
        );
    }
}
