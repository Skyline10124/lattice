use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::{AgentLoop, AgentLoopConfig, LlmClient};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_session::{SessionGraphStore, SqliteSessionGraphStore, TurnCommitStatus, TurnUsage};

use super::execute::{run_turn, stream_turn};
use super::types::{RunTurnRequest, RunTurnStage, RunTurnStatus, TurnEvent, TurnUsageSummary};
use crate::session::{CreateSessionRequest, SessionApi};

struct TestFixture {
    _dir: tempfile::TempDir,
    sessions_path: std::path::PathBuf,
    session_api: SessionApi,
    store: SqliteSessionGraphStore,
    agent_loop: Arc<AgentLoop>,
}

fn happy_events() -> Vec<LlmEvent> {
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

fn setup_fixture(llm: Arc<dyn LlmClient>, enabled_tools: &[&str]) -> TestFixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn hello() {}").unwrap();

    let sessions_path = root.join("sessions");
    let session_api = SessionApi::sqlite(&sessions_path);
    let store = SqliteSessionGraphStore::new(&sessions_path);

    let enabled: Vec<String> = enabled_tools.iter().map(|s| s.to_string()).collect();
    let executor = lattice_tools::executor::DefaultToolExecutor::for_project_root(
        &root.to_string_lossy(),
        enabled,
    )
    .expect("executor creation must succeed");

    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: Arc::new(executor),
    }));

    TestFixture {
        _dir: dir,
        sessions_path,
        session_api,
        store,
        agent_loop,
    }
}

fn setup_with_events(events: Vec<LlmEvent>, enabled_tools: &[&str]) -> TestFixture {
    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(events)
            .build(),
    );
    setup_fixture(llm, enabled_tools)
}

fn make_request(input: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: None,
        profile: None,
        model: Some("sonnet".to_string()),
        loaded_profile: None,
        cancel_flag: None,
    }
}

fn make_request_with_session(input: &str, session_id: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: Some(session_id.to_string()),
        profile: None,
        model: Some("sonnet".to_string()),
        loaded_profile: None,
        cancel_flag: None,
    }
}

fn make_loaded_profile(model: &str, system_prompt: &str) -> lattice_profile::LoadedAgentProfile {
    lattice_profile::LoadedAgentProfile {
        path: std::path::PathBuf::from("/tmp/test/agent.toml"),
        package_dir: std::path::PathBuf::from("/tmp/test"),
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
            tools: lattice_profile::ToolsSection::default(),
        },
        system_prompt: system_prompt.to_string(),
    }
}

fn last_log_entry(
    store: &SqliteSessionGraphStore,
    session_id: &str,
) -> lattice_session::TurnLogEntry {
    store
        .log(session_id)
        .expect("log after commit")
        .last()
        .expect("commit log entry")
        .clone()
}

struct CancelOnDoneLlm {
    cancel: Arc<AtomicBool>,
    pre_done_events: Mutex<Option<Vec<LlmEvent>>>,
    finish_reason: String,
    usage: Option<TokenUsage>,
}

impl CancelOnDoneLlm {
    fn new(
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

        enum Phase {
            Pending(
                VecDeque<LlmEvent>,
                String,
                Option<TokenUsage>,
                Arc<AtomicBool>,
            ),
            DoneEmitted,
        }

        let initial = Phase::Pending(VecDeque::from(pre), finish_reason, usage, cancel);
        let stream = futures::stream::unfold(initial, |state| async move {
            match state {
                Phase::Pending(mut queue, finish_reason, usage, cancel) => {
                    if let Some(event) = queue.pop_front() {
                        Some((event, Phase::Pending(queue, finish_reason, usage, cancel)))
                    } else {
                        cancel.store(true, Ordering::Relaxed);
                        let done = LlmEvent::Done {
                            finish_reason,
                            usage,
                        };
                        Some((done, Phase::DoneEmitted))
                    }
                }
                Phase::DoneEmitted => None,
            }
        });

        Ok(Box::pin(stream))
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn run_turn_auto_creates_session_and_returns_session_id() {
    let fixture = setup_with_events(happy_events(), &[]);
    let result = run_turn(
        fixture.session_api,
        fixture.agent_loop,
        make_request("hello"),
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
        fixture.agent_loop,
        make_request("test input"),
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
            profile: None,
            model: Some("sonnet".to_string()),
        })
        .unwrap();
    let session_id = created.session_id.clone();

    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(happy_events())
            .build(),
    );
    let enabled: Vec<String> = vec![];
    let executor = lattice_tools::executor::DefaultToolExecutor::for_project_root(
        &fixture._dir.path().to_string_lossy(),
        enabled,
    )
    .unwrap();
    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: Arc::new(executor),
    }));

    let result = run_turn(
        fixture.session_api,
        agent_loop,
        make_request_with_session("second turn", &session_id),
    )
    .await
    .unwrap();

    assert_eq!(result.session_id, session_id);
    assert_eq!(fixture.store.log(&session_id).unwrap().len(), 1);
}

#[tokio::test]
async fn run_turn_result_contains_contract_fields() {
    let fixture = setup_with_events(happy_events(), &[]);
    let result = run_turn(
        fixture.session_api,
        fixture.agent_loop,
        make_request("hello"),
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
        fixture.agent_loop,
        make_request("hello"),
    )
    .await
    .unwrap();

    assert_eq!(result.status, RunTurnStatus::Error);
    assert_eq!(result.assistant_output, "");
    let entry = last_log_entry(&fixture.store, &result.session_id);
    assert_eq!(entry.status, TurnCommitStatus::Error);
    assert!(entry
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("provider unavailable"));
}

#[tokio::test]
async fn run_turn_cancelled_outcome_is_committed() {
    let cancel = Arc::new(AtomicBool::new(false));
    let llm: Arc<dyn LlmClient> = Arc::new(CancelOnDoneLlm::new(
        cancel.clone(),
        vec![
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
        ],
        "tool_calls",
        Some(TokenUsage {
            prompt_tokens: 5,
            completion_tokens: 2,
            total_tokens: 7,
        }),
    ));
    let fixture = setup_fixture(llm, &["read_file"]);
    let mut request = make_request("hello");
    request.cancel_flag = Some(cancel);

    let result = run_turn(fixture.session_api, fixture.agent_loop, request)
        .await
        .unwrap();

    assert_eq!(result.status, RunTurnStatus::Cancelled);
    let entry = last_log_entry(&fixture.store, &result.session_id);
    assert_eq!(entry.status, TurnCommitStatus::Cancelled);
    assert!(entry
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("cancelled"));
    assert_eq!(entry.usage.as_ref().unwrap().total_tokens, 7);
}

#[tokio::test]
async fn run_cutover_model_override_takes_precedence_over_loaded_profile() {
    let fixture = setup_with_events(happy_events(), &[]);
    let request = RunTurnRequest {
        input: "test".to_string(),
        session_id: None,
        profile: None,
        model: Some("override-model".to_string()),
        loaded_profile: Some(make_loaded_profile("profile-model", "System from profile.")),
        cancel_flag: None,
    };

    let result = run_turn(fixture.session_api, fixture.agent_loop, request)
        .await
        .unwrap();
    assert_eq!(result.status, RunTurnStatus::Completed);

    let entry = last_log_entry(&fixture.store, &result.session_id);
    assert_eq!(entry.model, "override-model");
}

#[tokio::test]
async fn run_cutover_profile_model_used_when_no_override() {
    let fixture = setup_with_events(happy_events(), &[]);
    let request = RunTurnRequest {
        input: "test".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(make_loaded_profile("profile-model", "System from profile.")),
        cancel_flag: None,
    };

    let result = run_turn(fixture.session_api, fixture.agent_loop, request)
        .await
        .unwrap();
    assert_eq!(result.status, RunTurnStatus::Completed);

    let entry = last_log_entry(&fixture.store, &result.session_id);
    assert_eq!(entry.model, "profile-model");
}

#[tokio::test]
async fn run_turn_plumbs_stable_provider_to_agent_request_and_commit_metadata() {
    let fixture = setup_with_events(happy_events(), &[]);
    let request = RunTurnRequest {
        input: "test".to_string(),
        session_id: None,
        profile: None,
        model: Some("compat:deepseek-chat".to_string()),
        loaded_profile: None,
        cancel_flag: None,
    };

    let result = run_turn(fixture.session_api, fixture.agent_loop, request)
        .await
        .unwrap();
    assert_eq!(result.status, RunTurnStatus::Completed);

    let entry = last_log_entry(&fixture.store, &result.session_id);
    assert_eq!(entry.provider, "openai_compatible");
}

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
    let fixture = setup_fixture(llm, &["read_file"]);

    let events: Vec<TurnEvent> = stream_turn(
        fixture.session_api,
        fixture.agent_loop,
        make_request("read file"),
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
        fixture.agent_loop,
        make_request("hello"),
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
        last_log_entry(&fixture.store, &session_id).status,
        TurnCommitStatus::Error
    );
}

#[test]
fn turn_usage_summary_from_turn_usage() {
    let usage = TurnUsage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
    };
    let summary = TurnUsageSummary::from(usage);
    assert_eq!(summary.prompt_tokens, 100);
    assert_eq!(summary.completion_tokens, 50);
    assert_eq!(summary.total_tokens, 150);
}

#[tokio::test]
async fn run_turn_commits_parent_from_initial_transcript() {
    let fixture = setup_with_events(happy_events(), &[]);
    let first = run_turn(
        fixture.session_api,
        fixture.agent_loop,
        make_request("first turn"),
    )
    .await
    .unwrap();
    let first_commit_id = first.commit_id.clone();
    let session_id = first.session_id.clone();

    let session_api = SessionApi::sqlite(&fixture.sessions_path);
    let enabled: Vec<String> = vec![];
    let executor = lattice_tools::executor::DefaultToolExecutor::for_project_root(
        &fixture._dir.path().to_string_lossy(),
        enabled,
    )
    .unwrap();
    let agent_loop = Arc::new(AgentLoop::new(AgentLoopConfig {
        llm: Arc::new(
            FakeStreamingLlmClient::builder()
                .push_events(happy_events())
                .build(),
        ),
        tool_executor: Arc::new(executor),
    }));

    let result = run_turn(
        session_api,
        agent_loop,
        make_request_with_session("second turn", &session_id),
    )
    .await
    .unwrap();

    assert_eq!(result.status, RunTurnStatus::Completed);
    let entry = last_log_entry(&fixture.store, &result.session_id);
    assert_eq!(entry.parent_ids, vec![first_commit_id]);
}

// ── VER-07 verification tests ───────────────────────────────────────

#[test]
fn status_error_maps_to_commit_error() {
    assert_eq!(
        RunTurnStatus::Error.to_commit_status(),
        TurnCommitStatus::Error
    );
}

#[test]
fn turn_event_serializes_with_type_discriminator() {
    let event = TurnEvent::TokenDelta {
        content: "hi".to_string(),
    };
    let json = serde_json::to_value(&event).expect("TurnEvent should serialize");
    assert_eq!(json["type"], "token_delta");
    assert_eq!(json["content"], "hi");
}

#[test]
fn turn_event_unit_variant_serializes_correctly() {
    let event = TurnEvent::TurnCommitting;
    let json = serde_json::to_value(&event).expect("unit variant should serialize");
    assert_eq!(json["type"], "turn_committing");
    let obj = json
        .as_object()
        .expect("unit variant should serialize as object with `type` only");
    assert_eq!(
        obj.len(),
        1,
        "TurnCommitting should serialize with only `type`, got {obj:?}"
    );
}

#[test]
fn turn_event_struct_variant_with_optional_none() {
    let event = TurnEvent::TurnCancelled { reason: None };
    let json = serde_json::to_value(&event).expect("TurnCancelled should serialize");
    assert_eq!(json["type"], "turn_cancelled");
    assert!(
        json["reason"].is_null(),
        "Optional reason field should serialize as null when None, got {:?}",
        json["reason"]
    );
}

#[test]
fn turn_event_turn_started_serializes() {
    let event = TurnEvent::TurnStarted {
        session_id: "s1".to_string(),
        parent_commit_id: None,
        branch: None,
    };
    let json = serde_json::to_value(&event).expect("TurnStarted should serialize");
    assert_eq!(json["type"], "turn_started");
    assert_eq!(json["session_id"], "s1");
}

#[test]
fn old_failed_variant_removed_compile_guard() {
    let status = RunTurnStatus::Error;
    assert_ne!(format!("{status:?}"), "Failed");
    assert_eq!(format!("{status:?}"), "Error");
}

#[tokio::test]
async fn stream_turn_cancelled_commits_cancelled() {
    let cancel = Arc::new(AtomicBool::new(false));
    let llm: Arc<dyn LlmClient> = Arc::new(CancelOnDoneLlm::new(
        cancel.clone(),
        vec![
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
        ],
        "tool_calls",
        None,
    ));
    let fixture = setup_fixture(llm, &["read_file"]);
    let mut request = make_request("hello");
    request.cancel_flag = Some(cancel);

    let events: Vec<TurnEvent> = stream_turn(fixture.session_api, fixture.agent_loop, request)
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
        let entry = last_log_entry(&fixture.store, session_id);
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
        fixture.agent_loop,
        make_request("hello"),
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
        let transcript = fixture.store.head_transcript(session_id).unwrap();
        let assistant = transcript
            .messages
            .iter()
            .find(|m| m.role == "assistant")
            .expect("assistant message in commit");
        assert_eq!(assistant.content, "partial output before error");
        let entry = last_log_entry(&fixture.store, session_id);
        assert_eq!(entry.status, TurnCommitStatus::Error);
        assert!(entry
            .terminal_reason
            .as_ref()
            .unwrap()
            .contains("connection reset"));
    }
}
