//! Run/stream consistency integration tests (Phase 17, Plan 03, VER-13).
//!
//! Proves semantic consistency between `run_turn` (buffered) and `stream_turn`
//! (live) paths across three scenarios using NormalizedTurnObservation comparison.
//! Both observation builders read assistant_output and transcript from the
//! committed session store (D-09), never from live events or result fields.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::{AgentLoop, AgentLoopConfig, LlmClient};
use lattice_api::{RunTurnRequest, RunTurnStatus, SessionApi, TurnEvent, TurnUsageSummary};
use lattice_llm::{ChatRequest, ChatResponse, LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{SessionGraphStore, SqliteSessionGraphStore};
use lattice_tools::executor::DefaultToolExecutor;

// ── NormalizedTurnObservation (D-08, D-09) ─────────────────────────────────

/// Normalized observation struct that strips unstable fields (session_id,
/// commit_id, timestamps) for structured comparison between run_turn and
/// stream_turn outcomes.
#[derive(Debug, PartialEq)]
struct NormalizedTurnObservation {
    status: RunTurnStatus,
    usage: Option<TurnUsageSummary>,
    assistant_output: String,
    tool_results: Vec<NormalizedToolResult>,
    transcript_roles_and_content: Vec<(String, String)>,
    terminal_reason: Option<String>,
}

/// Normalized tool result for comparison (strips tool_call_id which may differ).
#[derive(Debug, PartialEq)]
struct NormalizedToolResult {
    name: String,
    ok: bool,
    output_len: usize,
    error_summary: Option<String>,
}

// ── Observation builders ───────────────────────────────────────────────────

/// Build observation from run_turn result + committed session store.
fn observe_run_turn(
    result: &lattice_api::RunTurnResult,
    session_store: &SqliteSessionGraphStore,
) -> NormalizedTurnObservation {
    // Read committed transcript from session store (D-09: durable anchor)
    let transcript = session_store
        .head_transcript(&result.session_id)
        .expect("head_transcript after run_turn commit");

    let assistant_output = transcript
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    let transcript_roles_and_content: Vec<(String, String)> = transcript
        .messages
        .iter()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();

    let tool_results: Vec<NormalizedToolResult> = result
        .tool_results
        .iter()
        .map(|t| NormalizedToolResult {
            name: t.name.clone(),
            ok: t.ok,
            output_len: t.output_len,
            error_summary: t.error_summary.clone(),
        })
        .collect();

    // terminal_reason: None for Completed; for Error, extract from log entry
    let terminal_reason = if result.status == RunTurnStatus::Error {
        let log = session_store
            .log(&result.session_id)
            .expect("log after run_turn");
        log.last().and_then(|entry| entry.terminal_reason.clone())
    } else {
        None
    };

    // Normalize zero-usage to None (matches stream side which has no
    // ModelFinished on error paths, so usage is None there)
    let usage = result.usage.as_ref().and_then(|u| {
        if u.prompt_tokens == 0 && u.completion_tokens == 0 && u.total_tokens == 0 {
            None
        } else {
            Some(u.clone())
        }
    });

    NormalizedTurnObservation {
        status: result.status.clone(),
        usage,
        assistant_output,
        tool_results,
        transcript_roles_and_content,
        terminal_reason,
    }
}

/// Build observation from stream_turn events + committed session store.
fn observe_stream_turn(
    events: &[TurnEvent],
    session_store: &SqliteSessionGraphStore,
) -> NormalizedTurnObservation {
    // Find TurnCommitted to get session_id and status
    let (session_id, status) = events
        .iter()
        .find_map(|e| match e {
            TurnEvent::TurnCommitted {
                session_id, status, ..
            } => Some((session_id.clone(), status.clone())),
            _ => None,
        })
        .expect("stream_turn must emit TurnCommitted");

    // Read committed transcript from session store (D-09: durable anchor)
    let transcript = session_store
        .head_transcript(&session_id)
        .expect("head_transcript after stream_turn commit");

    let assistant_output = transcript
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    let transcript_roles_and_content: Vec<(String, String)> = transcript
        .messages
        .iter()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();

    // Usage: sum all ModelFinished events (multi-round turns accumulate usage)
    let mut total_prompt = 0u64;
    let mut total_completion = 0u64;
    let mut total_total = 0u64;
    let mut has_usage = false;
    for e in events {
        if let TurnEvent::ModelFinished {
            usage: Some(u), ..
        } = e
        {
            total_prompt += u.prompt_tokens;
            total_completion += u.completion_tokens;
            total_total += u.total_tokens;
            has_usage = true;
        }
    }
    let usage = if has_usage {
        Some(TurnUsageSummary {
            prompt_tokens: total_prompt,
            completion_tokens: total_completion,
            total_tokens: total_total,
        })
    } else {
        None
    };

    // Tool results from ToolCallFinished events
    let tool_results: Vec<NormalizedToolResult> = events
        .iter()
        .filter_map(|e| match e {
            TurnEvent::ToolCallFinished {
                name,
                ok,
                output_len,
                error_summary,
                ..
            } => Some(NormalizedToolResult {
                name: name.clone(),
                ok: *ok,
                output_len: *output_len,
                error_summary: error_summary.clone(),
            }),
            _ => None,
        })
        .collect();

    // terminal_reason from TurnFailed if present
    let terminal_reason = events.iter().find_map(|e| match e {
        TurnEvent::TurnFailed { reason } => Some(reason.clone()),
        _ => None,
    });

    NormalizedTurnObservation {
        status,
        usage,
        assistant_output,
        tool_results,
        transcript_roles_and_content,
        terminal_reason,
    }
}

// ── Fake LLM (buffered path) ──────────────────────────────────────────────

struct FakeLlm {
    responses: Mutex<VecDeque<ChatResponse>>,
}

impl FakeLlm {
    fn new(responses: Vec<ChatResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(VecDeque::from(responses)),
        })
    }
}

#[async_trait::async_trait]
impl LlmClient for FakeLlm {
    async fn chat(
        &self,
        _request: ChatRequest,
    ) -> Result<ChatResponse, Box<dyn std::error::Error + Send + Sync>> {
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "FakeLlm: no more responses queued".into())
    }
}

// ── Workspace setup ───────────────────────────────────────────────────────

struct Workspace {
    _dir: tempfile::TempDir,
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

fn setup_workspace(enabled_tools: &[&str]) -> Workspace {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn hello() {}").unwrap();

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let enabled: Vec<String> = enabled_tools.iter().map(|s| s.to_string()).collect();
    let toml_content = format!(
        r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "You are a test agent."
[tools]
enabled = {enabled:?}
"#
    );
    std::fs::write(profile_dir.join("agent.toml"), toml_content).unwrap();

    let global_agents = tempfile::tempdir().unwrap();
    let paths = DiscoveryPaths {
        project_root: root.clone(),
        global_agents_dir: global_agents.path().to_path_buf(),
    };
    let loaded = lattice_profile::resolve_agent_profile(ProfileSource::Default, &paths)
        .expect("profile must load");

    Workspace {
        _dir: ws,
        _global_agents: global_agents,
        root,
        loaded,
    }
}

fn build_agent_loop(
    llm: Arc<dyn LlmClient>,
    workspace_root: &std::path::Path,
    enabled_tools: &[&str],
) -> Arc<AgentLoop> {
    let enabled: Vec<String> = enabled_tools.iter().map(|s| s.to_string()).collect();
    let executor =
        DefaultToolExecutor::for_project_root(&workspace_root.to_string_lossy(), enabled)
            .expect("executor creation must succeed");

    Arc::new(AgentLoop::new(AgentLoopConfig {
        llm,
        tool_executor: Arc::new(executor),
    }))
}

// ── Test 1: Completed content-only (D-11 scenario 1) ──────────────────────

#[tokio::test]
async fn consistency_completed_content_only() {
    let ws = setup_workspace(&[]);

    // Script: LLM returns "Hello, world!" with stop, usage {10, 5, 15}
    let chat_response = ChatResponse {
        content: "Hello, world!".to_string(),
        reasoning_content: None,
        tool_calls: None,
        finish_reason: "stop".to_string(),
        model: "fake-model".to_string(),
        usage: Some(TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        }),
    };

    let streaming_events = vec![
        LlmEvent::Token {
            content: "Hello, world!".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
    ];

    // --- Run path (independent tempdir, D-07) ---
    let run_session_dir = tempfile::tempdir().unwrap();
    let run_sessions_path = run_session_dir.path().join("sessions");
    let run_session_api = SessionApi::sqlite(&run_sessions_path);
    let run_llm = FakeLlm::new(vec![chat_response]);
    let run_agent_loop = build_agent_loop(run_llm, &ws.root, &[]);

    let run_request = RunTurnRequest {
        input: "hi".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded.clone()),
    };

    let run_result = lattice_api::run_turn(run_session_api, run_agent_loop, run_request)
        .await
        .expect("run_turn must succeed");

    let run_store = SqliteSessionGraphStore::new(&run_sessions_path);
    let run_obs = observe_run_turn(&run_result, &run_store);

    // --- Stream path (independent tempdir, D-07) ---
    let stream_session_dir = tempfile::tempdir().unwrap();
    let stream_sessions_path = stream_session_dir.path().join("sessions");
    let stream_session_api = SessionApi::sqlite(&stream_sessions_path);
    let stream_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(streaming_events)
            .build(),
    );
    let stream_agent_loop = build_agent_loop(stream_llm, &ws.root, &[]);

    let stream_request = RunTurnRequest {
        input: "hi".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded.clone()),
    };

    let events: Vec<TurnEvent> = lattice_api::stream_turn(
        stream_session_api,
        stream_agent_loop,
        stream_request,
    )
    .await
    .expect("stream_turn must start")
    .collect()
    .await;

    let stream_store = SqliteSessionGraphStore::new(&stream_sessions_path);
    let stream_obs = observe_stream_turn(&events, &stream_store);

    // --- Consistency assertion ---
    assert_eq!(run_obs, stream_obs, "run_turn and stream_turn observations must match for completed content-only scenario");
}

// ── Test 2: Error after partial token (D-11 scenario 2) ───────────────────

#[tokio::test]
async fn consistency_error_after_partial_token() {
    let ws = setup_workspace(&[]);

    // Script: LLM yields Token("partial "), Token("resp"), then Error
    // For the buffered path, the FakeLlm chat() returns an error (simulating
    // the stream_chat default fallback encountering an error event).
    // For the streaming path, we emit tokens then an in-band error.

    let streaming_events = vec![
        LlmEvent::Token {
            content: "partial ".to_string(),
        },
        LlmEvent::Token {
            content: "resp".to_string(),
        },
        LlmEvent::Error {
            message: "provider error".to_string(),
        },
    ];

    // The buffered FakeLlm uses the same event sequence via FakeStreamingLlmClient
    // which aggregates events into a ChatResponse. When it encounters LlmEvent::Error,
    // it returns Err from chat(). But the default stream_chat fallback on LlmClient
    // calls chat() first. For consistency, both paths should use FakeStreamingLlmClient
    // which handles both chat() and stream_chat() from the same event queue.

    // --- Run path (independent tempdir, D-07) ---
    // run_turn uses stream_chat (via the default fallback which calls chat, but
    // FakeStreamingLlmClient overrides stream_chat directly). Actually, run_turn
    // calls AgentLoop::run_turn which calls stream_chat. So both paths go through
    // stream_chat. We can use FakeStreamingLlmClient for both.
    let run_session_dir = tempfile::tempdir().unwrap();
    let run_sessions_path = run_session_dir.path().join("sessions");
    let run_session_api = SessionApi::sqlite(&run_sessions_path);
    let run_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(streaming_events.clone())
            .build(),
    );
    let run_agent_loop = build_agent_loop(run_llm, &ws.root, &[]);

    let run_request = RunTurnRequest {
        input: "hello".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded.clone()),
    };

    let run_result = lattice_api::run_turn(run_session_api, run_agent_loop, run_request)
        .await
        .expect("run_turn must succeed (error is in-turn, not startup)");

    let run_store = SqliteSessionGraphStore::new(&run_sessions_path);
    let run_obs = observe_run_turn(&run_result, &run_store);

    // --- Stream path (independent tempdir, D-07) ---
    let stream_session_dir = tempfile::tempdir().unwrap();
    let stream_sessions_path = stream_session_dir.path().join("sessions");
    let stream_session_api = SessionApi::sqlite(&stream_sessions_path);
    let stream_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(streaming_events)
            .build(),
    );
    let stream_agent_loop = build_agent_loop(stream_llm, &ws.root, &[]);

    let stream_request = RunTurnRequest {
        input: "hello".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded.clone()),
    };

    let events: Vec<TurnEvent> = lattice_api::stream_turn(
        stream_session_api,
        stream_agent_loop,
        stream_request,
    )
    .await
    .expect("stream_turn must start")
    .collect()
    .await;

    let stream_store = SqliteSessionGraphStore::new(&stream_sessions_path);
    let stream_obs = observe_stream_turn(&events, &stream_store);

    // Both should commit with Error status, partial content preserved
    assert_eq!(run_obs.status, RunTurnStatus::Error);
    assert_eq!(stream_obs.status, RunTurnStatus::Error);
    assert_eq!(
        run_obs, stream_obs,
        "run_turn and stream_turn observations must match for error-after-partial scenario"
    );
}

// ── Test 3: Tool call path (D-11 scenario 3) ─────────────────────────────

#[tokio::test]
async fn consistency_tool_call_path() {
    let ws = setup_workspace(&["read_file"]);

    // Script: Round 1 — tool call to read_file. Round 2 — final response.
    // Both paths use FakeStreamingLlmClient with identical event sequences.

    let round1_events = vec![
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

    let round2_events = vec![
        LlmEvent::Token {
            content: "File contents read successfully.".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 20,
                completion_tokens: 8,
                total_tokens: 28,
            }),
        },
    ];

    // --- Run path (independent tempdir, D-07) ---
    let run_session_dir = tempfile::tempdir().unwrap();
    let run_sessions_path = run_session_dir.path().join("sessions");
    let run_session_api = SessionApi::sqlite(&run_sessions_path);
    let run_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(round1_events.clone())
            .push_events(round2_events.clone())
            .build(),
    );
    let run_agent_loop = build_agent_loop(run_llm, &ws.root, &["read_file"]);

    let run_request = RunTurnRequest {
        input: "Read src/lib.rs".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded.clone()),
    };

    let run_result = lattice_api::run_turn(run_session_api, run_agent_loop, run_request)
        .await
        .expect("run_turn must succeed");

    let run_store = SqliteSessionGraphStore::new(&run_sessions_path);
    let run_obs = observe_run_turn(&run_result, &run_store);

    // --- Stream path (independent tempdir, D-07) ---
    let stream_session_dir = tempfile::tempdir().unwrap();
    let stream_sessions_path = stream_session_dir.path().join("sessions");
    let stream_session_api = SessionApi::sqlite(&stream_sessions_path);
    let stream_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(round1_events)
            .push_events(round2_events)
            .build(),
    );
    let stream_agent_loop = build_agent_loop(stream_llm, &ws.root, &["read_file"]);

    let stream_request = RunTurnRequest {
        input: "Read src/lib.rs".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded.clone()),
    };

    let events: Vec<TurnEvent> = lattice_api::stream_turn(
        stream_session_api,
        stream_agent_loop,
        stream_request,
    )
    .await
    .expect("stream_turn must start")
    .collect()
    .await;

    let stream_store = SqliteSessionGraphStore::new(&stream_sessions_path);
    let stream_obs = observe_stream_turn(&events, &stream_store);

    // Both must produce identical tool_results and final transcript
    assert_eq!(run_obs.status, RunTurnStatus::Completed);
    assert_eq!(stream_obs.status, RunTurnStatus::Completed);
    assert!(!run_obs.tool_results.is_empty(), "should have tool results");
    assert_eq!(
        run_obs, stream_obs,
        "run_turn and stream_turn observations must match for tool-call scenario"
    );
}

// ── Test 4: Live activity ordering (VER-13) ──────────────────────────────

#[tokio::test]
async fn stream_turn_emits_activity_before_commit() {
    let ws = setup_workspace(&[]);

    let streaming_events = vec![
        LlmEvent::Token {
            content: "Hello!".to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            }),
        },
    ];

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_api = SessionApi::sqlite(&sessions_path);
    let llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(streaming_events)
            .build(),
    );
    let agent_loop = build_agent_loop(llm, &ws.root, &[]);

    let request = RunTurnRequest {
        input: "hi".to_string(),
        session_id: None,
        profile: None,
        model: None,
        loaded_profile: Some(ws.loaded.clone()),
    };

    let events: Vec<TurnEvent> = lattice_api::stream_turn(session_api, agent_loop, request)
        .await
        .expect("stream_turn must start")
        .collect()
        .await;

    // Find positions of key events
    let model_started_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::ModelStarted { .. }))
        .expect("must have ModelStarted");

    let token_delta_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TokenDelta { .. }))
        .expect("must have TokenDelta");

    let turn_committing_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitting))
        .expect("must have TurnCommitting");

    let turn_committed_pos = events
        .iter()
        .position(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("must have TurnCommitted");

    // Assert ordering: ModelStarted < TokenDelta < TurnCommitting < TurnCommitted
    assert!(
        model_started_pos < token_delta_pos,
        "ModelStarted (pos {model_started_pos}) must appear BEFORE TokenDelta (pos {token_delta_pos})"
    );
    assert!(
        token_delta_pos < turn_committing_pos,
        "TokenDelta (pos {token_delta_pos}) must appear BEFORE TurnCommitting (pos {turn_committing_pos})"
    );
    assert!(
        turn_committing_pos < turn_committed_pos,
        "TurnCommitting (pos {turn_committing_pos}) must appear BEFORE TurnCommitted (pos {turn_committed_pos})"
    );
}
