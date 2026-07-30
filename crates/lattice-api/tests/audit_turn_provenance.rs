// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Full-turn audit rows carry PROV-relevant fields after a live CEP turn.
//!
//! #28 B11 adds the Component leg: a Cell-admitted Tool call records its
//! admission evidence apart from its observed effects, both survive the replay
//! projection as separate PROV activity attributes, and the turn attests under
//! the v2 predicate.

use std::sync::Arc;

use lancet_api::audit_db_path_for_project_in;
use lancet_api::{AuditReplayRequest, replay_audit};
use lancet_native::{AuditQueryFilters, CepAuditStore};
use lancet_protocol::effect_types::ImpactKind;

use lancet_audit::ScopedXdgStateHome;
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, RunTurnStatus};
use lattice_llm::LlmEvent;
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
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
    /// Holds the shared env lock and points `XDG_STATE_HOME` at a temp state
    /// root for the whole fixture lifetime, restoring the previous value on drop.
    _state: ScopedXdgStateHome,
    root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
    target_branch: String,
}

fn setup_workspace() -> Workspace {
    let state = ScopedXdgStateHome::new();
    let ws = TempDir::new().unwrap();
    let root = ws.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn answer() -> i32 { 1 }").unwrap();
    init_git_repo(&root);
    let target_branch = create_unattached_target_branch(&root, "lattice-target");

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();
    let enabled = ["observe.workspace", "mutate.edit"];
    let toml_content = format!(
        r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "test"
[tools]
enabled = {enabled:?}
[lancet.workspace]
denied_write_patterns = [".git/**"]
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
    let repo = lattice_session::test_utils::init_empty_repo(root);
    lattice_session::test_utils::commit_all(&repo, root, "init");
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

#[tokio::test]
async fn full_turn_audit_rows_and_replay_fragment() {
    let ws = setup_workspace();
    // No `set_current_dir`: the request below pins `test_project_root`, and
    // mutating the process-global CWD from concurrent tests made sibling tests
    // fail once a `TempDir` dropped (see the note in `tests/consistency.rs`).

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
        tool_rows.len() >= 2,
        "expected read + mutation calls, got {}",
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

    let turn_id = tool_rows[0].turn_id.clone();
    let replay = replay_audit(AuditReplayRequest::new(
        &ws.root,
        result.session_id.clone(),
        turn_id,
    ))
    .await
    .expect("operator replay_audit");

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
}

/// #28 B11 Component leg: one admitted `noop.echo` call produces two effects
/// (the gated `emit-event` operation and the result), the audit row separates
/// "why authorized" from "what happened", the replay projection carries both
/// as distinct PROV activity attributes, and the turn attests as v2.
#[tokio::test]
async fn component_turn_projects_admission_and_effects_and_attests_v2() {
    use lancet_native::provenance::project_turn_trace_with_evidence;
    use lancet_native::{
        CepServerNativeExt, CepTransport, MethodRegistry, ServerRunConfig,
        register_production_method_registry,
    };
    use lancet_protocol::provenance::CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2;
    use lancet_runtime::ComponentRuntime;

    let components =
        Arc::new(ComponentRuntime::noop_fixture().expect("closed no-op test composition"));
    let mut config = ServerRunConfig::in_memory_for_tests().with_capability_admission(
        lancet_protocol::CapabilityAdmissionPolicy {
            enabled_tools: std::collections::BTreeSet::from(["noop.echo".to_string()]),
            ..Default::default()
        },
    );
    config.components = Some(components);
    let audit = Arc::clone(&config.audit);
    let ctx = config.audit_context.read().await.clone();

    let mut registry = MethodRegistry::new();
    register_production_method_registry(&mut registry).expect("production registry");
    let (client, server) = CepTransport::pair(64);
    let server_task = tokio::spawn(async move {
        let _ = server.run_native(registry, config).await;
    });

    let value: serde_json::Value = client
        .call_named("noop.echo", serde_json::json!({ "message": "provenance" }))
        .await
        .expect("component call");
    assert_eq!(value, serde_json::json!({ "echoed": "provenance" }));
    audit.await_pending().await;

    let rows = audit
        .query_rows(&AuditQueryFilters {
            session_id: Some(ctx.session_id.clone()),
            turn_id: Some(ctx.turn_id.clone()),
            method: Some("noop.echo".to_string()),
            ..Default::default()
        })
        .expect("query rows");
    assert_eq!(rows.len(), 1, "exactly one noop.echo audit row");

    // B6: the replay projection keeps the two evidence families apart. Since
    // #112 the records come from the evidence table, resolved through the
    // `evidence_id` each Component row points at.
    let resolve = |id: lancet_native::EvidenceId| audit.call_evidence(id).ok().flatten();
    let trace = project_turn_trace_with_evidence(
        &ctx.turn_id,
        &rows,
        &lancet_native::EvidenceLookup::new(&resolve),
    );
    let attributes = &trace.fragment.activities[0].attributes;
    assert_eq!(
        attributes["admission_evidence"]["approved_capabilities"],
        serde_json::json!(["session.events", "session.result"])
    );
    assert_eq!(
        attributes["observed_effects"]["effects"]
            .as_array()
            .expect("observed effects array")
            .len(),
        2
    );
    assert!(
        attributes.get("denied_operations").is_none(),
        "nothing was refused on the allow path"
    );

    lancet_api::store_turn_attestation(&audit, &ctx, &ctx.turn_id).expect("store attestation");
    let stored = audit
        .attestation_for_turn(&ctx.turn_id)
        .expect("attestation lookup")
        .expect("attestation stored");
    assert_eq!(
        stored.predicate_type, CEP_TURN_ATTESTATION_PREDICATE_TYPE_V2,
        "a turn carrying Cell evidence attests as v2"
    );

    server_task.abort();
}
