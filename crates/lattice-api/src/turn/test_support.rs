// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::WorkspaceSection;
use lattice_session::SessionGraphStore;
use lattice_workspace::testing::DeterministicWorkspace;

use super::types::RunTurnRequest;
use super::upper::upper_backed_tools_enabled;
use crate::session::SessionApi;

pub(super) fn make_test_run_turn_request(
    llm: Arc<dyn LlmClient>,
    in_memory_audit: bool,
    workspace_override: Option<Arc<dyn lattice_workspace::Workspace>>,
    enabled_tools: &[String],
) -> RunTurnRequest {
    RunTurnRequest {
        input: "test".to_string(),
        session_id: TEST_SESSION_ID.to_string(),
        loaded_profile: Some(make_loaded_profile_with_tools(
            "sonnet",
            "System from profile.",
            enabled_tools,
        )),
        cancel_flag: None,
        test_llm: Some(llm),
        test_project_root: None,
        test_persist_audit: if in_memory_audit { None } else { Some(false) },
        workspace_override,
    }
}

pub(super) async fn build_test_production_deps(
    project_root: &str,
    enabled_tools: &[String],
    llm: Arc<dyn LlmClient>,
    in_memory_audit: bool,
    lancet: Option<&lattice_profile::LancetSection>,
    workspace_override: Option<Arc<dyn lattice_workspace::Workspace>>,
) -> super::cep::ProductionTurnDeps {
    let request = make_test_run_turn_request(
        llm.clone(),
        in_memory_audit,
        workspace_override,
        enabled_tools,
    );
    let loaded = request.loaded_profile.as_ref().unwrap();
    super::cep::build_production_turn_deps(super::cep::ProductionTurnDepsInput {
        request: &request,
        project_root,
        enabled_tools,
        llm,
        lancet,
        model_id: "test-model",
        runtime_version: "0.0.0-test",
        loaded_profile: loaded,
    })
    .await
    .expect("production deps")
}

pub(super) const TEST_SESSION_ID: &str = "test-session";

pub(super) struct TestFixture {
    pub(super) _dir: tempfile::TempDir,
    pub(super) project_root: String,
    pub(super) sessions_path: std::path::PathBuf,
    pub(super) session_api: SessionApi,
    pub(super) store: Box<dyn SessionGraphStore>,
    pub(super) llm: Arc<dyn LlmClient>,
    pub(super) enabled_tools: Vec<String>,
}

pub(super) fn happy_events() -> Vec<LlmEvent> {
    vec![
        LlmEvent::Token {
            content: "test response".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
    ]
}

pub(super) fn setup_fixture(llm: Arc<dyn LlmClient>, enabled_tools: &[&str]) -> TestFixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn hello() {}").unwrap();
    init_test_git_repo(&root);

    let sessions_path = root.join("sessions.git");
    std::fs::create_dir_all(&sessions_path).unwrap();
    let session_api = SessionApi::git(&sessions_path);
    let store: Box<dyn SessionGraphStore> = Box::new(lattice_session::GitSessionGraphStore::new(&sessions_path));
    store
        .create_session(TEST_SESSION_ID, None, "test", "main")
        .expect("create default test session");

    TestFixture {
        _dir: dir,
        project_root: root.to_string_lossy().to_string(),
        sessions_path,
        session_api,
        store,
        llm,
        enabled_tools: enabled_tools.iter().map(|s| s.to_string()).collect(),
    }
}

pub(super) fn setup_with_events(events: Vec<LlmEvent>, enabled_tools: &[&str]) -> TestFixture {
    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(events)
            .build(),
    );
    setup_fixture(llm, enabled_tools)
}

pub(super) fn make_request(input: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: TEST_SESSION_ID.to_string(),
        loaded_profile: Some(make_loaded_profile("sonnet", "System from profile.")),
        cancel_flag: None,
        test_llm: None,
        test_project_root: None,
        test_persist_audit: None,
        workspace_override: None,
    }
}

pub(super) fn make_request_with_session(input: &str, session_id: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: session_id.to_string(),
        loaded_profile: Some(make_loaded_profile("sonnet", "System from profile.")),
        cancel_flag: None,
        test_llm: None,
        test_project_root: None,
        test_persist_audit: None,
        workspace_override: None,
    }
}

pub(super) fn make_loaded_profile(
    model: &str,
    system_prompt: &str,
) -> lattice_profile::LoadedAgentProfile {
    make_loaded_profile_with_tools(model, system_prompt, &[])
}

pub(super) fn make_loaded_profile_with_tools(
    model: &str,
    system_prompt: &str,
    enabled_tools: &[String],
) -> lattice_profile::LoadedAgentProfile {
    let workspace = upper_backed_tools_enabled(enabled_tools).then(|| WorkspaceSection {
        egress: None,
        credentials: vec![],
        image: Some("test-template".to_string()),
        cpus: None,
        memory_mb: None,
        allowed_egress_targets: vec![],
        cleanup_rounds: 3,
        max_pending_snapshots: 5,
    });
    lattice_profile::LoadedAgentProfile {
        path: std::path::PathBuf::from("/tmp/test/agent.toml"),
        package_dir: std::env::current_dir().unwrap_or_else(|_| "/tmp".into()),
        profile: lattice_profile::AgentProfile {
            schema_version: 1,
            agent: lattice_profile::AgentSection {
                name: "test".to_string(),
                model: model.to_string(),
                description: None,
            },
            prompt: lattice_profile::PromptSection {
                system: Some(system_prompt.to_string()),
                system_path: None,
            },
            tools: lattice_profile::ToolsSection {
                enabled: enabled_tools.to_vec(),
            },
            llm: lattice_profile::LlmSection::default(),
            lancet: lattice_profile::LancetSection::default(),
            workspace,
        },
        system_prompt: system_prompt.to_string(),
    }
}

fn init_test_git_repo(root: &std::path::Path) {
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .expect("git init for test fixture");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@lattice.local"])
        .current_dir(root)
        .status()
        .expect("git user.email");
    std::process::Command::new("git")
        .args(["config", "user.name", "lattice-test"])
        .current_dir(root)
        .status()
        .expect("git user.name");
    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(root)
        .status()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-q", "-m", "init"])
        .current_dir(root)
        .status()
        .expect("git commit");
}

fn test_workspace_override() -> Arc<dyn lattice_workspace::Workspace> {
    Arc::new(DeterministicWorkspace::new().expect("deterministic workspace for upper-backed tests"))
}

pub(super) fn with_test_llm(
    mut request: RunTurnRequest,
    llm: Arc<dyn LlmClient>,
    enabled_tools: &[String],
    project_root: &str,
) -> RunTurnRequest {
    request.test_llm = Some(llm);
    request.test_project_root = Some(project_root.to_string());
    if request.loaded_profile.is_none() || !enabled_tools.is_empty() {
        request.loaded_profile = Some(make_loaded_profile_with_tools(
            "sonnet",
            "System from profile.",
            enabled_tools,
        ));
    }
    if upper_backed_tools_enabled(enabled_tools) && request.workspace_override.is_none() {
        request.workspace_override = Some(test_workspace_override());
    }
    request
}

pub(super) fn last_log_entry(
    store: &dyn SessionGraphStore,
    session_id: &str,
) -> lattice_session::TurnLogEntry {
    store
        .log(session_id)
        .expect("log after commit")
        .last()
        .expect("commit log entry")
        .clone()
}

pub(super) struct CancelOnDoneLlm {
    cancel: Arc<AtomicBool>,
    pre_done_events: Mutex<Option<Vec<LlmEvent>>>,
    finish_reason: String,
    usage: Option<TokenUsage>,
}

impl CancelOnDoneLlm {
    pub(super) fn new(
        cancel: Arc<AtomicBool>,
        pre_done_events: Vec<LlmEvent>,
        finish_reason: &str,
        usage: Option<TokenUsage>,
    ) -> Self {
        Self {
            cancel,
            pre_done_events: Mutex::new(Some(pre_done_events)),
            finish_reason: finish_reason.to_string(),
            usage,
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for CancelOnDoneLlm {
    async fn chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        unreachable!("stream-only fake")
    }

    async fn stream_chat(
        &self,
        _request: lattice_llm::ChatRequest,
    ) -> Result<lattice_llm::LlmStream, Box<dyn std::error::Error + Send + Sync>> {
        let pre = self
            .pre_done_events
            .lock()
            .unwrap()
            .take()
            .expect("CancelOnDoneLlm only supports a single round");
        let cancel = self.cancel.clone();
        let finish_reason = self.finish_reason.clone();
        let usage = self.usage.clone();

        enum StreamState {
            Pending(
                VecDeque<LlmEvent>,
                String,
                Option<TokenUsage>,
                Arc<AtomicBool>,
            ),
            DoneEmitted,
        }

        let initial = StreamState::Pending(VecDeque::from(pre), finish_reason, usage, cancel);
        let stream = futures::stream::unfold(initial, |state| async move {
            match state {
                StreamState::Pending(mut queue, finish_reason, usage, cancel) => {
                    if let Some(event) = queue.pop_front() {
                        Some((
                            event,
                            StreamState::Pending(queue, finish_reason, usage, cancel),
                        ))
                    } else {
                        cancel.store(true, Ordering::Relaxed);
                        let done = LlmEvent::Done {
                            finish_reason,
                            usage,
                        };
                        Some((done, StreamState::DoneEmitted))
                    }
                }
                StreamState::DoneEmitted => None,
            }
        });

        Ok(Box::pin(stream))
    }
}
