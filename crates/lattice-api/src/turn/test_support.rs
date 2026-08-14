// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_session::SessionGraphStore;

use super::types::RunTurnRequest;
use crate::session::SessionApi;

pub(super) fn make_test_run_turn_request(
    llm: Arc<dyn LlmClient>,
    in_memory_audit: bool,
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
    }
}

pub(super) async fn build_test_production_deps(
    project_root: &str,
    enabled_tools: &[String],
    llm: Arc<dyn LlmClient>,
    in_memory_audit: bool,
    lancet: Option<&lattice_profile::LancetSection>,
) -> super::cep::ProductionTurnDeps {
    // Ensure a git repo exists so the canonical project root resolves and
    // the project-shaped test fixtures match the suite's other fixtures.
    let root = std::path::Path::new(project_root);
    if !root.join(".git").exists() {
        std::fs::write(root.join("README.md"), "init\n").expect("write README");
        init_test_git_repo(root);
    }
    if !in_memory_audit {
        // A durable-audit fixture takes the production path, which strictly
        // opens the project's stores; create them first, as `session new`
        // does.
        ensure_project_stores(root);
    }
    let request = make_test_run_turn_request(llm.clone(), in_memory_audit, enabled_tools);
    super::cep::build_production_turn_deps(super::cep::ProductionTurnDepsInput {
        request: &request,
        project_root,
        enabled_tools,
        llm,
        lancet,
        model_id: "test-model",
        runtime_version: "0.0.0-test",
    })
    .await
    .expect("production deps")
}

pub(super) const TEST_SESSION_ID: &str = "test-session";

/// The store key binding test sessions to the fixture's project root.
pub(super) fn test_project_key(root: &std::path::Path) -> lattice_session::ProjectStoreKey {
    crate::root::LocalWorkspaceRoot::from_path(root)
        .expect("resolve fixture workspace root")
        .store_key()
}

/// First-create the project's durable stores for `project_root`, the way
/// `session new` does in production (#143 — a Turn never creates them).
///
/// Callers must already have `XDG_STATE_HOME` pointed at a test-owned root.
pub(super) fn ensure_project_stores(project_root: &std::path::Path) {
    let layout = lattice_session::StateLayout::resolve().expect("resolve state layout");
    crate::stores::create_project_stores(&layout, test_project_key(project_root))
        .expect("create fixture project stores");
}

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
    let session_api = SessionApi::git_create(&sessions_path).expect("create session store");
    let store: Box<dyn SessionGraphStore> = Box::new(
        lattice_session::GitSessionGraphStore::open(&sessions_path).expect("open session store"),
    );
    store
        .create_session(
            TEST_SESSION_ID,
            None,
            "test",
            "main",
            &test_project_key(&root),
        )
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
        },
        system_prompt: system_prompt.to_string(),
    }
}

fn init_test_git_repo(root: &std::path::Path) {
    let repo = lattice_session::test_utils::init_empty_repo(root);
    lattice_session::test_utils::commit_all(&repo, root, "init");
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
