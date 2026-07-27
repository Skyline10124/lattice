// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Fake-LLM end-to-end release scenario.
//!
//! Validates the full edit → edit cycle in a **single `run_turn`**
//! with a multi-round `FakeStreamingLlmClient` under the Direct-Local runtime.
//! Uses a persisted on-disk audit DB so the attestation can be inspected after
//! the turn completes.
//!
//! Filter: `release_e2e_criterion_8`

mod support;

use std::sync::Arc;

use lancet_api::audit_db_path_for_project_in;
use lancet_native::{AuditQueryFilters, CepAuditStore};
use lattice_agent::LlmClient;
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_api::{RunTurnRequest, RunTurnStatus};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{GitSessionGraphStore, SessionGraphStore};

// ── Streaming event helpers ──────────────────────────────────────────────────

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

// ── Workspace setup ──────────────────────────────────────────────────────────

struct ReleaseE2eWorkspace {
    _dir: tempfile::TempDir,
    _state: tempfile::TempDir,
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    canonical_root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

/// Build a temp workspace with a src/lib.rs seed and a custom profile.
///
/// The profile enables `mutate.edit` with an empty workspace deny list
/// (except `.git/**`) so edits apply cleanly.
fn setup_release_e2e_workspace(src_content: &str) -> ReleaseE2eWorkspace {
    let state = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("XDG_STATE_HOME", state.path());
    }
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), src_content).unwrap();
    support::init_git_repo(&root);

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();

    let toml_content = r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "You are a test agent."
[tools]
enabled = ["mutate.edit"]
[lancet.workspace]
denied_write_patterns = [".git/**"]
"#;
    std::fs::write(profile_dir.join("agent.toml"), toml_content).unwrap();

    let global_agents = tempfile::tempdir().unwrap();
    let paths = DiscoveryPaths {
        project_root: root.clone(),
        global_agents_dir: global_agents.path().to_path_buf(),
    };
    let loaded = lattice_profile::resolve_agent_profile(ProfileSource::Default, &paths)
        .expect("profile must load");

    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());

    ReleaseE2eWorkspace {
        _dir: ws,
        _state: state,
        _global_agents: global_agents,
        root,
        canonical_root,
        loaded,
    }
}

fn make_release_e2e_request(
    ws: &ReleaseE2eWorkspace,
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
        // Persist audit to disk so we can inspect attestation after run_turn.
        test_persist_audit: Some(false),
    }
}

// ── Release criterion-8 test ──────────────────────────────────────────────────

// Single `run_turn` driving mutate.edit → mutate.edit → Done(stop)
// without envelope declaration.
//
// Asserts:
// 1. Turn completes with status=Completed.
// 2. Direct-Local edits are verified via tool outcomes (section 6).
// 3. Session has exactly one commit in the graph.
// 4. On-disk attestation has correct in-toto subject / predicate / materials.
// 5. Audit rows are in edit → edit order.
// 6. Both mutate.edit tool results succeed without profile verify configured.

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
async fn release_e2e_criterion_8() {
    let initial_content = "pub fn answer() -> i32 { 1 }";
    let bad_content = "pub fn answer() -> i32 { 2 }"; // No profile verify configured, edit succeeds
    let good_content = "pub fn answer() -> i32 { 42 } // RELEASE_E2E_OK"; // verify passes

    let ws = setup_release_e2e_workspace(initial_content);

    // ── Compute blake3 hashes for the two edit steps ───────────────────
    let hash1 = format!(
        "blake3:{}",
        blake3::hash(initial_content.as_bytes()).to_hex()
    );

    // Edit 1: apply bad content (no profile verify configured).
    let bad_edit_args = serde_json::json!({
        "path": "src/lib.rs",
        "expected_hash": hash1,
        "edits": [{
            "op": "replace_text",
            "old": initial_content,
            "new": bad_content
        }]
    });

    // Edit 2: refresh expected_hash after edit 1 mutates Local content.
    let hash2 = format!("blake3:{}", blake3::hash(bad_content.as_bytes()).to_hex());
    let good_edit_args = serde_json::json!({
        "path": "src/lib.rs",
        "expected_hash": hash2,
        "edits": [{
            "op": "replace_text",
            "old": bad_content,
            "new": good_content
        }]
    });

    // ── Build FakeStreamingLlmClient with 3 rounds ─────
    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            // Round 1: mutate.edit (intermediate content)
            .push_events(tool_call_events(
                "call_1",
                "mutate.edit",
                &bad_edit_args.to_string(),
            ))
            // Round 2: mutate.edit (final content with sentinel)
            .push_events(tool_call_events(
                "call_2",
                "mutate.edit",
                &good_edit_args.to_string(),
            ))
            // Round 3: final assistant response after both edits
            .push_events(final_response_events(
                "Criterion-8 verified: edit(fail) → edit(ok).",
            ))
            .build(),
    );

    // ── Execute the single turn ──────────────────────────────────────────
    let target = create_unattached_target_branch(&ws.root, "lattice-target-release");
    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = lattice_api::SessionApi::git(&sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
            project_branch: target,
        })
        .expect("create session")
        .session_id;

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::git(&sessions_path),
        make_release_e2e_request(
            &ws,
            "Run release criterion-8 sequence",
            &session_id,
            fake_llm,
        ),
    )
    .await
    .expect("run_turn must succeed");

    // ── 1. Turn completed successfully ───────────────────────────────────
    assert_eq!(
        result.status,
        RunTurnStatus::Completed,
        "turn must complete; output: {}",
        result.assistant_output
    );
    assert!(
        result.assistant_output.contains("Criterion-8 verified"),
        "assistant output: {}",
        result.assistant_output
    );

    // ── 2. Direct-Local edits apply in the guarded Local Workspace ─
    // Verified via mutate.edit tool outcomes in section 6 below.

    // ── 3. Session has exactly one commit ────────────────────────────────
    let store = GitSessionGraphStore::new(&sessions_path);
    let log = store
        .log(&result.session_id)
        .expect("session log must be readable");
    assert_eq!(log.len(), 1, "session must have exactly one commit");

    // ── 4. Attestation: subject / predicate / materials ──────────────────
    //
    // The audit DB was persisted to disk (test_persist_audit: Some(false)).
    let audit_path =
        audit_db_path_for_project_in(ws._state.path(), &ws.canonical_root).expect("audit path");
    assert!(
        audit_path.exists(),
        "on-disk audit DB must exist at {audit_path:?}"
    );
    let audit = CepAuditStore::open(&audit_path).expect("open on-disk audit DB");

    let turn_id = format!("{}:root", result.session_id);
    let attestation = audit
        .attestation_for_turn(&turn_id)
        .expect("attestation lookup must not error")
        .expect("attestation must be stored after run_turn completes");

    // Parse the stored JSON statement.
    let stmt: serde_json::Value = serde_json::from_str(&attestation.statement_json)
        .expect("statement_json must be valid JSON");

    // subject: non-empty array; first entry name must start with "turn:"
    let subjects = stmt["subject"]
        .as_array()
        .expect("statement.subject must be a JSON array");
    assert!(!subjects.is_empty(), "statement.subject must be non-empty");
    assert!(
        subjects
            .iter()
            .any(|s| s["name"].as_str().unwrap_or("").starts_with("turn:")),
        "statement.subject must include an entry with name starting with 'turn:'; got: {subjects:?}"
    );

    // predicate_type: must match CEP_TURN_ATTESTATION_PREDICATE_TYPE
    assert_eq!(
        attestation.predicate_type, "https://lattice.dev/attestation/cep-turn/v1",
        "predicate_type must be CEP_TURN_ATTESTATION_PREDICATE_TYPE"
    );

    // predicate.materials: must include mutate.edit in the sequence
    let materials = stmt["predicate"]["materials"]
        .as_array()
        .expect("predicate.materials must be a JSON array");
    assert!(
        !materials.is_empty(),
        "predicate.materials must be non-empty"
    );
    let method_names: Vec<&str> = materials
        .iter()
        .filter_map(|m| m["method"].as_str())
        .collect();
    assert!(
        method_names.contains(&"mutate.edit"),
        "materials must include mutate.edit; got: {method_names:?}"
    );

    // ── 5. Audit PROV ordering: mutate.edit → mutate.edit ─
    let rows = audit
        .query_rows(&AuditQueryFilters {
            turn_id: Some(turn_id.clone()),
            ..Default::default()
        })
        .expect("query audit rows");

    assert!(
        rows.len() >= 2,
        "must have at least 2 audit rows (2x edit); got: {}",
        rows.len()
    );

    // Audit rows are returned in audit_id order (insertion order).
    let methods: Vec<&str> = rows.iter().map(|r| r.method.as_str()).collect();
    let edit_positions: Vec<usize> = methods
        .iter()
        .enumerate()
        .filter(|&(_, &m)| m == "mutate.edit")
        .map(|(i, _)| i)
        .collect();

    assert_eq!(
        edit_positions.len(),
        2,
        "must have exactly 2 mutate.edit audit rows; got {}: {methods:?}",
        edit_positions.len()
    );
    assert!(
        edit_positions[0] < edit_positions[1],
        "first edit must precede second edit in audit; order: {methods:?}"
    );

    // ── 6. Tool results: both edits succeed without profile verify ─
    let edit_results: Vec<_> = result
        .tool_results
        .iter()
        .filter(|t| t.name == "mutate.edit")
        .collect();
    assert_eq!(
        edit_results.len(),
        2,
        "must have 2 mutate.edit tool results; all results: {:?}",
        result
            .tool_results
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );
    assert!(
        edit_results[0].ok,
        "first mutate.edit must succeed without profile verify; error: {:?}",
        edit_results[0].error_summary
    );
    assert!(
        edit_results[1].ok,
        "second mutate.edit must succeed; error: {:?}",
        edit_results[1].error_summary
    );

    println!(
        "release criterion-8 PASSED: session={}, turn_id={}, attestation_predicate={}",
        result.session_id, turn_id, attestation.predicate_type
    );
}
