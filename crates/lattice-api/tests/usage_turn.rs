// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Deterministic usage tests for the main turn path.
//!
//! These tests exercise the real `run_turn` → live `stream_turn` collector →
//! durable coordinator saga with a fake streaming LLM.
//! No network, no real API keys.

mod support;

use std::sync::Arc;

use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::RunTurnRequest;
use lattice_conversation::MessageRole;
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{GitSessionGraphStore, SessionGraphStore, StateLayout};

fn session_layout(sessions_path: &std::path::Path) -> StateLayout {
    let state_root = sessions_path
        .parent()
        .expect("session path has a parent")
        .join("state");
    StateLayout::in_state_root(&state_root)
}

fn session_repo(sessions_path: &std::path::Path) -> std::path::PathBuf {
    session_layout(sessions_path)
        .lattice_root()
        .join("sessions.git")
}
fn create_session_store(sessions_path: impl AsRef<std::path::Path>) -> lattice_api::SessionApi {
    let sessions_path = sessions_path.as_ref();
    let layout = session_layout(sessions_path);
    lattice_api::SessionApi::git_create_with_layout(session_repo(sessions_path), &layout)
        .expect("create session store")
}

fn open_session_store(sessions_path: &std::path::Path) -> lattice_api::SessionApi {
    let layout = session_layout(sessions_path);
    lattice_api::SessionApi::git_open_with_layout(session_repo(sessions_path), &layout)
        .expect("open session store")
}

fn store_key_hex(root: &std::path::Path) -> String {
    lattice_api::LocalWorkspaceRoot::from_path(root)
        .expect("resolve workspace root")
        .store_key_hex()
}

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

/// Temp workspace with src/lib.rs and a default agent profile.
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

fn setup_workspace(src_content: &str, enabled_tools: &[&str]) -> Workspace {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), src_content).unwrap();
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

/// Create an unattached branch at the current HEAD without checking it out.
fn create_unattached_target_branch(root: &std::path::Path, name: &str) -> String {
    let mut repo = gix::open_opts(root, gix::open::Options::isolated()).expect("open repo");
    // Isolated repos have no identity; inject a fixture committer for the reflog.
    {
        let mut cfg = repo.config_snapshot_mut();
        cfg.set_raw_value("user.name", "lattice-test")
            .expect("set name");
        cfg.set_raw_value("user.email", "test@lattice.local")
            .expect("set email");
        let _ = cfg.commit();
    }
    let head = repo
        .head_id()
        .expect("HEAD id for unattached branch")
        .detach();
    repo.reference(
        format!("refs/heads/{name}"),
        head,
        gix::refs::transaction::PreviousValue::MustNotExist,
        "create unattached target branch",
    )
    .expect("create unattached target branch");
    name.to_string()
}

fn create_session_with_branch(
    sessions_path: &std::path::Path,
    branch: &str,
    project_root: &std::path::Path,
) -> String {
    create_session_store(sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
            project_branch: branch.to_string(),
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
    // No `set_current_dir` here: `test_project_root` already pins the root, and
    // mutating the process-global CWD from concurrent tests made sibling tests
    // fail once a `TempDir` dropped (see the note in `tests/consistency.rs`).
    RunTurnRequest {
        input: input.to_string(),
        session_id: session_id.to_string(),
        loaded_profile: Some(ws.loaded.clone()),
        cancel_flag: None,
        test_llm: Some(llm),
        test_project_root: Some(ws.root.to_string_lossy().to_string()),
    }
}

// ── Test 2: edit workspace file and commit ──────────────────────────────────

#[tokio::test]
async fn usage_run_turn_can_edit_workspace_file_and_commit() {
    let ws = setup_workspace("pub fn answer() -> i32 { 1 }", &["mutate.edit"]);

    let original = "pub fn answer() -> i32 { 1 }";
    let hash = format!("blake3:{}", blake3::hash(original.as_bytes()).to_hex());
    let edit_args = serde_json::json!({
        "path": "src/lib.rs",
        "expected_hash": hash,
        "edits": [{
            "op": "replace_text",
            "old": original,
            "new": "pub fn answer() -> i32 { 42 }"
        }]
    });
    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            .push_events(tool_call_events(
                "call_1",
                "mutate.edit",
                &edit_args.to_string(),
            ))
            .push_events(final_response_events("Edited successfully."))
            .build(),
    );
    let target = create_unattached_target_branch(&ws.root, "lattice-target-edit");
    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_session_with_branch(&sessions_path, &target, &ws.root);
    let result = lattice_api::run_turn(
        open_session_store(&sessions_path),
        make_request(&ws, "Change the answer to 42", &session_id, fake_llm),
    )
    .await
    .expect("run_turn must succeed");

    let edit_result = result
        .tool_results
        .iter()
        .find(|t| t.name == "mutate.edit")
        .expect("mutate.edit tool result");
    assert!(
        edit_result.ok,
        "mutate.edit must succeed in the Local Workspace: {:?}",
        edit_result.error_summary
    );

    assert!(matches!(
        result.outcome,
        lattice_session::TurnOutcome::Completed { .. }
    ));

    let verify_api = open_session_store(&sessions_path);
    let log = verify_api
        .log(&result.session_id)
        .expect("log must succeed");
    assert_eq!(log.len(), 1);

    assert!(
        result
            .tool_results
            .iter()
            .any(|t| t.name == "mutate.edit" && t.ok),
        "should have a successful mutate.edit tool result"
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

    let target = create_unattached_target_branch(&ws.root, "lattice-target-resume");
    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = create_session_with_branch(&sessions_path, &target, &ws.root);

    let result1 = lattice_api::run_turn(
        open_session_store(&sessions_path),
        make_request(&ws, "Hello", &session_id, fake_llm.clone()),
    )
    .await
    .expect("turn 1 must succeed");
    assert!(matches!(
        result1.outcome,
        lattice_session::TurnOutcome::Completed { .. }
    ));
    let request2 = make_request(&ws, "Follow up", &session_id, fake_llm);
    let result2 = lattice_api::run_turn(open_session_store(&sessions_path), request2)
        .await
        .expect("turn 2 must succeed");
    assert!(matches!(
        result2.outcome,
        lattice_session::TurnOutcome::Completed { .. }
    ));

    let store =
        GitSessionGraphStore::open(session_repo(&sessions_path)).expect("open session store");
    let transcript = store
        .head_transcript(&session_id)
        .expect("head_transcript after turn 2");
    assert!(
        transcript
            .messages
            .iter()
            .any(|message| message.role == MessageRole::User
                && lattice_session::test_helpers::text(message) == Some("Hello")),
        "transcript must include first turn user message"
    );
    assert!(
        transcript
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Assistant
                && lattice_session::test_helpers::text(message) == Some("First turn done.")),
        "transcript must include first turn assistant message"
    );

    let verify_api = open_session_store(&sessions_path);
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
