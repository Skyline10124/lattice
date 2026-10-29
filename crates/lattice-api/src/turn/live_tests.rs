// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{execute::start_turn, test_support::*, types::*};
use crate::session::ApiError;
use lattice_agent::testing::FakeStreamingLlmClient;
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
                    provider: "openai-compatible".into(),
                    system_prompt: None,
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
async fn wrong_workspace_is_rejected_before_acceptance_or_provider() {
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
    std::fs::remove_dir_all(fixture.layout.project_store(
        &fixture.root.store_key(),
        lancet_runtime::history::HISTORY_REPO_DIR,
    ))
    .unwrap();
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
async fn orphaned_turn_allows_send_without_mutating_old_facts_or_events() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let (sid, old) = seed(&fixture);
    let before = fixture
        .api
        .read(move |read| read.turn_detail(sid, old))
        .await
        .unwrap();
    assert_eq!(before.summary.progress, StepProgress::Reserved);
    let mut request = fixture.request.clone();
    request.session_id = Some(sid);
    request.agent = None;
    let (new, events) = fixture.run(request).await;
    assert_ne!(new.turn_id, old);
    let read = fixture.store.begin_read().unwrap();
    assert_eq!(read.turn_detail(sid, old).unwrap(), before);
    assert_eq!(llm.call_count(), 1);
    assert_eq!(read.log(sid).unwrap().nodes.len(), 5);
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
async fn used_turn_conflicts_before_configuration_and_never_runs_twice() {
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
    );
    assert!(matches!(
        execution.acceptance.await.unwrap(),
        Err(ApiError::Conflict(
            crate::session::RequestConflict::TurnIdUsed
        ))
    ));
    assert!(execution.completion.await.unwrap().is_err());
    retry.input.push(' ');
    let conflict = start_turn(
        fixture.api.clone(),
        fixture.layout.clone(),
        first.turn_id,
        fixture.root.clone(),
        retry,
    );
    assert!(matches!(
        conflict.acceptance.await.unwrap(),
        Err(ApiError::Conflict(
            crate::session::RequestConflict::TurnIdUsed
        ))
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
    );
    let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("accepted")
    };
    execution.completion.await.unwrap().unwrap();
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

#[tokio::test]
async fn cancelled_before_first_request_closes_reserved_step_without_attempt() {
    let llm = Arc::new(FakeStreamingLlmClient::builder().build());
    let fixture = setup_fixture(llm.clone(), &[]);
    let mut request = fixture.request.clone();
    request.cancel_flag = Some(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    let (detail, events) = fixture.run(request).await;
    assert_eq!(llm.call_count(), 0);
    assert_eq!(detail.steps.len(), 1);
    assert!(detail.steps[0].attempts.is_empty());
    assert_eq!(
        detail.steps[0].end,
        StepEnd::Stop {
            cause: StopCause::Cancelled
        }
    );
    assert!(detail.summary.progress.is_closed());
    assert!(
        matches!(events.last(), Some(TurnEvent::TurnCancelled { step, .. }) if step == &detail.steps[0])
    );
}

#[tokio::test]
async fn continue_at_library_step_limit_closes_reserved_step_32_without_request() {
    use lattice_conversation::*;
    use lattice_llm::testing::ScriptEvent;
    let mut builder = FakeStreamingLlmClient::builder();
    for _ in 0..MAX_MODEL_STEPS {
        let message = NativeMessage::new(
            MessageRole::Assistant,
            Some(lattice_conversation::ReplayOrigin {
                protocol: Protocol::ChatCompletions,
                provider_id: "local".into(),
            }),
            vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: UnboundToolCall {
                    provider_call_id: Some("read".into()),
                    name: "observe__workspace".into(),
                    arguments: serde_json::json!({
                        "request": {"mode": "read_file", "path": "src/lib.rs"}
                    }),
                },
            })],
        );
        builder = builder.push_events(
            lattice_llm::testing::parts_events(message.parts)
                .into_iter()
                .chain([ScriptEvent::Done {
                    finish_reason: "tool_calls".into(),
                    usage: None,
                }])
                .collect(),
        );
    }
    let llm = Arc::new(builder.build());
    let fixture = setup_fixture(llm.clone(), &["observe__workspace"]);
    let (detail, events) = fixture.run(fixture.request.clone()).await;
    assert_eq!(llm.call_count(), MAX_MODEL_STEPS as usize);
    assert_eq!(detail.steps.len(), MAX_MODEL_STEPS as usize + 1);
    for step in &detail.steps[..MAX_MODEL_STEPS as usize] {
        assert_eq!(step.end, StepEnd::Continue);
        assert!(step.progress.is_closed());
    }
    let limit = detail.steps.last().unwrap();
    assert_eq!(limit.key.ordinal, MAX_MODEL_STEPS);
    assert!(limit.attempts.is_empty());
    assert!(limit.slots.is_empty());
    assert_eq!(
        limit.end,
        StepEnd::Stop {
            cause: StopCause::ModelStepLimit
        }
    );
    assert_eq!(detail.summary.progress, limit.progress);
    assert!(events.iter().all(|event| !matches!(event,
        TurnEvent::ModelStarted { key, .. } if key.model_step_ordinal >= MAX_MODEL_STEPS)));
    assert!(events.iter().any(|event| matches!(event,
        TurnEvent::StepClosed { step } if step.key.ordinal == MAX_MODEL_STEPS - 1)));
    assert!(
        matches!(events.last(), Some(TurnEvent::TurnFailed { step, summary, .. }) if step == limit && summary == &detail.summary)
    );
}

#[tokio::test]
async fn raced_existing_turn_conflicts_even_when_readback_session_does_not_exist() {
    let llm = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let fixture = setup_fixture(llm.clone(), &[]);
    let turn_id = id();
    let observer = Arc::new(super::observer::ObserverSink::default());
    let (entered, entry) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    *observer.before_accept.lock().unwrap() = Some((entered, resume));
    let execution = super::execute::start_execution(super::execute::ExecutionInput {
        session: fixture.api.clone(),
        layout: fixture.layout.clone(),
        turn_id,
        workspace: fixture.root.clone(),
        request: fixture.request.clone(),
        prepared_model: None,
        shutdown: Arc::default(),
        observer,
        new_session: Some(crate::local::new_session_id()),
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), entry)
        .await
        .unwrap()
        .unwrap();
    // 另一个 Session 的提交在前置查重之后获胜；输家 Session 尚未创建，读回必定失败。
    let winner = start_turn(
        fixture.api.clone(),
        fixture.layout.clone(),
        turn_id,
        fixture.root.clone(),
        fixture.request.clone(),
    );
    assert!(matches!(
        winner.acceptance.await.unwrap().unwrap(),
        TurnAcceptance::Accepted { .. }
    ));
    winner.completion.await.unwrap().unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        execution.acceptance.await.unwrap(),
        Err(ApiError::Conflict(
            crate::session::RequestConflict::TurnIdUsed
        ))
    ));
    assert!(matches!(
        execution.completion.await.unwrap(),
        Err(ApiError::Conflict(
            crate::session::RequestConflict::TurnIdUsed
        ))
    ));
    assert_eq!(llm.call_count(), 1);
}
