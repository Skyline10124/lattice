// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0
use futures::StreamExt;
use lattice_llm::LlmEvent;
use lattice_session::TurnCommitStatus;

use super::execute::stream_turn;
use super::test_support::*;
use super::types::{RunTurnStatus, TurnEvent};
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
        status, session_id, ..
    } = committed
    {
        assert_eq!(*status, RunTurnStatus::Error);
        let transcript = fixture.store.as_ref().head_transcript(session_id).unwrap();
        let assistant = transcript
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .expect("assistant message in commit");
        assert_eq!(assistant.content, "partial output before error");
        let entry = last_log_entry(fixture.store.as_ref(), session_id);
        assert_eq!(entry.status, TurnCommitStatus::Error);
        assert!(
            entry
                .terminal_reason
                .as_ref()
                .unwrap()
                .contains("connection reset")
        );
    }
}
