// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Full-turn audit rows carry PROV-relevant fields after a live CEP turn.

use std::sync::Arc;

use lancet_api::audit_db_path_for_project_in;
use lancet_authority::policy::CepTurnContext;
use lancet_catalog::methods::audit::{AuditReplay, AuditReplayParams};
use lancet_native::{
    AuditQueryFilters, CepAuditStore, CepTransport, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};
use lancet_protocol::effect_types::ImpactKind;

use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, RunTurnStatus};
use lattice_llm::LlmEvent;
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_workspace::testing::DeterministicWorkspace;
use tempfile::TempDir;

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
            usage: None,
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
            usage: None,
        },
    ]
}

struct Workspace {
    _dir: TempDir,
    _state: TempDir,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
    target_branch: String,
}

fn setup_workspace() -> Workspace {
    let state = TempDir::new().unwrap();
    unsafe {
        std::env::set_var("XDG_STATE_HOME", state.path());
    }
    let ws = TempDir::new().unwrap();
    let root = ws.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }").unwrap();
    init_git_repo(&root);
    let target_branch = create_unattached_target_branch(&root, "lattice-target");

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let enabled = [
        "observe.workspace",
        "exec.command_run",
        "mutate.edit",
    ];
    let toml_content = format!(
        r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "test"
[tools]
enabled = {enabled:?}
[workspace]
image = "test-template"
[lancet.workspace]
denied_write_patterns = [".git/**"]
[lancet.command]
allowed_diagnostic_commands = [["echo"]]
"#
    );
    std::fs::write(profile_dir.join("agent.toml"), toml_content).unwrap();

    let global_agents = TempDir::new().unwrap();
    let paths = DiscoveryPaths {
        project_root: root.clone(),
        global_agents_dir: global_agents.path().to_path_buf(),
    };
    let loaded =
        lattice_profile::resolve_agent_profile(ProfileSource::Default, &paths).expect("profile");

    Workspace {
        _dir: ws,
        _state: state,
        root,
        loaded,
        target_branch,
    }
}

fn init_git_repo(root: &std::path::Path) {
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .expect("git init");
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

fn create_unattached_target_branch(root: &std::path::Path, name: &str) -> String {
    std::process::Command::new("git")
        .args(["branch", name])
        .current_dir(root)
        .status()
        .expect("git branch (create unattached target)");
    name.to_string()
}

#[tokio::test]
async fn full_turn_audit_rows_and_replay_fragment() {
    let ws = setup_workspace();
    std::env::set_current_dir(&ws.root).unwrap();

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
                "c1",
                "observe.workspace",
                r#"{"mode":"read_file","path":"src/lib.rs"}"#,
            ))
            .push_events(tool_call_events(
                "c2",
                "exec.command_run",
                r#"{"argv":["echo","audit-turn"]}"#,
            ))
            .push_events(tool_call_events(
                "c3",
                "mutate.edit",
                &edit_args.to_string(),
            ))
            .push_events(final_response_events("done"))
            .build(),
    );

    let session_dir = TempDir::new().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = lattice_api::SessionApi::git(&sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
            project_branch: ws.target_branch.clone(),
        })
        .expect("create session")
        .session_id;

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::git(&sessions_path),
        RunTurnRequest {
            input: "audit turn provenance".into(),
            session_id,
            loaded_profile: Some(ws.loaded.clone()),
            cancel_flag: None,
            test_llm: Some(fake_llm),
            test_project_root: Some(ws.root.to_string_lossy().to_string()),
            test_persist_audit: Some(false),
            workspace_override: Some(Arc::new(
                DeterministicWorkspace::new().expect("deterministic workspace"),
            )),
        },
    )
    .await
    .expect("run_turn");

    assert_eq!(result.status, RunTurnStatus::Completed);

    let audit_path = audit_db_path_for_project_in(ws._state.path(), &ws.root).expect("audit path");
    assert!(audit_path.exists(), "audit db must be persisted on disk");

    let audit = CepAuditStore::open(&audit_path).expect("open audit");
    let rows = audit
        .query_rows(&AuditQueryFilters {
            session_id: Some(result.session_id.clone()),
            ..Default::default()
        })
        .expect("query rows");

    let tool_rows: Vec<_> = rows
        .iter()
        .filter(|r| !r.method.starts_with("audit."))
        .collect();
    assert!(
        tool_rows.len() >= 3,
        "expected read + command + mutation calls, got {}",
        tool_rows.len()
    );

    for row in &tool_rows {
        assert!(!row.call_id.to_string().is_empty());
        assert!(!row.method.is_empty());
        assert!(!row.impact_kind.is_empty());
        assert!(!row.turn_id.is_empty());
        assert_eq!(row.session_id, result.session_id);
        assert!(
            row.model_id.as_deref().is_some_and(|m| !m.is_empty()),
            "model_id required on row {}",
            row.audit_id
        );
        // Mutation rows no longer carry routing_upper_workspace_id since
        // the envelope system was removed; skip that check.
        let _ = row;
    }

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("registry");
    let root = ws.root.to_str().unwrap();
    let mut config = ServerRunConfig::in_memory_for_tests();
    config.audit = Arc::new(audit);
    config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
    {
        let mut ctx = config.audit_context.write().await;
        ctx.session_id = result.session_id.clone();
    }

    let (client, server) = CepTransport::pair(32);
    let server_task = tokio::spawn(async move {
        server.run(registry, config).await.unwrap();
    });

    let turn_id = tool_rows[0].turn_id.clone();
    let replay = client
        .call::<AuditReplay>(AuditReplayParams { turn_id })
        .await
        .expect("observe.audit_replay");

    assert_eq!(
        replay.trace.fragment.activities.len(),
        tool_rows.len(),
        "replay activity count should match tool audit rows"
    );
    assert!(
        replay
            .trace
            .fragment
            .activities
            .iter()
            .any(|a| { !a.method.is_empty() && a.impact_kind != ImpactKind::AuditObservation }),
        "replay fragment must include tool activities"
    );

    drop(client);
    server_task.abort();
}
