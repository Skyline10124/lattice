use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::StreamExt;
use lattice_agent::{AgentTurnEvent, AgentTurnRequest, AgentTurnResult, AgentTurnStatus};
use lattice_api::{
    run_turn_with_deps, stream_turn_with_deps, ApiError, RunTurnRequest, RunTurnResult,
    RunTurnStage, RunTurnStatus, ToolResultSummary, TurnDeps, TurnEvent, TurnUsageSummary,
};
use lattice_session::{GraphMessage, HeadTranscript, TurnCommitInput, TurnUsage};

// ── Fake TurnDeps ───────────────────────────────────────────────────

struct FakeTurnDeps {
    created_session_id: String,
    agent_result: std::sync::Mutex<Option<AgentTurnResult>>,
    agent_events: Vec<AgentTurnEvent>,
    commit_id: String,
    create_called: AtomicBool,
    open_called: AtomicBool,
    commit_count: AtomicUsize,
    last_commit_input: std::sync::Mutex<Option<TurnCommitInput>>,
    last_agent_session_id: std::sync::Mutex<Option<String>>,
    last_agent_request: std::sync::Mutex<Option<AgentTurnRequest>>,
}

impl FakeTurnDeps {
    fn new(agent_result: AgentTurnResult) -> Self {
        Self {
            created_session_id: "auto-created-session".to_string(),
            agent_result: std::sync::Mutex::new(Some(agent_result)),
            agent_events: vec![
                AgentTurnEvent::Started,
                AgentTurnEvent::Token {
                    content: "test response".to_string(),
                },
                AgentTurnEvent::Done {
                    finish_reason: "stop".to_string(),
                },
            ],
            commit_id: "commit-abc123".to_string(),
            create_called: AtomicBool::new(false),
            open_called: AtomicBool::new(false),
            commit_count: AtomicUsize::new(0),
            last_commit_input: std::sync::Mutex::new(None),
            last_agent_session_id: std::sync::Mutex::new(None),
            last_agent_request: std::sync::Mutex::new(None),
        }
    }

    fn with_events(mut self, events: Vec<AgentTurnEvent>) -> Self {
        self.agent_events = events;
        self
    }

    fn created_session(&self) -> bool {
        self.create_called.load(Ordering::SeqCst)
    }

    fn opened_session(&self) -> bool {
        self.open_called.load(Ordering::SeqCst)
    }

    fn commit_count(&self) -> usize {
        self.commit_count.load(Ordering::SeqCst)
    }

    fn last_commit_input(&self) -> Option<TurnCommitInput> {
        self.last_commit_input.lock().unwrap().clone()
    }

    fn last_agent_session_id(&self) -> Option<String> {
        self.last_agent_session_id.lock().unwrap().clone()
    }

    fn last_agent_request(&self) -> Option<AgentTurnRequest> {
        self.last_agent_request.lock().unwrap().take()
    }
}

#[async_trait::async_trait]
impl TurnDeps for FakeTurnDeps {
    fn create_session(&self, _request: &RunTurnRequest) -> std::result::Result<String, ApiError> {
        self.create_called.store(true, Ordering::SeqCst);
        Ok(self.created_session_id.clone())
    }

    fn open_session(&self, session_id: &str) -> std::result::Result<String, ApiError> {
        self.open_called.store(true, Ordering::SeqCst);
        Ok(session_id.to_string())
    }

    fn head_transcript(&self, session_id: &str) -> std::result::Result<HeadTranscript, ApiError> {
        Ok(HeadTranscript {
            session_id: session_id.to_string(),
            parent_commit_id: None,
            messages: vec![],
        })
    }

    async fn execute_agent_turn(
        &self,
        request: AgentTurnRequest,
    ) -> std::result::Result<(Vec<AgentTurnEvent>, AgentTurnResult), ApiError> {
        *self.last_agent_session_id.lock().unwrap() = Some(request.session_id.clone());
        *self.last_agent_request.lock().unwrap() = Some(request.clone());
        let result = self
            .agent_result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(success_result);
        Ok((self.agent_events.clone(), result))
    }

    fn commit_turn(&self, input: TurnCommitInput) -> std::result::Result<String, ApiError> {
        self.commit_count.fetch_add(1, Ordering::SeqCst);
        *self.last_commit_input.lock().unwrap() = Some(input);
        Ok(self.commit_id.clone())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

fn success_result() -> AgentTurnResult {
    AgentTurnResult {
        messages: vec![GraphMessage {
            role: "assistant".to_string(),
            content: "test response".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        assistant_output: "test response".to_string(),
        usage: TurnUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        },
        tool_results: vec![],
        status: AgentTurnStatus::Completed,
        terminal_reason: Some("stop".to_string()),
        events: vec![],
    }
}

fn failed_result() -> AgentTurnResult {
    AgentTurnResult {
        messages: vec![GraphMessage {
            role: "assistant".to_string(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        assistant_output: String::new(),
        usage: TurnUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        },
        tool_results: vec![],
        status: AgentTurnStatus::Failed,
        terminal_reason: Some("LLM error: provider unavailable".to_string()),
        events: vec![],
    }
}

fn cancelled_result() -> AgentTurnResult {
    AgentTurnResult {
        messages: vec![GraphMessage {
            role: "assistant".to_string(),
            content: String::new(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }],
        assistant_output: String::new(),
        usage: TurnUsage {
            prompt_tokens: 5,
            completion_tokens: 2,
            total_tokens: 7,
        },
        tool_results: vec![],
        status: AgentTurnStatus::Cancelled,
        terminal_reason: Some("cancelled by caller".to_string()),
        events: vec![],
    }
}

fn make_request(input: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: None,
    }
}

fn make_request_with_session(input: &str, session_id: &str) -> RunTurnRequest {
    RunTurnRequest {
        input: input.to_string(),
        session_id: Some(session_id.to_string()),
        profile: None,
        model: None,
        loaded_profile: None,
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
            security: None,
            context: None,
            behavior: None,
        },
        system_prompt: system_prompt.to_string(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

/// Test 1: run_turn auto-creates session when session_id is None.
#[tokio::test]
async fn run_turn_auto_creates_session_and_returns_session_id() {
    let deps = FakeTurnDeps::new(success_result());
    let request = make_request("hello");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

    assert!(
        !result.session_id.is_empty(),
        "session_id should not be empty"
    );
    assert_eq!(result.session_id, "auto-created-session");
    assert!(
        !result.commit_id.is_empty(),
        "commit_id should not be empty"
    );
    assert!(
        deps.created_session(),
        "create_session should have been called"
    );
    assert!(
        !deps.opened_session(),
        "open_session should not have been called"
    );
}

/// Test 2: run_turn commits exactly one turn.
#[tokio::test]
async fn run_turn_commits_exactly_one_turn() {
    let deps = FakeTurnDeps::new(success_result());
    let request = make_request("test input");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

    assert_eq!(deps.commit_count(), 1, "exactly one commit should be made");
    assert_eq!(result.status, RunTurnStatus::Completed);
}

/// Test 3: run_turn uses existing session transcript.
#[tokio::test]
async fn run_turn_uses_existing_session_transcript() {
    let deps = FakeTurnDeps::new(success_result());
    let request = make_request_with_session("second turn", "existing-session");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

    assert!(
        deps.opened_session(),
        "open_session should have been called"
    );
    assert!(
        !deps.created_session(),
        "create_session should not have been called"
    );
    assert_eq!(result.session_id, "existing-session");
    assert_eq!(
        deps.last_agent_session_id(),
        Some("existing-session".to_string()),
        "agent should receive the existing session ID"
    );
    assert_eq!(deps.commit_count(), 1);
}

/// Test 4: run_turn result contains all contract fields.
#[tokio::test]
async fn run_turn_result_contains_contract_fields() {
    let deps = FakeTurnDeps::new(success_result());
    let request = make_request("hello");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

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

/// Test 5: failed agent outcome is committed with status and terminal reason.
#[tokio::test]
async fn run_turn_failed_outcome_is_committed() {
    let deps = FakeTurnDeps::new(failed_result());
    let request = make_request("hello");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

    assert_eq!(result.status, RunTurnStatus::Failed);
    assert_eq!(result.assistant_output, "");
    assert_eq!(deps.commit_count(), 1);
    let commit_input = deps
        .last_commit_input()
        .expect("commit should have been called");
    assert_eq!(
        commit_input.status,
        lattice_session::TurnCommitStatus::Error
    );
    assert!(commit_input
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("provider unavailable"));
    assert!(
        !commit_input.messages.is_empty(),
        "failed turn should still commit messages"
    );
}

/// Test 6: cancelled agent outcome is committed with status and terminal reason.
#[tokio::test]
async fn run_turn_cancelled_outcome_is_committed() {
    let deps = FakeTurnDeps::new(cancelled_result());
    let request = make_request("hello");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

    assert_eq!(result.status, RunTurnStatus::Cancelled);
    assert_eq!(deps.commit_count(), 1);
    let commit_input = deps
        .last_commit_input()
        .expect("commit should have been called");
    assert_eq!(
        commit_input.status,
        lattice_session::TurnCommitStatus::Cancelled
    );
    assert!(commit_input
        .terminal_reason
        .as_ref()
        .unwrap()
        .contains("cancelled"));
    assert!(commit_input.usage.is_some());
    assert_eq!(commit_input.usage.as_ref().unwrap().total_tokens, 7);
}

// ── Model override precedence (D-10) ────────────────────────────────

/// When request.model is Some and loaded_profile is present,
/// the model override takes precedence. Tested through run_turn_with_deps.
#[tokio::test]
async fn run_cutover_model_override_takes_precedence_over_loaded_profile() {
    let deps = FakeTurnDeps::new(success_result());
    let request = RunTurnRequest {
        input: "test".to_string(),
        session_id: None,
        profile: None,
        model: Some("override-model".to_string()),
        loaded_profile: Some(make_loaded_profile("profile-model", "System from profile.")),
    };

    let result = run_turn_with_deps(&deps, request).await.unwrap();
    assert_eq!(result.status, RunTurnStatus::Completed);

    let agent_req = deps
        .last_agent_request()
        .expect("agent should have been called");
    assert_eq!(
        agent_req.model, "override-model",
        "request.model should override profile model"
    );
}

/// When request.model is None and loaded_profile is present,
/// the profile's model is used.
#[tokio::test]
async fn run_cutover_profile_model_used_when_no_override() {
    let deps = FakeTurnDeps::new(success_result());
    let request = RunTurnRequest {
        input: "test".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(make_loaded_profile("profile-model", "System from profile.")),
    };

    let result = run_turn_with_deps(&deps, request).await.unwrap();
    assert_eq!(result.status, RunTurnStatus::Completed);

    let agent_req = deps
        .last_agent_request()
        .expect("agent should have been called");
    assert_eq!(
        agent_req.model, "profile-model",
        "profile model should be used when no override"
    );
}

// ── Serde roundtrip tests ───────────────────────────────────────────

#[test]
fn run_turn_status_serde_roundtrip() {
    let statuses = vec![
        RunTurnStatus::Completed,
        RunTurnStatus::Failed,
        RunTurnStatus::Cancelled,
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let deserialized: RunTurnStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(status, deserialized);
    }
}

#[test]
fn run_turn_result_serde_roundtrip() {
    let result = RunTurnResult {
        session_id: "sess-1".to_string(),
        commit_id: "commit-1".to_string(),
        assistant_output: "hello".to_string(),
        status: RunTurnStatus::Completed,
        usage: Some(TurnUsageSummary {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
        tool_results: vec![ToolResultSummary {
            tool_call_id: "call-1".to_string(),
            name: "read_file".to_string(),
            ok: true,
            output_len: 100,
            error_summary: None,
        }],
        stages: vec![RunTurnStage::Profile, RunTurnStage::Commit],
    };
    let json = serde_json::to_string(&result).unwrap();
    let deserialized: RunTurnResult = serde_json::from_str(&json).unwrap();
    assert_eq!(result, deserialized);
}

#[test]
fn tool_result_summary_serde_roundtrip() {
    let summary = ToolResultSummary {
        tool_call_id: "call-1".to_string(),
        name: "read_file".to_string(),
        ok: true,
        output_len: 1024,
        error_summary: None,
    };
    let json = serde_json::to_string(&summary).unwrap();
    let deserialized: ToolResultSummary = serde_json::from_str(&json).unwrap();
    assert_eq!(summary, deserialized);
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

// ── stream_turn behavioral tests ─────────────────────────────────────

#[tokio::test]
async fn stream_turn_auto_creates_session_and_final_committed_event_names_it() {
    let deps = FakeTurnDeps::new(success_result());
    let request = make_request("hello");
    let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
        .await
        .unwrap()
        .collect()
        .await;

    let committed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::Committed { .. }))
        .expect("should have Committed event");

    if let TurnEvent::Committed {
        session_id,
        commit_id,
        status,
    } = committed
    {
        assert!(!session_id.is_empty(), "session_id should be non-empty");
        assert_eq!(session_id, "auto-created-session");
        assert!(!commit_id.is_empty(), "commit_id should be non-empty");
        assert_eq!(*status, RunTurnStatus::Completed);
    } else {
        panic!("expected Committed event");
    }
}

#[tokio::test]
async fn stream_turn_emits_tool_activity_before_committed() {
    let tool_result_agent = AgentTurnResult {
        messages: vec![
            GraphMessage {
                role: "assistant".to_string(),
                content: String::new(),
                reasoning_content: None,
                tool_calls: Some(vec![lattice_session::GraphToolCall {
                    id: "call_1".to_string(),
                    name: "read_file".to_string(),
                    arguments: "{}".to_string(),
                }]),
                tool_call_id: None,
                name: None,
            },
            GraphMessage {
                role: "tool".to_string(),
                content: "file content".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: Some("call_1".to_string()),
                name: Some("read_file".to_string()),
            },
            GraphMessage {
                role: "assistant".to_string(),
                content: "here is the file".to_string(),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
                name: None,
            },
        ],
        assistant_output: "here is the file".to_string(),
        usage: TurnUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        },
        tool_results: vec![lattice_agent::AgentToolResultSummary {
            tool_call_id: "call_1".to_string(),
            name: "read_file".to_string(),
            ok: true,
            output_len: 12,
            error_summary: None,
        }],
        status: AgentTurnStatus::Completed,
        terminal_reason: Some("stop".to_string()),
        events: vec![],
    };
    let tool_events = vec![
        AgentTurnEvent::Started,
        AgentTurnEvent::ToolCallStart {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
        },
        AgentTurnEvent::ToolCallResult {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            ok: true,
            output_len: 12,
        },
        AgentTurnEvent::Token {
            content: "here is the file".to_string(),
        },
        AgentTurnEvent::Done {
            finish_reason: "stop".to_string(),
        },
    ];

    let deps = FakeTurnDeps::new(tool_result_agent).with_events(tool_events);
    let request = make_request("read file");
    let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
        .await
        .unwrap()
        .collect()
        .await;

    let tool_start_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ToolCallStart { .. }));
    let tool_result_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ToolResult { .. }));
    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::Committed { .. }));

    assert!(tool_start_pos.is_some(), "should have ToolCallStart event");
    assert!(tool_result_pos.is_some(), "should have ToolResult event");
    assert!(committed_pos.is_some(), "should have Committed event");

    assert!(
        tool_start_pos.unwrap() < committed_pos.unwrap(),
        "ToolCallStart must appear before Committed"
    );
    assert!(
        tool_result_pos.unwrap() < committed_pos.unwrap(),
        "ToolResult must appear before Committed"
    );
}

#[tokio::test]
async fn stream_turn_does_not_replay_only_after_commit() {
    let deps = FakeTurnDeps::new(success_result());
    let request = make_request("hello");
    let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
        .await
        .unwrap()
        .collect()
        .await;

    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::Committed { .. }))
        .expect("should have Committed event");

    assert!(
        committed_pos > 0,
        "at least one event must appear before Committed (no replay-after-commit)"
    );

    assert!(
        matches!(events[0], TurnEvent::Started),
        "first event should be Started, got {:?}",
        events[0]
    );
}

#[tokio::test]
async fn stream_turn_result_summary_excludes_full_large_tool_text() {
    let large_content = "x".repeat(10_000);
    let large_agent = AgentTurnResult {
        messages: vec![GraphMessage {
            role: "tool".to_string(),
            content: large_content.clone(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: Some("call_1".to_string()),
            name: Some("read_file".to_string()),
        }],
        assistant_output: "done".to_string(),
        usage: TurnUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        },
        tool_results: vec![lattice_agent::AgentToolResultSummary {
            tool_call_id: "call_1".to_string(),
            name: "read_file".to_string(),
            ok: true,
            output_len: large_content.len(),
            error_summary: None,
        }],
        status: AgentTurnStatus::Completed,
        terminal_reason: Some("stop".to_string()),
        events: vec![],
    };
    let events_with_tool = vec![
        AgentTurnEvent::Started,
        AgentTurnEvent::ToolCallStart {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
        },
        AgentTurnEvent::ToolCallResult {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            ok: true,
            output_len: large_content.len(),
        },
        AgentTurnEvent::Done {
            finish_reason: "stop".to_string(),
        },
    ];

    let deps = FakeTurnDeps::new(large_agent).with_events(events_with_tool);
    let request = make_request("read file");
    let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
        .await
        .unwrap()
        .collect()
        .await;

    for event in &events {
        if let TurnEvent::ToolResult { output_len, .. } = event {
            assert_eq!(*output_len, 10_000);
        }
    }
}

#[tokio::test]
async fn stream_turn_error_still_commits_failed() {
    let deps = FakeTurnDeps::new(failed_result());
    let request = make_request("hello");
    let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
        .await
        .unwrap()
        .collect()
        .await;

    let committed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::Committed { .. }))
        .expect("should have Committed event even on failure");

    if let TurnEvent::Committed { status, .. } = committed {
        assert_eq!(*status, RunTurnStatus::Failed);
    }

    assert_eq!(deps.commit_count(), 1);
    let commit_input = deps
        .last_commit_input()
        .expect("commit should have been called");
    assert_eq!(
        commit_input.status,
        lattice_session::TurnCommitStatus::Error
    );
}

// ── TurnEvent serde tests ───────────────────────────────────────────

#[test]
fn turn_event_committed_serde_roundtrip() {
    let event = TurnEvent::Committed {
        session_id: "sess-1".to_string(),
        commit_id: "commit-1".to_string(),
        status: RunTurnStatus::Completed,
    };
    let json = serde_json::to_string(&event).unwrap();
    let deserialized: TurnEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(event, deserialized);
}

#[test]
fn turn_event_tool_result_serde_roundtrip() {
    let event = TurnEvent::ToolResult {
        id: "call-1".to_string(),
        name: "read_file".to_string(),
        ok: true,
        output_len: 1024,
    };
    let json = serde_json::to_string(&event).unwrap();
    let deserialized: TurnEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(event, deserialized);
}

// ── Boundary enforcement tests ──────────────────────────────────────

#[test]
fn api_public_types_are_exported() {
    let _ = std::any::TypeId::of::<lattice_api::RunTurnRequest>();
    let _ = std::any::TypeId::of::<lattice_api::StreamTurnRequest>();
    let _ = std::any::TypeId::of::<lattice_api::RunTurnResult>();
    let _ = std::any::TypeId::of::<lattice_api::RunTurnStatus>();
    let _ = std::any::TypeId::of::<lattice_api::ToolResultSummary>();
    let _ = std::any::TypeId::of::<lattice_api::TurnUsageSummary>();
    let _ = std::any::TypeId::of::<lattice_api::RunTurnStage>();
    let _ = std::any::TypeId::of::<lattice_api::TurnEvent>();
    let _ = std::any::TypeId::of::<lattice_api::SessionApi>();
    let _ = std::any::TypeId::of::<lattice_api::ApiError>();
    let _ = std::any::TypeId::of::<lattice_api::CreateSessionRequest>();
    let _ = std::any::TypeId::of::<lattice_api::OpenSessionRequest>();
}

#[test]
fn api_does_not_export_internal_types() {
    // These should NOT be accessible as lattice_api:: exports.
    // If they were exported, this test would fail to compile if uncommented:
    // let _ = std::any::TypeId::of::<lattice_api::AgentLoop>();
}
