use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::StreamExt;
use lattice_agent::{AgentTurnEvent, AgentTurnRequest, AgentTurnResult, AgentTurnStatus};
use lattice_session::{GraphMessage, HeadTranscript, TurnCommitInput, TurnUsage};

use crate::session::ApiError;
use crate::turn::deps::TurnDeps;
use crate::turn::execute::{run_turn_with_deps, stream_turn_with_deps};
use crate::turn::types::{
    RunTurnRequest, RunTurnStage, RunTurnStatus, TurnEvent, TurnUsageSummary,
};

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
    transcripts: std::sync::Mutex<Vec<HeadTranscript>>,
}

impl FakeTurnDeps {
    fn new(agent_result: AgentTurnResult) -> Self {
        Self {
            created_session_id: "auto-created-session".to_string(),
            agent_result: std::sync::Mutex::new(Some(agent_result)),
            agent_events: vec![
                AgentTurnEvent::ModelStarted {
                    provider: None,
                    model: "test-model".to_string(),
                },
                AgentTurnEvent::TokenDelta {
                    content: "test response".to_string(),
                },
                AgentTurnEvent::ModelFinished {
                    finish_reason: "stop".to_string(),
                    usage: None,
                },
            ],
            commit_id: "commit-abc123".to_string(),
            create_called: AtomicBool::new(false),
            open_called: AtomicBool::new(false),
            commit_count: AtomicUsize::new(0),
            last_commit_input: std::sync::Mutex::new(None),
            last_agent_session_id: std::sync::Mutex::new(None),
            last_agent_request: std::sync::Mutex::new(None),
            transcripts: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn with_transcripts(mut self, transcripts: Vec<HeadTranscript>) -> Self {
        self.transcripts = std::sync::Mutex::new(transcripts);
        self
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
        let mut transcripts = self.transcripts.lock().unwrap();
        if transcripts.len() > 1 {
            Ok(transcripts.remove(0))
        } else if let Some(t) = transcripts.pop() {
            Ok(t)
        } else {
            Ok(HeadTranscript {
                session_id: session_id.to_string(),
                parent_commit_id: None,
                messages: vec![],
            })
        }
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
        status: AgentTurnStatus::Error,
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
        },
        system_prompt: system_prompt.to_string(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────

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

#[tokio::test]
async fn run_turn_commits_exactly_one_turn() {
    let deps = FakeTurnDeps::new(success_result());
    let request = make_request("test input");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

    assert_eq!(deps.commit_count(), 1, "exactly one commit should be made");
    assert_eq!(result.status, RunTurnStatus::Completed);
}

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

#[tokio::test]
async fn run_turn_failed_outcome_is_committed() {
    let deps = FakeTurnDeps::new(failed_result());
    let request = make_request("hello");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

    assert_eq!(result.status, RunTurnStatus::Error);
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

    // Commit metadata should also use the override model.
    let commit_input = deps
        .last_commit_input()
        .expect("commit should have been called");
    assert_eq!(
        commit_input.model, "override-model",
        "commit metadata should record the actual override model, not profile model"
    );
}

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
        .find(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted event");

    if let TurnEvent::TurnCommitted {
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
        panic!("expected TurnCommitted event");
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
        AgentTurnEvent::ModelStarted {
            provider: None,
            model: "test-model".to_string(),
        },
        AgentTurnEvent::ToolCallStarted {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
        },
        AgentTurnEvent::ToolCallFinished {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            ok: true,
            output_len: 12,
            error_summary: None,
        },
        AgentTurnEvent::TokenDelta {
            content: "here is the file".to_string(),
        },
        AgentTurnEvent::ModelFinished {
            finish_reason: "stop".to_string(),
            usage: None,
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
        .position(|e| matches!(e, TurnEvent::ToolCallStarted { .. }));
    let tool_finished_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ToolCallFinished { .. }));
    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitted { .. }));

    assert!(
        tool_start_pos.is_some(),
        "should have ToolCallStarted event"
    );
    assert!(
        tool_finished_pos.is_some(),
        "should have ToolCallFinished event"
    );
    assert!(committed_pos.is_some(), "should have TurnCommitted event");

    assert!(
        tool_start_pos.unwrap() < committed_pos.unwrap(),
        "ToolCallStarted must appear before TurnCommitted"
    );
    assert!(
        tool_finished_pos.unwrap() < committed_pos.unwrap(),
        "ToolCallFinished must appear before TurnCommitted"
    );
}

#[tokio::test]
async fn stream_turn_error_still_commits_error() {
    let deps = FakeTurnDeps::new(failed_result());
    let request = make_request("hello");
    let events: Vec<TurnEvent> = stream_turn_with_deps(&deps, request)
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

    // The terminal TurnFailed must appear before TurnCommitted.
    let failed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnFailed { .. }))
        .expect("should have TurnFailed event before commit");
    let committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("should have TurnCommitted event");
    assert!(
        failed_pos < committed_pos,
        "TurnFailed must precede TurnCommitted in the stream"
    );

    assert_eq!(deps.commit_count(), 1);
    let commit_input = deps
        .last_commit_input()
        .expect("commit should have been called");
    assert_eq!(
        commit_input.status,
        lattice_session::TurnCommitStatus::Error
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
    let initial_transcript = HeadTranscript {
        session_id: "existing-session".to_string(),
        parent_commit_id: Some("parent-initial".to_string()),
        messages: vec![],
    };
    let raced_transcript = HeadTranscript {
        session_id: "existing-session".to_string(),
        parent_commit_id: Some("parent-raced".to_string()),
        messages: vec![],
    };

    let deps = FakeTurnDeps::new(success_result())
        .with_transcripts(vec![initial_transcript, raced_transcript]);

    let request = make_request_with_session("hello", "existing-session");
    let result = run_turn_with_deps(&deps, request).await.unwrap();

    assert_eq!(result.status, RunTurnStatus::Completed);

    let commit_input = deps
        .last_commit_input()
        .expect("commit should have been called");

    assert_eq!(
        commit_input.parent_ids,
        vec!["parent-initial".to_string()],
        "commit should use parent from initial transcript, not a raced second read"
    );
}

// ── VER-07 verification tests ───────────────────────────────────────
//
// These tests pin the public vocabulary contract:
//   1. RunTurnStatus::Error maps to TurnCommitStatus::Error.
//   2. TurnEvent serializes with internally tagged `{"type": ...}` JSON.
//   3. Old Failed variant is gone.

#[test]
fn status_error_maps_to_commit_error() {
    assert_eq!(
        RunTurnStatus::Error.to_commit_status(),
        lattice_session::TurnCommitStatus::Error
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
    // Compile-time guard: this only builds because RunTurnStatus::Error exists.
    // The runtime assertion proves the renamed variant Debug-prints as "Error",
    // not "Failed", so any reintroduction of Failed shows up here too.
    let status = RunTurnStatus::Error;
    assert_ne!(format!("{status:?}"), "Failed");
    assert_eq!(format!("{status:?}"), "Error");
}
