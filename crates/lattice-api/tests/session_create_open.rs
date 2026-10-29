// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session 在首次接受时原子创建；重开及同身份冲突保持冻结配置和原始输入。
use lattice_api::local::*;
use lattice_api::{RunTurnRequest, SessionApi, StateLayout, TurnAcceptance, TurnCompletion};
use lattice_llm::testing::ScriptEvent;
use lattice_session::store::{PublicTurnOutcome, SessionStore};
use std::sync::Arc;

#[tokio::test]
async fn first_acceptance_reopen_preserves_identity_without_observers_or_configuration() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let api = SessionApi::from_store(store.clone());
    let profile = project.path().join("agent.toml");
    std::fs::write(
        &profile,
        "schema_version=1\n[agent]\nname='alpha'\nmodel='local/test'\n[prompt]\nsystem='frozen'\n",
    )
    .unwrap();
    let llm = Arc::new(
        lattice_agent::testing::FakeStreamingLlmClient::builder()
            .push_events(vec![
                ScriptEvent::Text {
                    content: "answer".into(),
                },
                ScriptEvent::Done {
                    finish_reason: "stop".into(),
                    usage: None,
                },
            ])
            .build(),
    );
    let request = RunTurnRequest {
        input: " original \n".into(),
        session_id: None,
        agent: Some("alpha".into()),
        loaded_profile: Some(lattice_profile::load_agent_profile(&profile).unwrap()),
        cancel_flag: None,
        test_llm: Some((
            llm.clone(),
            lattice_session::store::AttemptProtocol::ChatCompletions,
        )),
        test_project_root: None,
    };
    let root = lattice_api::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let id = new_turn_id();
    let execution = lattice_api::start_turn(
        api.clone(),
        layout.clone(),
        id,
        root.clone(),
        request.clone(),
    );
    let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("not accepted")
    };
    let session_id = turn.session_id;
    let TurnCompletion::Closed(completed) = execution.completion.await.unwrap().unwrap();
    assert_eq!(completed.summary.outcome, PublicTurnOutcome::Completed);
    drop(execution.cancel_flag);
    drop(api);
    drop(store);
    std::fs::write(
        &profile,
        "schema_version=1\n[agent]\nname='alpha'\nmodel='local/changed'\n[prompt]\nsystem='changed'\n",
    )
    .unwrap();
    let store = Arc::new(SessionStore::open(&layout).unwrap());
    let api = SessionApi::from_store(store.clone());
    let mut retry = request;
    retry.loaded_profile = None;
    retry.agent = None;
    retry.test_llm = None;
    let execution = lattice_api::start_turn(api, layout, id, root, retry);
    assert!(matches!(
        execution.acceptance.await.unwrap(),
        Err(lattice_api::ApiError::Conflict(
            lattice_api::local::ConflictReason::TurnIdUsed
        ))
    ));
    assert!(execution.completion.await.unwrap().is_err());
    assert_eq!(
        store
            .begin_read()
            .unwrap()
            .turn_detail(session_id, id)
            .unwrap(),
        *completed
    );
    assert_eq!(llm.call_count(), 1);
    let read = store.begin_read().unwrap();
    assert_eq!(read.list(None, 10).unwrap().items.len(), 1);
    assert_eq!(read.log(session_id).unwrap().nodes.len(), 3);
    assert_eq!(read.turn_input(session_id, id).unwrap(), " original \n");
    let config = read.turn_config(session_id, id).unwrap();
    assert_eq!(config.agent_name, "alpha");
    assert_eq!(config.model, "test");
    assert_eq!(config.system_prompt.as_deref(), Some("frozen"));
}
