// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{execute::start_turn, test_support::*, types::*};
use crate::session::ApiError;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::LlmEvent;
use lattice_session::store::*;
use std::sync::Arc;

fn id() -> lancet_protocol::TurnId {
    lancet_protocol::TurnId::parse(&uuid::Uuid::new_v4().to_string()).unwrap()
}
fn seed(fixture: &TestFixture) -> (lancet_protocol::SessionId, lancet_protocol::TurnId) {
    let sid =
        lancet_protocol::SessionId::parse(&uuid::Uuid::new_v4().simple().to_string()).unwrap();
    let turn = id();
    fixture
        .store
        .accept_turn(
            &fixture.store.try_lock_session(sid).unwrap(),
            AcceptTurn {
                session: AcceptSession::New { id: sid },
                turn_id: turn,
                project_store_key: fixture.root.store_key(),
                input: "old request".into(),
                accepted_at: chrono::Utc::now(),
                agent_selection: AgentSelection::Explicit,
                config: FrozenTurnConfig {
                    agent_name: "test".into(),
                    model: "old-model".into(),
                    provider: "openai_compatible".into(),
                    system_prompt: None,
                    profile_snapshot_json: "{}".into(),
                    policy_snapshot_json: "{}".into(),
                    catalog: ToolCatalogSnapshot::new("test".into(), vec![]).unwrap(),
                    limits: TurnExecutionLimits {
                        timeout_secs: 300,
                        max_model_steps: 32,
                        max_model_attempts: 1,
                    },
                },
            },
        )
        .unwrap();
    lancet_api::WorkspaceHistory::create(lancet_api::WorkspaceHistoryBacking::for_key(
        fixture.layout.clone(),
        fixture.root.store_key(),
    ))
    .unwrap();
    (sid, turn)
}

#[tokio::test]
async fn wrong_workspace_is_rejected_before_auto_close_or_provider() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let (sid, old) = seed(&fixture);
    let before = fixture
        .store
        .begin_read()
        .unwrap()
        .turn_detail(sid, old)
        .unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let sentinel = elsewhere.path().join("sentinel");
    std::fs::write(&sentinel, "untouched").unwrap();
    let mut request = fixture.request.clone();
    request.session_id = Some(sid);
    let execution = start_turn(
        fixture.api.clone(),
        fixture.layout.clone(),
        id(),
        crate::LocalWorkspaceRoot::from_path(elsewhere.path()).unwrap(),
        request,
        None,
    );
    assert!(matches!(
        execution.acceptance.await.unwrap(),
        Err(ApiError::Conflict(_))
    ));
    assert!(execution.completion.await.unwrap().is_err());
    assert_eq!(llm.call_count(), 0);
    assert_eq!(
        fixture
            .store
            .begin_read()
            .unwrap()
            .turn_detail(sid, old)
            .unwrap(),
        before
    );
    assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "untouched");
}

#[tokio::test]
async fn existing_session_requires_its_workspace_history_before_accepting_new_turn() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let (first, _) = fixture.run(fixture.request.clone()).await;
    let backing = lancet_api::WorkspaceHistoryBacking::for_key(
        fixture.layout.clone(),
        fixture.root.store_key(),
    );
    std::fs::remove_dir_all(backing.store_path().unwrap()).unwrap();
    let mut request = fixture.request.clone();
    request.session_id = Some(first.session_id);
    let (execution, _) = fixture.start(request);
    assert!(execution.acceptance.await.unwrap().is_err());
    assert!(execution.completion.await.unwrap().is_err());
    assert_eq!(llm.call_count(), 1);
    let read = fixture.store.begin_read().unwrap();
    assert_eq!(
        read.turn_detail(first.session_id, first.turn_id).unwrap(),
        first
    );
    assert_eq!(read.log(first.session_id).unwrap().nodes.len(), 3);
}

#[tokio::test]
async fn new_write_closes_old_turn_without_stealing_identity_or_events() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let (sid, old) = seed(&fixture);
    assert!(
        fixture
            .api
            .read(move |read| read.turn_detail(sid, old))
            .await
            .unwrap()
            .summary
            .is_none(),
        "read is closure-free"
    );
    let mut request = fixture.request.clone();
    request.session_id = Some(sid);
    request.agent = None;
    let (new, events) = fixture.run(request).await;
    assert_ne!(new.turn_id, old);
    let read = fixture.store.begin_read().unwrap();
    assert_eq!(
        read.turn_detail(sid, old).unwrap().summary.unwrap().outcome,
        PublicTurnOutcome::Failed
    );
    assert_eq!(llm.call_count(), 1);
    assert_eq!(read.log(sid).unwrap().nodes.len(), 4);
    let terminal: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::TurnCompleted { turn, .. }
            | TurnEvent::TurnFailed { turn, .. }
            | TurnEvent::TurnCancelled { turn, .. } => Some(turn.turn_id),
            _ => None,
        })
        .collect();
    assert_eq!(terminal, vec![new.turn_id]);
}

#[tokio::test]
async fn busy_session_rejects_promptly_while_another_session_can_execute() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let (sid, _) = seed(&fixture);
    let lock = fixture.store.try_lock_session(sid).unwrap();
    let mut request = fixture.request.clone();
    request.session_id = Some(sid);
    let (execution, _) = fixture.start(request);
    assert!(
        matches!(tokio::time::timeout(std::time::Duration::from_secs(1), execution.acceptance).await.unwrap().unwrap(), Err(ApiError::Store(error)) if matches!(error.as_ref(), StoreError::Busy { .. }))
    );
    assert!(execution.completion.await.unwrap().is_err());
    assert_eq!(llm.call_count(), 0);
    let (other, _) = fixture.run(fixture.request.clone()).await;
    assert_ne!(other.session_id, sid);
    assert_eq!(other.summary.unwrap().outcome, PublicTurnOutcome::Completed);
    assert_eq!(llm.call_count(), 1);
    drop(lock);
}

#[tokio::test]
async fn duplicate_turn_is_readonly_without_configuration_but_different_input_conflicts() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let (first, _) = fixture.run(fixture.request.clone()).await;
    let mut retry = fixture.request.clone();
    retry.loaded_profile = None;
    retry.agent = None;
    retry.test_llm = None;
    let execution = start_turn(
        fixture.api.clone(),
        fixture.layout.clone(),
        first.turn_id,
        fixture.root.clone(),
        retry.clone(),
        None,
    );
    assert!(
        matches!(execution.acceptance.await.unwrap().unwrap(), TurnAcceptance::Existing { turn } if turn == first)
    );
    assert_eq!(
        execution.completion.await.unwrap().unwrap(),
        TurnCompletion::Closed(Box::new(first.clone()))
    );
    retry.input.push(' ');
    let conflict = start_turn(
        fixture.api.clone(),
        fixture.layout.clone(),
        first.turn_id,
        fixture.root.clone(),
        retry,
        None,
    );
    assert!(matches!(
        conflict.acceptance.await.unwrap(),
        Err(ApiError::Conflict(_))
    ));
    assert!(conflict.completion.await.unwrap().is_err());
    assert_eq!(llm.call_count(), 1);
    let mut independent = fixture.request.clone();
    independent.session_id = Some(first.session_id);
    let (second, _) = fixture.run(independent).await;
    assert_ne!(second.turn_id, first.turn_id);
    assert_eq!(llm.call_count(), 2);
}

#[tokio::test]
async fn dropped_and_slow_observers_do_not_cancel_or_truncate_the_durable_reply() {
    for keep_observer in [false, true] {
        let mut events: Vec<_> = (0..400)
            .map(|i| LlmEvent::Token {
                content: format!("tok{i}"),
            })
            .collect();
        events.push(LlmEvent::Done {
            finish_reason: "stop".into(),
            usage: None,
        });
        let fixture = setup_with_events(events, &[]);
        let (execution, observer) = fixture.start(fixture.request.clone());
        let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
            panic!("accepted")
        };
        let retained = keep_observer.then_some(observer);
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), execution.completion)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(result, TurnCompletion::Closed(detail) if detail.summary.as_ref().unwrap().outcome == PublicTurnOutcome::Completed)
        );
        let Node::Step { content, .. } = fixture
            .store
            .begin_read()
            .unwrap()
            .node(NodeRef {
                session_id: turn.session_id,
                node: NodeId(2),
            })
            .unwrap()
        else {
            panic!("step")
        };
        let expected = (0..400).map(|i| format!("tok{i}")).collect::<String>();
        assert!(
            matches!(&content.assistant.parts[0].semantic, lattice_conversation::MessageSemantic::Text { text } if text == &expected)
        );
        if let Some(observer) = retained {
            assert!(
                observer.events_after(0).is_none(),
                "lag needs snapshot, not incomplete deltas"
            );
        }
    }
}

#[tokio::test]
async fn absent_session_fails_before_startup_and_injected_store_remains_authoritative() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let mut invalid = fixture.request.clone();
    invalid.session_id =
        Some(lancet_protocol::SessionId::parse("00000000000000000000000000000001").unwrap());
    let (execution, _) = fixture.start(invalid);
    assert!(
        matches!(execution.acceptance.await.unwrap(), Err(ApiError::Store(error)) if matches!(error.as_ref(), StoreError::NotFound { .. }))
    );
    assert!(execution.completion.await.unwrap().is_err());
    assert_eq!(llm.call_count(), 0);
    let other = tempfile::tempdir().unwrap();
    let layout = crate::StateLayout::in_state_root(other.path());
    let execution = start_turn(
        fixture.api.clone(),
        layout.clone(),
        id(),
        fixture.root.clone(),
        fixture.request.clone(),
        None,
    );
    let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("accepted")
    };
    assert!(matches!(
        execution.completion.await.unwrap().unwrap(),
        TurnCompletion::Closed(_)
    ));
    assert_eq!(
        fixture
            .store
            .begin_read()
            .unwrap()
            .log(turn.session_id)
            .unwrap()
            .nodes
            .len(),
        3
    );
    assert!(
        matches!(
            SessionStore::open(&layout),
            Err(StoreError::NotInitialized { .. })
        ),
        "the execution must not silently reopen a layout-default SessionStore"
    );
    assert_eq!(llm.call_count(), 1);
}
