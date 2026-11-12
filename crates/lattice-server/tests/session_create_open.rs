// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session 由 create_session 显式创建；首轮接受、重开及同身份冲突保持冻结配置和原始输入。
use lattice_llm::testing::ScriptEvent;
use lattice_local::{AgentName, ConflictReason, new_session_id, new_turn_id};
use lattice_server::{RunTurnRequest, SessionApi, StateLayout, TurnAcceptance, TurnCompletion};
use lattice_session::store::{EffectiveProfile, PublicTurnOutcome, SessionSettings, SessionStore};
use std::sync::Arc;

#[tokio::test]
async fn first_acceptance_reopen_preserves_identity_without_observers_or_configuration() {
    let state = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let layout = StateLayout::in_state_root(state.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let api = SessionApi::from_store(store.clone());
    let paths = lattice_profile::DiscoveryPaths {
        project_root: project.path().to_path_buf(),
        global_agents_dir: state.path().join("global-agents"),
    };
    let profile = paths.project_agents_dir().join("alpha/agent.toml");
    std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
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
    let uid =
        std::os::unix::fs::MetadataExt::uid(&std::fs::symlink_metadata(project.path()).unwrap());
    let alpha = AgentName::parse("alpha").unwrap();
    let loaded_profile = lattice_profile::load_agent_profile(&alpha, &paths, uid).unwrap();
    let root = lattice_server::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let session_id = new_session_id();
    let settings = SessionSettings {
        profile: EffectiveProfile {
            agent_name: loaded_profile.profile.agent.name,
            description: loaded_profile.profile.agent.description,
            default_model: loaded_profile.profile.agent.model.clone(),
            default_effort: loaded_profile.profile.agent.effort,
            timeout_secs: loaded_profile.profile.llm.timeout_secs,
            retry: loaded_profile.profile.llm.retry,
        },
        system_prompt: Some(loaded_profile.system_prompt),
        model: loaded_profile.profile.agent.model,
        effort: loaded_profile.profile.agent.effort,
    };
    store
        .create_session(
            &store.try_lock_session(session_id).unwrap(),
            root.project_path(),
            settings,
            chrono::Utc::now(),
        )
        .unwrap();
    let request = RunTurnRequest {
        input: " original \n".into(),
        session_id,
        expected_settings: None,
        cancel_flag: None,
        tools: None,
        test_llm: Some((
            llm.clone(),
            lattice_session::store::AttemptProtocol::ChatCompletions,
            vec![],
        )),
    };
    let id = new_turn_id();
    let execution = lattice_server::start_turn(api.clone(), id, root.clone(), request.clone());
    let TurnAcceptance::Accepted { turn } = execution.acceptance.await.unwrap().unwrap() else {
        panic!("not accepted")
    };
    assert_eq!(turn.session_id, session_id);
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
    retry.test_llm = None;
    let execution = lattice_server::start_turn(api, id, root, retry);
    assert!(matches!(
        execution.acceptance.await.unwrap(),
        Err(lattice_server::ApiError::Conflict(
            ConflictReason::TurnIdUsed
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
