//! Fake-LLM end-to-end release scenario.
//!
//! Validates the full declare → exec.command_run → patch(fail-verify) → patch(ok)
//! cycle in a **single `run_turn`** with a multi-round `FakeStreamingLlmClient`.
//! Uses a persisted on-disk audit DB so the attestation can be inspected after
//! the turn completes.
//!
//! Filter: `release_e2e_criterion_8`

use std::collections::BTreeSet;
use std::sync::Arc;

use lancet_native::{AuditQueryFilters, CepAuditStore};
use lancet_protocol::envelope::{
    Capability, Constraints, ExecutionEnvelope, Impact, ImpactConfidence, ImpactKind, Intent,
    OnFailure, Scope, VerifyCommand,
};
use lattice_agent::testing::FakeStreamingLlmClient;
use lattice_agent::LlmClient;
use lattice_api::{RunTurnRequest, RunTurnStatus};
use lattice_llm::{LlmEvent, TokenUsage};
use lattice_profile::{DiscoveryPaths, LoadedAgentProfile, ProfileSource};
use lattice_session::{SessionGraphStore, SqliteSessionGraphStore};

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
    _global_agents: tempfile::TempDir,
    root: std::path::PathBuf,
    canonical_root: std::path::PathBuf,
    loaded: LoadedAgentProfile,
}

/// Build a temp workspace with a src/lib.rs seed and a custom profile.
///
/// The `[lancet.command]` section adds `grep` to `allowed_diagnostic_commands`
/// so the `grep -q RELEASE_E2E_OK src/lib.rs` verify command passes policy validation.
fn setup_release_e2e_workspace(src_content: &str) -> ReleaseE2eWorkspace {
    let ws = tempfile::tempdir().unwrap();
    let root = ws.path().to_path_buf();

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/lib.rs"), src_content).unwrap();

    let profile_dir = root.join(".lattice/agents/default");
    std::fs::create_dir_all(&profile_dir).unwrap();

    // The [lancet.command] section overrides allowed_diagnostic_commands so that
    // "grep" is classified as Diagnostic — required by validate_verify_command.
    let toml_content = r#"schema_version = 1
[agent]
name = "default"
model = "sonnet"
[prompt]
system = "You are a test agent."
[tools]
enabled = ["session.envelope", "exec.command_run", "mutate.patch"]
[lancet.command]
allowed_diagnostic_commands = [["echo"], ["grep"]]
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
    std::env::set_current_dir(&ws.root).expect("set workspace cwd");
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

/// Single `run_turn` driving declare → exec.command_run →
/// patch(verify-fail, RetainDiff) → patch(verify-pass) → Done(stop).
///
/// Asserts:
/// 1. Turn completes with status=Completed.
/// 2. Workspace file contains the RELEASE_E2E_OK sentinel after the second patch.
/// 3. Session has exactly one commit in the graph.
/// 4. On-disk attestation has correct in-toto subject / predicate / materials.
/// 5. Audit rows are in declare → exec.command_run → patch → patch order.
/// 6. First patch tool result is ok=false (VerifyFailed); second is ok=true.
#[tokio::test]
async fn release_e2e_criterion_8() {
    let initial_content = "pub fn answer() -> i32 { 1 }";
    let bad_content = "pub fn answer() -> i32 { 2 }"; // no RELEASE_E2E_OK — verify fails
    let good_content = "pub fn answer() -> i32 { 42 } // RELEASE_E2E_OK"; // verify passes

    let ws = setup_release_e2e_workspace(initial_content);

    // ── Compute blake3 hashes for the two patch steps ──────────────────
    let hash1 = format!(
        "blake3:{}",
        blake3::hash(initial_content.as_bytes()).to_hex()
    );
    let hash2 = format!("blake3:{}", blake3::hash(bad_content.as_bytes()).to_hex());

    // Patch 1 (bad): changes content but does NOT add RELEASE_E2E_OK sentinel.
    // verify.sh ("grep -q RELEASE_E2E_OK src/lib.rs") will exit 1 → VerifyFailed.
    // OnFailure::RetainDiff means the diff IS applied; file IS changed.
    let bad_patch = format!(
        "*** Begin Patch\n*** Update File: src/lib.rs\n*** Old Hash: {hash1}\n@@\n-{hash1}\t{initial_content}\n+{bad_content}\n*** End Patch\n"
    );

    // Patch 2 (good): changes content to include RELEASE_E2E_OK.
    // Hash uses the post-bad-patch content since RetainDiff left it changed.
    let good_patch = format!(
        "*** Begin Patch\n*** Update File: src/lib.rs\n*** Old Hash: {hash2}\n@@\n-{hash2}\t{bad_content}\n+{good_content}\n*** End Patch\n"
    );

    // ── Envelope for Round 1: RunCommand + WritePath + verify ──────────
    let envelope = ExecutionEnvelope {
        intent: Intent {
            description: "release criterion-8 declare".into(),
        },
        capabilities: BTreeSet::from([
            Capability::RunCommand {
                argv_allowlist: vec!["echo".into()],
            },
            Capability::WritePath {
                path_pattern: "src/**".into(),
            },
        ]),
        scope: Scope::default(),
        impact: vec![Impact {
            kind: ImpactKind::WorkspaceMutation,
            target: "src/lib.rs".into(),
            scope: None,
            confidence: ImpactConfidence::Conservative,
            unknowns: vec![],
        }],
        constraints: Constraints::default(),
        // verify: "grep -q RELEASE_E2E_OK src/lib.rs" — runs from project root.
        // Classified Diagnostic because ["grep"] is in cep.command.allowed_diagnostic_commands.
        verify: Some(VerifyCommand {
            command: "grep -q RELEASE_E2E_OK src/lib.rs".into(),
            expected_exit_code: None, // default expected exit code is 0
        }),
        on_failure: Some(OnFailure::RetainDiff),
        extensions: serde_json::Map::new(),
    };
    let declare_args = serde_json::json!({
        "mode": "declare",
        "envelope": envelope,
        "supersede": false,
        "reason": "release criterion-8 authority"
    });

    // Round 2: exec.command_run uses the RunCommand from the declared envelope.
    let run_args = serde_json::json!({
        "argv": ["echo", "release-command-run-ok"],
        "timeout_ms": 5000,
        "declared_impact": "command_execution"
    });

    // ── Build FakeStreamingLlmClient with 5 rounds ──────────────────────
    let fake_llm: Arc<dyn LlmClient> = Arc::new(
        FakeStreamingLlmClient::builder()
            // Round 1: session.envelope → sets up capabilities + verify command
            .push_events(tool_call_events(
                "call_0",
                "session.envelope",
                &declare_args.to_string(),
            ))
            // Round 2: exec.command_run — verifies RunCommand capability works
            .push_events(tool_call_events(
                "call_1",
                "exec.command_run",
                &run_args.to_string(),
            ))
            // Round 3: mutate.patch (bad) — no sentinel → verify fails, diff retained
            .push_events(tool_call_events(
                "call_2",
                "mutate.patch",
                &serde_json::json!({ "patch": bad_patch }).to_string(),
            ))
            // Round 4: mutate.patch (good) — adds sentinel → verify passes
            .push_events(tool_call_events(
                "call_3",
                "mutate.patch",
                &serde_json::json!({ "patch": good_patch }).to_string(),
            ))
            // Round 5: final assistant response after both patches
            .push_events(final_response_events(
                "Criterion-8 verified: declare → run → patch(fail) → patch(ok).",
            ))
            .build(),
    );

    // ── Execute the single turn ──────────────────────────────────────────
    let session_dir = tempfile::tempdir().unwrap();
    let sessions_path = session_dir.path().join("sessions");
    let session_id = lattice_api::SessionApi::sqlite(&sessions_path)
        .create_session(lattice_api::CreateSessionRequest {
            label: None,
            active_agent: "default".to_string(),
        })
        .expect("create session")
        .session_id;

    let result = lattice_api::run_turn(
        lattice_api::SessionApi::sqlite(&sessions_path),
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

    // ── 2. Workspace file contains the RELEASE_E2E_OK sentinel ─────────────────
    let final_content = std::fs::read_to_string(ws.root.join("src/lib.rs")).unwrap();
    assert!(
        final_content.contains("RELEASE_E2E_OK"),
        "src/lib.rs must contain RELEASE_E2E_OK after good patch, got: {final_content}"
    );

    // ── 3. Session has exactly one commit ────────────────────────────────
    let store = SqliteSessionGraphStore::new(&sessions_path);
    let log = store
        .log(&result.session_id)
        .expect("session log must be readable");
    assert_eq!(log.len(), 1, "session must have exactly one commit");

    // ── 4. Attestation: subject / predicate / materials ──────────────────
    //
    // The audit DB was persisted to disk (test_persist_audit: Some(false)).
    // build_production_turn_deps canonicalizes project_root before building the
    // audit path, so we open from canonical_root/.lattice/cep_audit.db.
    let audit_path = ws.canonical_root.join(".lattice").join("cep_audit.db");
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

    // predicate.materials: must include all three methods in the sequence
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
        method_names.contains(&"session.envelope"),
        "materials must include session.envelope; got: {method_names:?}"
    );
    assert!(
        method_names.contains(&"exec.command_run"),
        "materials must include exec.command_run; got: {method_names:?}"
    );
    assert!(
        method_names.contains(&"mutate.patch"),
        "materials must include mutate.patch; got: {method_names:?}"
    );

    // predicate.capabilities: must be non-empty (write_path, run_command at minimum)
    let capabilities = stmt["predicate"]["capabilities"]
        .as_array()
        .expect("predicate.capabilities must be a JSON array");
    assert!(
        !capabilities.is_empty(),
        "predicate.capabilities must be non-empty"
    );

    // ── 5. Audit PROV ordering: declare → exec.command_run → patch → patch ────
    let rows = audit
        .query_rows(&AuditQueryFilters {
            turn_id: Some(turn_id.clone()),
            ..Default::default()
        })
        .expect("query audit rows");

    assert!(
        rows.len() >= 4,
        "must have at least 4 audit rows (declare, exec.command_run, 2x patch); got: {}",
        rows.len()
    );

    // Audit rows are returned in audit_id order (insertion order).
    let methods: Vec<&str> = rows.iter().map(|r| r.method.as_str()).collect();
    let declare_pos = methods
        .iter()
        .position(|&m| m == "session.envelope")
        .expect("session.envelope must appear in audit rows");
    let run_pos = methods
        .iter()
        .position(|&m| m == "exec.command_run")
        .expect("exec.command_run must appear in audit rows");
    let patch_positions: Vec<usize> = methods
        .iter()
        .enumerate()
        .filter(|(_, &m)| m == "mutate.patch")
        .map(|(i, _)| i)
        .collect();

    assert_eq!(
        patch_positions.len(),
        2,
        "must have exactly 2 mutate.patch audit rows; got {}: {methods:?}",
        patch_positions.len()
    );
    assert!(
        declare_pos < run_pos,
        "declare must precede exec.command_run in audit; order: {methods:?}"
    );
    assert!(
        run_pos < patch_positions[0],
        "exec.command_run must precede first patch in audit; order: {methods:?}"
    );
    assert!(
        patch_positions[0] < patch_positions[1],
        "first patch must precede second patch in audit; order: {methods:?}"
    );

    // ── 6. Tool results: declare ok=true, first patch ok=false, second ok=true ─────────
    let declare_result = result
        .tool_results
        .iter()
        .find(|t| t.name == "session.envelope")
        .expect("must have session.envelope tool result");
    assert!(
        declare_result.ok,
        "session.envelope must succeed before mutation; error: {:?}",
        declare_result.error_summary
    );

    let patch_results: Vec<_> = result
        .tool_results
        .iter()
        .filter(|t| t.name == "mutate.patch")
        .collect();
    assert_eq!(
        patch_results.len(),
        2,
        "must have 2 mutate.patch tool results; all results: {:?}",
        result
            .tool_results
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );
    assert!(
        !patch_results[0].ok,
        "first mutate.patch must fail (verify returned non-zero exit code)"
    );
    assert!(
        patch_results[1].ok,
        "second mutate.patch must succeed (verify found RELEASE_E2E_OK sentinel)"
    );

    println!(
        "release criterion-8 PASSED: session={}, turn_id={}, attestation_predicate={}",
        result.session_id, turn_id, attestation.predicate_type
    );
}
