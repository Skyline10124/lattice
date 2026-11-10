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
struct StaticTestTools {
    specs: Vec<crate::ToolSpec>,
}

impl StaticTestTools {
    fn from_names(names: &[&str]) -> Arc<dyn crate::ToolExecutor> {
        if names.is_empty() {
            return Arc::new(crate::NoTools);
        }
        Arc::new(Self {
            specs: names
                .iter()
                .map(|name| crate::ToolSpec {
                    name: (*name).to_owned(),
                    description: format!("测试工具 {name}"),
                    input_schema: serde_json::json!({ "type": "object" }),
                    tool_identity: format!("test:{name}@1"),
                })
                .collect(),
        })
    }
}

#[async_trait::async_trait]
impl crate::ToolExecutor for StaticTestTools {
    async fn open_turn(
        &self,
        _session_id: lattice_conversation::SessionId,
        _workspace: &crate::LocalWorkspaceRoot,
    ) -> crate::session::Result<Box<dyn crate::TurnTools>> {
        Ok(Box::new(Self {
            specs: self.specs.clone(),
        }))
    }
}

#[async_trait::async_trait]
impl crate::TurnTools for StaticTestTools {
    fn specs(&self) -> &[crate::ToolSpec] {
        &self.specs
    }

    async fn execute(
        &self,
        _started: lattice_session::store::StartedInvocation,
        name: &str,
        _arguments: serde_json::Value,
        _cancel: Arc<std::sync::atomic::AtomicBool>,
        _deadline: tokio::time::Instant,
    ) -> crate::ToolOutcome {
        crate::ToolOutcome::Succeeded(serde_json::json!({ "tool": name, "ok": true }))
    }
}

pub(super) fn setup_fixture(llm: Arc<dyn LlmClient>, enabled_tools: &[&str]) -> TestFixture {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/lib.rs"), "pub fn hello() {}").unwrap();
    let paths = lattice_profile::DiscoveryPaths {
        project_root: project.path().to_path_buf(),
        global_agents_dir: state.path().join("global-agents"),
    };
    let path = paths.project_agents_dir().join("test/agent.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        "schema_version=1\n[agent]\nname='test'\nmodel='local/sonnet'\n[prompt]\nsystem='System from profile.'\n",
    )
    .unwrap();
    let layout = crate::StateLayout::in_state_root(state.path());
    let store = Arc::new(SessionStore::create(&layout).unwrap());
    let root = crate::LocalWorkspaceRoot::from_path(project.path()).unwrap();
    let agent = crate::AgentName::parse("test").unwrap();
    let loaded_profile =
        lattice_profile::load_agent_profile(&agent, &paths, crate::local::ipc::uid()).unwrap();
    let request = RunTurnRequest {
        model: None,
        effort: None,
        input: "hello".into(),
        session_id: None,
        agent: Some(agent),
        loaded_profile: Some(loaded_profile),
        cancel_flag: None,
        tools: Some(StaticTestTools::from_names(enabled_tools)),
        test_llm: Some((
            llm,
            lattice_session::store::AttemptProtocol::ChatCompletions,
            vec![],
        )),
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
            turn_id: lattice_conversation::TurnId::parse(&uuid::Uuid::new_v4().to_string())
                .unwrap(),
            workspace: self.root.clone(),
            request,
            prepared_model: None,
            prepared_profile: None,
            io: None,
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
