// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_session::TurnCommitStatus;

use super::execute::run_turn;
use super::test_support::*;
use super::types::{RunTurnRequest, RunTurnStage, RunTurnStatus};
use crate::session::{CreateSessionRequest, SessionApi};

#[tokio::test]
async fn run_turn_uses_required_session_and_returns_session_id() {
    let fixture = setup_with_events(happy_events(), &[]);
    let result = run_turn(
        fixture.session_api,
        with_test_llm(
            make_request("hello"),
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap();

    assert!(
        !result.session_id.is_empty(),
        "session_id should not be empty"
    );
    assert!(
        !result.commit_id.is_empty(),
        "commit_id should not be empty"
    );
    assert_eq!(fixture.store.log(&result.session_id).unwrap().len(), 1);
}

#[tokio::test]
async fn run_turn_commits_exactly_one_turn() {
    let fixture = setup_with_events(happy_events(), &[]);
    let result = run_turn(
        fixture.session_api,
        with_test_llm(
            make_request("test input"),
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap();

    assert_eq!(result.status, RunTurnStatus::Completed);
    assert_eq!(fixture.store.log(&result.session_id).unwrap().len(), 1);
}

#[tokio::test]
async fn run_turn_uses_existing_session_transcript() {
    let fixture = setup_with_events(happy_events(), &[]);
    let created = fixture
        .session_api
        .create_session(CreateSessionRequest {
            label: None,
            active_agent: "test".to_string(),
            project_branch: "main".to_string(),
        })
        .unwrap();
    let session_id = created.session_id.clone();

    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let mut request = make_request_with_session("second turn", &session_id);
    request.test_llm = Some(llm);
    request.test_project_root = Some(fixture.project_root.clone());

    let result = run_turn(fixture.session_api, request).await.unwrap();

    assert_eq!(result.session_id, session_id);
    assert_eq!(fixture.store.log(&session_id).unwrap().len(), 1);
}

#[tokio::test]
async fn run_turn_result_contains_contract_fields() {
    let fixture = setup_with_events(happy_events(), &[]);
    let result = run_turn(
        fixture.session_api,
        with_test_llm(
            make_request("hello"),
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap();

    assert!(!result.session_id.is_empty());
    assert!(!result.commit_id.is_empty());
    assert_eq!(result.assistant_output, "test response");
    assert_eq!(result.status, RunTurnStatus::Completed);
    let usage = result.usage.as_ref().expect("usage should be present");
    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 5);
    assert_eq!(usage.total_tokens, 15);
    assert!(result.tool_results.is_empty());
    assert_eq!(
        result.stages,
        vec![
            RunTurnStage::Profile,
            RunTurnStage::Session,
            RunTurnStage::Agent,
            RunTurnStage::Llm,
            RunTurnStage::Tools,
            RunTurnStage::Commit,
            RunTurnStage::Render,
        ]
    );
}

#[tokio::test]
async fn run_turn_failed_outcome_is_committed() {
    let events = vec![LlmEvent::Error {
        message: "LLM error: provider unavailable".to_string(),
    }];
    let fixture = setup_with_events(events, &[]);
    let result = run_turn(
        fixture.session_api,
        with_test_llm(
            make_request("hello"),
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap();

    assert_eq!(result.status, RunTurnStatus::Error);
    assert_eq!(result.assistant_output, "");
    let entry = last_log_entry(fixture.store.as_ref(), &result.session_id);
    assert_eq!(entry.status, TurnCommitStatus::Error);
    assert!(
        entry
            .terminal_reason
            .as_ref()
            .unwrap()
            .contains("provider unavailable")
    );
}

#[tokio::test]
async fn run_turn_cancelled_outcome_is_committed() {
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
        Some(TokenUsage {
            prompt_tokens: 5,
            completion_tokens: 2,
            total_tokens: 7,
        }),
    ));
    let fixture = setup_fixture(llm, &["observe.workspace"]);
    let mut request = make_request("hello");
    request.cancel_flag = Some(cancel);

    let result = run_turn(
        fixture.session_api,
        with_test_llm(
            request,
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap();

    assert_eq!(result.status, RunTurnStatus::Cancelled);
    let entry = last_log_entry(fixture.store.as_ref(), &result.session_id);
    assert_eq!(entry.status, TurnCommitStatus::Cancelled);
    assert!(
        entry
            .terminal_reason
            .as_ref()
            .unwrap()
            .contains("cancelled")
    );
    assert_eq!(entry.usage.as_ref().unwrap().total_tokens, 7);
}

#[tokio::test]
async fn run_cutover_uses_loaded_profile_model() {
    let fixture = setup_with_events(happy_events(), &[]);
    let request = with_test_llm(
        RunTurnRequest {
            input: "test".to_string(),
            session_id: TEST_SESSION_ID.to_string(),
            loaded_profile: Some(make_loaded_profile("profile-model", "System from profile.")),
            cancel_flag: None,
            test_llm: None,
            test_project_root: None,
            test_persist_audit: None,
            workspace_override: None,
        },
        fixture.llm.clone(),
        &fixture.enabled_tools,
        &fixture.project_root,
    );

    let result = run_turn(fixture.session_api, request).await.unwrap();
    assert_eq!(result.status, RunTurnStatus::Completed);

    let entry = last_log_entry(fixture.store.as_ref(), &result.session_id);
    assert_eq!(entry.model, "profile-model");
}

#[tokio::test]
async fn run_turn_plumbs_stable_provider_to_agent_request_and_commit_metadata() {
    let fixture = setup_with_events(happy_events(), &[]);
    let request = with_test_llm(
        RunTurnRequest {
            input: "test".to_string(),
            session_id: TEST_SESSION_ID.to_string(),
            loaded_profile: Some(make_loaded_profile(
                "compat:deepseek-chat",
                "System from profile.",
            )),
            cancel_flag: None,
            test_llm: None,
            test_project_root: None,
            test_persist_audit: None,
            workspace_override: None,
        },
        fixture.llm.clone(),
        &fixture.enabled_tools,
        &fixture.project_root,
    );

    let result = run_turn(fixture.session_api, request).await.unwrap();
    assert_eq!(result.status, RunTurnStatus::Completed);

    let entry = last_log_entry(fixture.store.as_ref(), &result.session_id);
    assert_eq!(entry.provider, "openai_compatible");
}

#[tokio::test]
async fn run_turn_commits_parent_from_initial_transcript() {
    let fixture = setup_with_events(happy_events(), &[]);
    let first = run_turn(
        fixture.session_api,
        with_test_llm(
            make_request("first turn"),
            fixture.llm.clone(),
            &fixture.enabled_tools,
            &fixture.project_root,
        ),
    )
    .await
    .unwrap();
    let first_commit_id = first.commit_id.clone();
    let session_id = first.session_id.clone();

    let session_api = SessionApi::sqlite(&fixture.sessions_path);
    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let mut request = make_request_with_session("second turn", &session_id);
    request.test_llm = Some(llm);
    request.test_project_root = Some(fixture.project_root.clone());

    let result = run_turn(session_api, request).await.unwrap();

    assert_eq!(result.status, RunTurnStatus::Completed);
    let entry = last_log_entry(fixture.store.as_ref(), &result.session_id);
    assert_eq!(entry.parent_ids, vec![first_commit_id]);
}
