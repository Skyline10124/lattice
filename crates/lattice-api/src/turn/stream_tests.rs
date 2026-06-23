// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use futures::StreamExt;
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_session::TurnCommitStatus;

use super::execute::stream_turn;
use super::test_support::*;
use super::types::{RunTurnStatus, TurnEvent};

#[tokio::test]
async fn stream_turn_emits_tool_activity_before_committed() {
    let round1 = vec![
        LlmEvent::ToolCallStart {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
        },
        LlmEvent::ToolCallDelta {
            id: "call_1".to_string(),
            arguments_delta: r#"{"path":"src/lib.rs"}"#.to_string(),
        },
        LlmEvent::ToolCallEnd {
            id: "call_1".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "tool_calls".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
    ];
    let round2 = vec![
        LlmEvent::Token {
            content: "here is the file".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: None,
        },
    ];

    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(round1)
            .push_events(round2)
            .build(),
    );
    let fixture = setup_fixture(llm, &["observe.workspace"]);

    let events: Vec<TurnEvent> = stream_turn(
        fixture.session_api,
        with_test_llm(
            make_request("read file"),
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap()
    .collect()
    .await;

    let tool_start_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ToolCallStarted { .. }));
    let tool_finished_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ToolCallFinished { .. }));
    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitted { .. }));

    assert!(tool_start_pos.is_some());
    assert!(tool_finished_pos.is_some());
    assert!(committed_pos.is_some());
    assert!(tool_start_pos.unwrap() < committed_pos.unwrap());
    assert!(tool_finished_pos.unwrap() < committed_pos.unwrap());
}

#[tokio::test]
async fn stream_turn_error_still_commits_error() {
    let events = vec![LlmEvent::Error {
        message: "LLM error: provider unavailable".to_string(),
    }];
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

    let committed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted event even on failure");

    if let TurnEvent::TurnCommitted { status, .. } = committed {
        assert_eq!(*status, RunTurnStatus::Error);
    }

    let failed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnFailed { .. }))
        .expect("should have TurnFailed event before commit");
    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted event");
    assert!(failed_pos < committed_pos);

    let session_id = events
        .iter()
        .find_map(|e| match e {
            TurnEvent::TurnCommitted { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        last_log_entry(fixture.store.as_ref(), &session_id).status,
        TurnCommitStatus::Error
    );
}

#[tokio::test]
async fn stream_turn_cancelled_commits_cancelled() {
    let cancel = Arc::new(AtomicBool::new(false));
    let llm: Arc<dyn LlmClient> = Arc::new(CancelOnDoneLlm::new(
        cancel.clone(),
        vec![
            LlmEvent::ToolCallStart {
                id: "call_1".to_string(),
                name: "observe.workspace".to_string(),
            },
            LlmEvent::ToolCallDelta {
                id: "call_1".to_string(),
                arguments_delta: r#"{"mode":"read_file","path":"src/lib.rs"}"#.to_string(),
            },
            LlmEvent::ToolCallEnd {
                id: "call_1".to_string(),
            },
        ],
        "tool_calls",
        None,
    ));
    let fixture = setup_fixture(llm, &["observe.workspace"]);
    let mut request = make_request("hello");
    request.cancel_flag = Some(cancel);

    let events: Vec<TurnEvent> = stream_turn(
        fixture.session_api,
        with_test_llm(
            request,
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap()
    .collect()
    .await;

    let cancelled = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnCancelled { .. }))
        .expect("should have TurnCancelled event");
    if let TurnEvent::TurnCancelled { reason } = cancelled {
        assert!(
            reason.as_ref().unwrap().contains("cancelled"),
            "TurnCancelled reason should mention cancellation"
        );
    }

    let committing_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitting))
        .expect("should have TurnCommitting event");
    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted event");
    if let TurnEvent::TurnCommitted {
        status, session_id, ..
    } = &events[committed_pos]
    {
        assert_eq!(*status, RunTurnStatus::Cancelled);
        let entry = last_log_entry(fixture.store.as_ref(), session_id);
        assert_eq!(entry.status, TurnCommitStatus::Cancelled);
    }

    let cancelled_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCancelled { .. }))
        .unwrap();
    assert!(cancelled_pos < committing_pos);
    assert!(committing_pos < committed_pos);
}

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

#[tokio::test]
async fn stream_turn_emits_cep_lifecycle_events() {
    use lancet_catalog::CepMethod;
    use lancet_catalog::methods::TodoWrite;

    let tool_events = vec![
        LlmEvent::ToolCallStart {
            id: "c1".to_string(),
            name: TodoWrite::NAME.to_string(),
        },
        LlmEvent::ToolCallDelta {
            id: "c1".to_string(),
            arguments_delta: r#"{"todos":[]}"#.to_string(),
        },
        LlmEvent::ToolCallEnd {
            id: "c1".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "tool_calls".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 2,
                total_tokens: 7,
            }),
        },
        LlmEvent::Token {
            content: "done".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 1,
                total_tokens: 6,
            }),
        },
    ];

    let fixture = setup_with_events(tool_events, &[TodoWrite::NAME]);
    let request = with_test_llm(
        make_request("write todos"),
        fixture.llm.clone(),
        &fixture.enabled_tools,
        &fixture.project_root,
    );

    let events: Vec<TurnEvent> = stream_turn(fixture.session_api, request)
        .await
        .expect("stream_turn should start")
        .collect()
        .await;

    let cep_started = events
        .iter()
        .position(|e| matches!(e, TurnEvent::CepCallStarted { .. }))
        .expect("CepCallStarted must be present");
    let cep_finished = events
        .iter()
        .position(|e| matches!(e, TurnEvent::CepCallFinished { .. }))
        .expect("CepCallFinished must be present");
    assert!(
        cep_started < cep_finished,
        "CepCallStarted must precede CepCallFinished"
    );

    let cep_started_event = events
        .iter()
        .find(
            |e| matches!(e, TurnEvent::CepCallStarted { method, .. } if method == TodoWrite::NAME),
        )
        .expect("CepCallStarted for session.todo_write must be present");
    if let TurnEvent::CepCallStarted {
        semantic_stage,
        guard_stage,
        impact_kind,
        ..
    } = cep_started_event
    {
        assert_eq!(semantic_stage, "cep_native");
        assert_eq!(guard_stage, "cep_policy_lifecycle");
        assert_eq!(impact_kind, "session_state_mutation");
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolCallStarted { .. })),
        "ToolCallStarted must remain distinct from CepCallStarted"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TurnEvent::ToolCallFinished { .. })),
        "ToolCallFinished must remain distinct from CepCallFinished"
    );
    let tool_finished = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ToolCallFinished { .. }))
        .expect("ToolCallFinished must be present");
    assert!(
        cep_finished < tool_finished,
        "CepCallFinished must precede ToolCallFinished"
    );
}
