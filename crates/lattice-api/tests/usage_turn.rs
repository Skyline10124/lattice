//! Deterministic usage tests for the main turn path.
//!
//! These tests exercise the real `run_turn` → live `stream_turn` collector →
//! `AgentLoop` CEP adapter pipeline with a fake streaming LLM.
//! No network, no real API keys.

use std::collections::BTreeSet;
use std::sync::Arc;

use futures::StreamExt;
use lancet_protocol::envelope::{
    Capability, Constraints, ExecutionEnvelope, Impact, ImpactConfidence, ImpactKind, Intent,
    OnFailure, Scope,
};
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::LlmClient;
use lattice_api::{RunTurnRequest, RunTurnStatus, TurnEvent};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{SessionGraphStore, SqliteSessionGraphStore};

// ── Streaming event helpers ─────────────────────────────────────────────────

fn usage_tokens() -> TokenUsage {
    TokenUsage {
        prompt_tokens: 10,
        completion_tokens: 5,
        total_tokens: 15,
    }
}

fn tool_call_events(id: &str, name: &str, args: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
        },
        LlmEvent::ToolCallDelta {
            id: id.to_string(),
            arguments_delta: args.to_string(),
        },
        LlmEvent::ToolCallEnd { id: id.to_string() },
        LlmEvent::Done {
            finish_reason: "tool_calls".to_string(),
            usage: Some(usage_tokens()),
        },
    ]
}

fn final_response_events(text: &str) -> Vec<LlmEvent> {
    vec![
        LlmEvent::Token {
            content: text.to_string(),
        },
        LlmEvent::Done {
            finish_reason: "stop".to_string(),
            usage: Some(usage_tokens()),
        },
    ]
}

fn line_hash(line_number: usize, line: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lattice.hashline.v2\0");
    hasher.update(&(line_number as u64).to_be_bytes());
    hasher.update(b"\0");
    hasher.update(line.as_bytes());
    let digest = hasher.finalize();
    let hex = digest.to_hex();
    format!("blake3:{}", &hex[..4])
}

/// Temp workspace with src/lib.rs and a default agent profile.
struct Workspace {
    _dir: tempfile::TempDir,
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

fn setup_workspace(src_content: &str, enabled_tools: &[&str]) -> Workspace {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), src_content).unwrap();

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

fn create_default_session(sessions_path: &std::path::Path) -> String {
    lattice_api::SessionApi::sqlite(sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
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
    std::env::set_current_dir(&ws.root).expect("set workspace cwd");
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

// ── Test 1: read workspace file through default profile ─────────────────────

#[tokio::test]
async fn usage_run_turn_can_read_workspace_file_with_default_profile() {
    let ws = setup_workspace("pub fn answer() -> i32 { 42 }", &["observe.workspace"]);

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "call_1",
                "observe.workspace",
                r#"{"mode":"read_file","path":"src/lib.rs"}"#,
            ))
            .push_events(final_response_events("Read workspace file successfully."))
            .build(),
    );

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_default_session(&sessions_path);

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        make_request(&ws, "Read src/lib.rs", &session_id, fake_llm),
    )
    .await
    .expect("run_turn must succeed");

    assert_eq!(result.status, RunTurnStatus::Completed);
    assert!(
        result
            .assistant_output
            .contains("Read workspace file successfully"),
        "assistant output: {}",
        result.assistant_output
    );

    let tool_result = result
        .tool_results
        .iter()
        .find(|t| t.name == "observe.workspace")
        .expect("should have an observe.workspace tool result");
    assert!(tool_result.output_len > 0, "tool output must be non-empty");

    let store = SqliteSessionGraphStore::new(&sessions_path);
    let transcript = store
        .head_transcript(&result.session_id)
        .expect("head_transcript");
    assert!(
        transcript
            .messages
            .iter()
            .any(|m| m.role == "tool" && m.content.contains("pub fn answer")),
        "committed tool message must contain workspace file content"
    );

    let verify_api = lattice_api::SessionApi::sqlite(&sessions_path);
    let log = verify_api
        .log(&result.session_id)
        .expect("log must succeed");
    assert_eq!(log.len(), 1, "session log should have exactly one commit");
}

#[tokio::test]
async fn usage_stream_turn_model_started_includes_routed_provider() {
    let ws = setup_workspace("pub fn answer() -> i32 { 42 }", &[]);

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(final_response_events("Done."))
            .build(),
    );

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_default_session(&sessions_path);

    let events: Vec<TurnEvent> = lattice_api::stream_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        make_request(&ws, "Hello", &session_id, fake_llm),
    )
    .await
    .expect("stream_turn must start")
    .collect()
    .await;

    let model_started = events
        .iter()
        .find_map(|event| match event {
            TurnEvent::ModelStarted { provider, model } => Some((provider, model)),
            _ => None,
        })
        .expect("stream should include ModelStarted");

    assert_eq!(model_started.0.as_deref(), Some("anthropic"));
    assert_eq!(model_started.1, "sonnet");
}

// ── Test: profile-disabled exec.command_run is rejected before CEP dispatch ─────

#[tokio::test]
async fn rejects_profile_disabled_command_run() {
    let ws = setup_workspace("pub fn answer() -> i32 { 42 }", &["observe.workspace"]);

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "call_1",
                "exec.command_run",
                r#"{"argv":["echo","blocked"]}"#,
            ))
            .push_events(final_response_events(
                "Should not execute exec.command_run.",
            ))
            .build(),
    );

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_default_session(&sessions_path);

    let events: Vec<TurnEvent> = lattice_api::stream_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        make_request(&ws, "Try exec.command_run", &session_id, fake_llm),
    )
    .await
    .expect("stream_turn must start")
    .collect()
    .await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            TurnEvent::ToolCallFinished {
                name,
                ok: false,
                ..
            } if name == "exec.command_run"
        )),
        "disabled exec.command_run must surface as a failed ToolCallFinished"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            TurnEvent::CepCallStarted { method, .. } if method == "exec.command_run"
        )),
        "disabled exec.command_run must not emit CepCallStarted"
    );
}

// ── Test 2: patch workspace file and commit ─────────────────────────────────

#[tokio::test]
async fn usage_run_turn_can_patch_workspace_file_and_commit() {
    let ws = setup_workspace(
        "pub fn answer() -> i32 { 1 }",
        &["observe.workspace", "session.envelope", "mutate.patch"],
    );

    let original = "pub fn answer() -> i32 { 1 }";
    let hash = format!("blake3:{}", blake3::hash(original.as_bytes()).to_hex());
    let line_hash = line_hash(1, original);

    let patch_doc = format!(
        "\
*** Begin Patch
*** Update File: src/lib.rs
*** Old Hash: {hash}
@@
-{line_hash}\tpub fn answer() -> i32 {{ 1 }}
+pub fn answer() -> i32 {{ 42 }}
*** End Patch
"
    );
    let mut capabilities = BTreeSet::new();
    capabilities.insert(Capability::WritePath {
        path_pattern: "src/**".into(),
    });
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "update answer implementation".into(),
        },
        capabilities,
        scope: Scope::default(),
        impact: vec![Impact {
            kind: ImpactKind::WorkspaceMutation,
            target: "src/lib.rs".into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        verify: None,
        on_failure: Some(OnFailure::RetainDiff),
        extensions: serde_json::Map::new(),
    };
    let declare_args = serde_json::json!({
        "mode": "declare",
        "envelope": envelope,
        "reason": "authorize usage test patch"
    });

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "call_0",
                "session.envelope",
                &declare_args.to_string(),
            ))
            .push_events(tool_call_events(
                "call_1",
                "mutate.patch",
                &format!(
                    r#"{{"patch":{}}}"#,
                    serde_json::to_string(&patch_doc).unwrap()
                ),
            ))
            .push_events(final_response_events("Patched successfully."))
            .build(),
    );
    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_default_session(&sessions_path);
    let result = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        make_request(&ws, "Change the answer to 42", &session_id, fake_llm),
    )
    .await
    .expect("run_turn must succeed");

    let patched = std::fs::read_to_string(ws.root.join("src/lib.rs")).unwrap();
    assert!(
        patched.contains("42"),
        "file should contain 42, got: {patched}"
    );
    assert!(!patched.contains("{ 1 }"), "old value should be gone");

    assert_eq!(result.status, RunTurnStatus::Completed);

    let verify_api = lattice_api::SessionApi::sqlite(&sessions_path);
    let log = verify_api
        .log(&result.session_id)
        .expect("log must succeed");
    assert_eq!(log.len(), 1);

    let declare_result = result
        .tool_results
        .iter()
        .find(|t| t.name == "session.envelope")
        .expect("should have a session.envelope tool result");
    assert!(
        declare_result.ok,
        "session.envelope must succeed before patching; error: {:?}",
        declare_result.error_summary
    );

    assert!(
        result.tool_results.iter().any(|t| t.name == "mutate.patch"),
        "should have a mutate.patch tool result"
    );
}

// ── Test 3: resume session includes previous transcript ─────────────────────

#[tokio::test]
async fn usage_resume_session_includes_previous_turn_context() {
    let ws = setup_workspace("pub fn answer() -> i32 { 42 }", &["observe.workspace"]);

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(final_response_events("First turn done."))
            .push_events(final_response_events("Second turn done."))
            .build(),
    );

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_default_session(&sessions_path);

    let result1 = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        make_request(&ws, "Hello", &session_id, fake_llm.clone()),
    )
    .await
    .expect("turn 1 must succeed");
    assert_eq!(result1.status, RunTurnStatus::Completed);
    let request2 = make_request(&ws, "Follow up", &session_id, fake_llm);
    let result2 = lattice_api::run_turn(lattice_api::SessionApi::sqlite(&sessions_path), request2)
        .await
        .expect("turn 2 must succeed");
    assert_eq!(result2.status, RunTurnStatus::Completed);

    let store = SqliteSessionGraphStore::new(&sessions_path);
    let transcript = store
        .head_transcript(&session_id)
        .expect("head_transcript after turn 2");
    assert!(
        transcript
            .messages
            .iter()
            .any(|m| m.role == "user" && m.content == "Hello"),
        "transcript must include first turn user message"
    );
    assert!(
        transcript
            .messages
            .iter()
            .any(|m| m.role == "assistant" && m.content == "First turn done."),
        "transcript must include first turn assistant message"
    );

    let verify_api = lattice_api::SessionApi::sqlite(&sessions_path);
    let log = verify_api.log(&session_id).expect("log must succeed");
    assert_eq!(log.len(), 2, "session log should have 2 commits");

    let second = &log[1];
    assert!(
        second.parent_ids.contains(&result1.commit_id),
        "second commit parent should include first commit"
    );

    let status = verify_api.status(&session_id).expect("status must succeed");
    assert_eq!(status.commit_count, 2, "status should report 2 commits");
    assert!(
        matches!(status.head, lattice_session::GraphHead::Branch { name } if name == "main"),
        "HEAD should be on main branch"
    );
}

// ── CEP: network policy without FetchUrl("*") ──────────────────────────

fn setup_workspace_with_cep_network(
    src_content: &str,
    enabled_tools: &[&str],
    network_toml: &str,
) -> Workspace {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), src_content).unwrap();

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
{network_toml}
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

#[tokio::test]
async fn public_https_policy_requires_fetch_capability() {
    let ws = setup_workspace_with_cep_network(
        "fn main() {}",
        &["fetch.web"],
        "[lancet.network]\nallow_public_https = true\nallow_localhost = false\n",
    );

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "call_1",
                "fetch.web",
                r#"{"url":"https://example.com/"}"#,
            ))
            .push_events(final_response_events("Fetched public HTTPS."))
            .build(),
    );

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_default_session(&sessions_path);

    let events: Vec<TurnEvent> = lattice_api::stream_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        make_request(&ws, "Fetch example.com", &session_id, fake_llm),
    )
    .await
    .expect("stream_turn must start")
    .collect()
    .await;

    assert!(
        events
            .iter()
            .any(|event| matches!(event, TurnEvent::TurnCommitted { status, .. } if *status == RunTurnStatus::Completed)),
        "turn must complete"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            TurnEvent::ToolCallFinished {
                name,
                ok: false,
                ..
            } if name == "fetch.web"
        )),
        "fetch.web must be denied without FetchUrl capability even with allow_public_https"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            TurnEvent::CepCallStarted {
                method,
                impact_kind,
                semantic_stage,
                ..
            } if method == "fetch.web"
                && impact_kind == "external_observation"
                && semantic_stage == "cep_native"
        )),
        "native fetch.web must emit CEP lifecycle with external_observation impact"
    );
}

#[tokio::test]
async fn rejects_profile_disabled_web_fetch() {
    let ws = setup_workspace("pub fn answer() -> i32 { 42 }", &["observe.workspace"]);

    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "call_1",
                "fetch.web",
                r#"{"url":"https://example.com/"}"#,
            ))
            .push_events(final_response_events("Should not fetch."))
            .build(),
    );

    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_default_session(&sessions_path);

    let events: Vec<TurnEvent> = lattice_api::stream_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
        make_request(&ws, "Try fetch.web", &session_id, fake_llm),
    )
    .await
    .expect("stream_turn must start")
    .collect()
    .await;

    assert!(
        events.iter().any(|event| matches!(
            event,
            TurnEvent::ToolCallFinished {
                name,
                ok: false,
                ..
            } if name == "fetch.web"
        )),
        "disabled fetch.web must surface as failed ToolCallFinished"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            TurnEvent::CepCallStarted { method, .. } if method == "fetch.web"
        )),
        "disabled fetch.web must not emit CepCallStarted"
    );
}
