// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use super::{observer::ObserverSink, types::*};
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::TokenUsage;
use lattice_llm::testing::ScriptEvent;
use lattice_session::store::SessionStore;
use std::sync::Arc;

pub(super) struct TestFixture {
    pub api: crate::SessionApi,
    pub store: Arc<SessionStore>,
    pub layout: crate::StateLayout,
    pub root: crate::LocalWorkspaceRoot,
    pub request: RunTurnRequest,
    // 数据库句柄先于临时目录释放。
    pub _state: tempfile::TempDir,
    pub _project: tempfile::TempDir,
}
pub(super) fn happy_events() -> Vec<ScriptEvent> {
    vec![
        ScriptEvent::Text {
            content: "test response".into(),
        },
        ScriptEvent::Done {
            finish_reason: "stop".into(),
            usage: Some(TokenUsage {
                input: Some(10),
                output: Some(5),
                ..Default::default()
            }),
        },
    ]
}
pub(super) fn setup_fixture(llm: Arc<dyn LlmClient>, enabled_tools: &[&str]) -> TestFixture {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn hello() {}").unwrap();
    let path = project.path().join("agent.toml");
    std::fs::write(&path, format!("schema_version=1\n[agent]\nname='test'\nmodel='local/sonnet'\n[prompt]\nsystem='System from profile.'\n[tools]\nenabled={enabled_tools:?}\n")).unwrap();
    let layout = crate::StateLayout::in_state_root(state.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let root = crate::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let request = RunTurnRequest {
        input: "hello".into(),
        session_id: None,
        agent: Some("test".into()),
        loaded_profile: Some(lattice_profile::load_agent_profile(&path).unwrap()),
        cancel_flag: None,
        test_llm: Some((
            llm,
            lattice_session::store::AttemptProtocol::ChatCompletions,
        )),
        test_project_root: Some(root.canonical_path().to_str().unwrap().into()),
    };
    TestFixture {
        api: crate::SessionApi::from_store(store.clone()),
        store,
        layout,
        root,
        request,
        _state: state,
        _project: project,
    }
}
pub(super) fn setup_with_events(events: Vec<ScriptEvent>, tools: &[&str]) -> TestFixture {
    setup_fixture(
        Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(events)
                .build(),
        ),
        tools,
    )
}
impl TestFixture {
    pub fn start(&self, request: RunTurnRequest) -> (TurnExecution, Arc<ObserverSink>) {
        let observer = Arc::new(ObserverSink::default());
        let execution = super::execute::start_execution(super::execute::ExecutionInput {
            session: self.api.clone(),
            layout: self.layout.clone(),
            turn_id: lancet_protocol::TurnId::parse(&uuid::Uuid::new_v4().to_string()).unwrap(),
            workspace: self.root.clone(),
            request,
            prepared_model: None,
            shutdown: Arc::default(),
            observer: observer.clone(),
            new_session: Some(crate::local::new_session_id()),
        });
        (execution, observer)
    }
    pub async fn run(
        &self,
        request: RunTurnRequest,
    ) -> (lattice_session::store::TurnDetail, Vec<TurnEvent>) {
        let (execution, observer) = self.start(request);
        execution.acceptance.await.unwrap().unwrap();
        let completed = execution.completion.await.unwrap().unwrap();
        let events = observer.events();
        let TurnCompletion::Closed(detail) = completed;
        (*detail, events)
    }
}
