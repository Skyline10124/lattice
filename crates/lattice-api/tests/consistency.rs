// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Run/stream consistency integration tests.
//!
//! Proves `run_turn` is equivalent to collecting a `stream_turn` event stream
//! via [`collect_turn_result_from_events`], with durable session-store anchors.

mod support;

use std::sync::Arc;

use futures::StreamExt;
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{
    RunTurnRequest, RunTurnResult, RunTurnStatus, SessionApi, TurnEvent, TurnUsageSummary,
    collect_turn_result_from_events, run_turn, stream_turn,
};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{GitSessionGraphStore, SessionGraphStore, StateLayout, WorkspaceDisposition};

fn session_layout(sessions_path: &std::path::Path) -> StateLayout {
    let state_root = sessions_path
        .parent()
        .expect("session path has a parent")
        .join("state");
    StateLayout::in_state_root(&state_root)
}

fn create_session_store(sessions_path: impl AsRef<std::path::Path>) -> SessionApi {
    let sessions_path = sessions_path.as_ref();
    let layout = session_layout(sessions_path);
    SessionApi::git_create_with_layout(sessions_path, &layout).expect("create session store")
}

fn store_key_hex(root: &std::path::Path) -> String {
    lattice_api::LocalWorkspaceRoot::from_path(root)
        .expect("resolve workspace root")
        .store_key_hex()
}

#[derive(Debug, PartialEq)]
struct DurableTurnObservation {
    status: RunTurnStatus,
    disposition: WorkspaceDisposition,
    usage: Option<TurnUsageSummary>,
    assistant_output: String,
    terminal_reason: Option<String>,
}

fn last_terminal_reason(session_store: &GitSessionGraphStore, session_id: &str) -> Option<String> {
    session_store
        .log(session_id)
        .expect("log after commit")
        .last()
        .and_then(|entry| entry.terminal_reason.clone())
}

fn last_usage(session_store: &GitSessionGraphStore, session_id: &str) -> Option<TurnUsageSummary> {
    session_store
        .log(session_id)
        .expect("log after commit")
        .last()
        .and_then(|entry| entry.usage)
        .map(TurnUsageSummary::from)
        .and_then(|usage| {
            if usage.prompt_tokens == 0 && usage.completion_tokens == 0 && usage.total_tokens == 0 {
                None
            } else {
                Some(usage)
            }
        })
}

fn observe_durable_turn(
    session_store: &GitSessionGraphStore,
    session_id: &str,
    status: RunTurnStatus,
) -> DurableTurnObservation {
    let transcript = session_store
        .head_transcript(session_id)
        .expect("head_transcript after commit");

    let assistant_output = transcript
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant")
        .map(|m| m.content.clone())
        .unwrap_or_default();

    let disposition = session_store
        .log(session_id)
        .expect("log after commit")
        .last()
        .map(|entry| entry.disposition.clone())
        .unwrap_or(WorkspaceDisposition::NotApplicable);

    DurableTurnObservation {
        status,
        disposition,
        usage: last_usage(session_store, session_id),
        assistant_output,
        terminal_reason: last_terminal_reason(session_store, session_id),
    }
}

struct Workspace {
    _dir: tempfile::TempDir,
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

fn workspace_toml_suffix(_enabled_tools: &[&str]) -> &'static str {
    r#"
[lancet.workspace]
denied_write_patterns = [".git/**"]
"#
}

fn setup_workspace(enabled_tools: &[&str]) -> Workspace {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn hello() {}").unwrap();
    support::init_git_repo(&root);

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
enabled = {enabled:?}{workspace}
"#,
        workspace = workspace_toml_suffix(enabled_tools),
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

fn create_default_session(
    sessions_path: &std::path::Path,
    project_root: &std::path::Path,
) -> String {
    create_session_store(sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
            project_branch: "main".to_string(),
            project_store_key: store_key_hex(project_root),
        })
        .expect("create session")
        .session_id
}

fn make_request(
    ws: &Workspace,
    input: &str,
    session_id: &str,
    llm: Arc<dyn LlmClient>,
) -> RunTurnRequest {
    // Deliberately does NOT `set_current_dir`. `test_project_root` below already
    // pins the root, so `resolve_project_root` never consults the CWD — but the
    // CWD is process-global while the five `#[tokio::test]`s in this binary run
    // concurrently, each owning its own `TempDir`. Pointing the process at one
    // test's temp dir let a sibling's `TempDir` drop leave the process with no
    // valid CWD, and `gix` reads the CWD even for absolute paths, so unrelated
    // tests failed with `Could not obtain the current directory` (~5% locally,
    // and it took down both CI lanes). The redundant mutation is the whole
    // hazard, so it is gone.
    RunTurnRequest {
        input: input.to_string(),
        session_id: session_id.to_string(),
        loaded_profile: Some(ws.loaded.clone()),
        cancel_flag: None,
        test_llm: Some(llm),
        test_project_root: Some(ws.root.to_string_lossy().to_string()),
        test_persist_audit: None,
    }
}

fn llm_with_events(llm_events: Vec<LlmEvent>) -> Arc<dyn LlmClient> {
    Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(llm_events)
            .build(),
    )
}

fn llm_with_rounds(rounds: Vec<Vec<LlmEvent>>) -> Arc<dyn LlmClient> {
    let mut builder = FakeStreamingLlmClient::builder();
    for round in rounds {
        builder = builder.push_events(round);
    }
    Arc::new(builder.build())
}

async fn assert_run_equals_collector(
    ws: &Workspace,
    llm_events: Vec<LlmEvent>,
    _enabled_tools: &[&str],
    input: &str,
) {
    let stream_dir = tempfile::tempdir().unwrap();
    let stream_sessions_path = stream_dir.path().join("sessions");
    let stream_store =
        GitSessionGraphStore::create(&stream_sessions_path).expect("create session store");
    let stream_session = create_session_store(&stream_sessions_path);
    let stream_session_id = create_default_session(&stream_sessions_path, &ws.root);
    let stream_request = make_request(
        ws,
        input,
        &stream_session_id,
        llm_with_events(llm_events.clone()),
    );

    let events: Vec<TurnEvent> = stream_turn(stream_session, stream_request)
        .await
        .expect("stream_turn must start")
        .collect()
        .await;

    let collected =
        collect_turn_result_from_events(events).expect("collector must finish on committed stream");

    let run_dir = tempfile::tempdir().unwrap();
    let run_sessions_path = run_dir.path().join("sessions");
    let run_session = create_session_store(&run_sessions_path);
    let run_session_id = create_default_session(&run_sessions_path, &ws.root);
    let run_request = make_request(ws, input, &run_session_id, llm_with_events(llm_events));
    let run_result = run_turn(run_session, run_request)
        .await
        .expect("run_turn must succeed");

    assert_run_results_equivalent(&collected, &run_result);

    let durable = observe_durable_turn(
        &stream_store,
        &collected.session_id,
        collected.status.clone(),
    );
    assert_eq!(durable.status, collected.status);
    assert_eq!(
        durable.disposition, collected.disposition,
        "durable and collected disposition must match"
    );
    assert_eq!(durable.assistant_output, collected.assistant_output);
    assert_eq!(durable.usage, collected.usage);
}

fn assert_run_results_equivalent(collected: &RunTurnResult, run_result: &RunTurnResult) {
    assert_eq!(collected.status, run_result.status);
    assert_eq!(
        collected.disposition, run_result.disposition,
        "streamed and synchronous disposition must be identical"
    );
    assert_eq!(collected.assistant_output, run_result.assistant_output);
    assert_eq!(collected.usage, run_result.usage);
    assert_eq!(collected.tool_results, run_result.tool_results);
    assert_eq!(collected.stages, run_result.stages);
    assert!(
        !run_result.session_id.is_empty(),
        "run_turn must return a session_id"
    );
    assert!(
        !run_result.commit_id.is_empty(),
        "run_turn must return a commit_id"
    );
}

#[tokio::test]
async fn consistency_completed_content_only() {
    let ws = setup_workspace(&[]);
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

    assert_run_equals_collector(&ws, streaming_events, &[], "hi").await;
}

#[tokio::test]
async fn consistency_error_after_partial_token() {
    let ws = setup_workspace(&[]);
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

    let stream_dir = tempfile::tempdir().unwrap();
    let stream_sessions_path = stream_dir.path().join("sessions");
    let stream_store =
        GitSessionGraphStore::create(&stream_sessions_path).expect("create session store");
    let stream_session = create_session_store(&stream_sessions_path);
    let stream_session_id = create_default_session(&stream_sessions_path, &ws.root);
    let stream_request = make_request(
        &ws,
        "hello",
        &stream_session_id,
        llm_with_events(streaming_events.clone()),
    );

    let events: Vec<TurnEvent> = stream_turn(stream_session, stream_request)
        .await
        .expect("stream_turn must start")
        .collect()
        .await;

    let collected = collect_turn_result_from_events(events.clone()).unwrap();

    let run_dir = tempfile::tempdir().unwrap();
    let run_sessions_path = run_dir.path().join("sessions");
    let run_session = create_session_store(&run_sessions_path);
    let run_session_id = create_default_session(&run_sessions_path, &ws.root);
    let run_request = make_request(
        &ws,
        "hello",
        &run_session_id,
        llm_with_events(streaming_events),
    );
    let run_result = run_turn(run_session, run_request).await.unwrap();

    assert_run_results_equivalent(&collected, &run_result);
    assert_eq!(run_result.status, RunTurnStatus::Error);
    assert_eq!(collected.assistant_output, "partial resp");

    let event_reason = events.iter().find_map(|e| match e {
        TurnEvent::TurnFailed { reason } => Some(reason.clone()),
        _ => None,
    });
    let durable = observe_durable_turn(
        &stream_store,
        &collected.session_id,
        collected.status.clone(),
    );
    assert_eq!(event_reason, durable.terminal_reason);
}

#[tokio::test]
async fn consistency_tool_call_path() {
    let ws = setup_workspace(&["observe.workspace"]);

    let round1_events = vec![
        LlmEvent::ToolCallStart {
            id: "call_1".to_string(),
            name: "observe.workspace".to_string(),
        },
        LlmEvent::ToolCallDelta {
            id: "call_1".to_string(),
            arguments_delta: r#"{"mode":"read_file","path":"src/lib.rs"}"#.to_string(),
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

    let all_events = vec![round1_events.clone(), round2_events.clone()];
    let stream_dir = tempfile::tempdir().unwrap();
    let stream_sessions_path = stream_dir.path().join("sessions");
    let stream_store =
        GitSessionGraphStore::create(&stream_sessions_path).expect("create session store");
    let stream_session = create_session_store(&stream_sessions_path);
    let stream_session_id = create_default_session(&stream_sessions_path, &ws.root);
    let stream_request = make_request(
        &ws,
        "Read src/lib.rs",
        &stream_session_id,
        llm_with_rounds(all_events.clone()),
    );

    let events: Vec<TurnEvent> = stream_turn(stream_session, stream_request)
        .await
        .expect("stream_turn must start")
        .collect()
        .await;

    let collected = collect_turn_result_from_events(events).unwrap();

    let run_dir = tempfile::tempdir().unwrap();
    let run_sessions_path = run_dir.path().join("sessions");
    let run_session = create_session_store(&run_sessions_path);
    let run_session_id = create_default_session(&run_sessions_path, &ws.root);
    let run_request = make_request(
        &ws,
        "Read src/lib.rs",
        &run_session_id,
        llm_with_rounds(all_events),
    );
    let run_result = run_turn(run_session, run_request).await.unwrap();

    assert_run_results_equivalent(&collected, &run_result);
    assert_eq!(run_result.status, RunTurnStatus::Completed);
    assert!(!run_result.tool_results.is_empty());

    let durable = observe_durable_turn(&stream_store, &collected.session_id, collected.status);
    assert_eq!(durable.assistant_output, "File contents read successfully.");
}

/// Parity: completed turns have matching disposition across stream, run, and durable.
#[tokio::test]
async fn consistency_completed_disposition_parity() {
    let ws = setup_workspace(&[]);
    let streaming_events = vec![
        LlmEvent::Token {
            content: "hello".to_string(),
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

    let stream_dir = tempfile::tempdir().unwrap();
    let stream_sessions_path = stream_dir.path().join("sessions");
    let stream_store =
        GitSessionGraphStore::create(&stream_sessions_path).expect("create session store");
    let stream_session = create_session_store(&stream_sessions_path);
    let stream_session_id = create_default_session(&stream_sessions_path, &ws.root);
    let stream_request = make_request(
        &ws,
        "hi",
        &stream_session_id,
        llm_with_events(streaming_events.clone()),
    );

    let events: Vec<TurnEvent> = stream_turn(stream_session, stream_request)
        .await
        .expect("stream_turn must start")
        .collect()
        .await;

    let collected = collect_turn_result_from_events(events.clone()).unwrap();
    assert_eq!(collected.status, RunTurnStatus::Completed);
    assert_eq!(collected.disposition, WorkspaceDisposition::NotApplicable);

    // Verify TurnCommitted event has disposition
    let committed = events
        .iter()
        .find(|e| matches!(e, TurnEvent::TurnCommitted { .. }))
        .expect("must have TurnCommitted");
    if let TurnEvent::TurnCommitted {
        disposition,
        status,
        ..
    } = committed
    {
        assert_eq!(*status, RunTurnStatus::Completed);
        assert_eq!(*disposition, WorkspaceDisposition::NotApplicable);
    }

    let run_dir = tempfile::tempdir().unwrap();
    let run_sessions_path = run_dir.path().join("sessions");
    let run_session = create_session_store(&run_sessions_path);
    let run_session_id = create_default_session(&run_sessions_path, &ws.root);
    let run_request = make_request(
        &ws,
        "hi",
        &run_session_id,
        llm_with_events(streaming_events),
    );
    let run_result = run_turn(run_session, run_request).await.unwrap();

    assert_run_results_equivalent(&collected, &run_result);
    assert_eq!(run_result.disposition, WorkspaceDisposition::NotApplicable);

    let durable = observe_durable_turn(&stream_store, &collected.session_id, collected.status);
    assert_eq!(durable.disposition, collected.disposition);
}

/// Parity: error turns have matching disposition across stream, run, and durable.
#[tokio::test]
async fn consistency_error_disposition_parity() {
    let ws = setup_workspace(&[]);
    let streaming_events = vec![
        LlmEvent::Token {
            content: "partial".to_string(),
        },
        LlmEvent::Error {
            message: "provider error".to_string(),
        },
    ];

    let stream_dir = tempfile::tempdir().unwrap();
    let stream_sessions_path = stream_dir.path().join("sessions");
    let stream_store =
        GitSessionGraphStore::create(&stream_sessions_path).expect("create session store");
    let stream_session = create_session_store(&stream_sessions_path);
    let stream_session_id = create_default_session(&stream_sessions_path, &ws.root);
    let stream_request = make_request(
        &ws,
        "hello",
        &stream_session_id,
        llm_with_events(streaming_events.clone()),
    );

    let events: Vec<TurnEvent> = stream_turn(stream_session, stream_request)
        .await
        .expect("stream_turn must start")
        .collect()
        .await;

    let collected = collect_turn_result_from_events(events.clone()).unwrap();
    assert_eq!(collected.status, RunTurnStatus::Error);
    assert_eq!(collected.disposition, WorkspaceDisposition::Aborted);

    let run_dir = tempfile::tempdir().unwrap();
    let run_sessions_path = run_dir.path().join("sessions");
    let run_session = create_session_store(&run_sessions_path);
    let run_session_id = create_default_session(&run_sessions_path, &ws.root);
    let run_request = make_request(
        &ws,
        "hello",
        &run_session_id,
        llm_with_events(streaming_events),
    );
    let run_result = run_turn(run_session, run_request).await.unwrap();

    assert_run_results_equivalent(&collected, &run_result);
    assert_eq!(run_result.disposition, WorkspaceDisposition::Aborted);

    let durable = observe_durable_turn(&stream_store, &collected.session_id, collected.status);
    assert_eq!(durable.disposition, collected.disposition);
}
