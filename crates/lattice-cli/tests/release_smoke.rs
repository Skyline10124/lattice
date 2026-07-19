// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Isolated CLI release smoke tests.
//!
//! Every smoke command sets temporary HOME, XDG_CONFIG_HOME, and project cwd
//! to prevent reading real user ~/.lattice state or leaking credentials.
//!
//! Coverage: session-first execution, models --auth, resolve sonnet, session status,
//! session log, and doctor.
//!
//! Threat model: (information disclosure), (tampering).

use lattice_session::SessionGraphStore;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Build a `Command` for the `lattice` binary with isolated environment.
fn lattice(temp_home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lattice"));
    cmd.env("HOME", temp_home)
        .env("XDG_CONFIG_HOME", temp_home.join(".config"))
        .env("LATTICE_AGENTS_DIR", "") // Prevent falling back to real ~/.lattice/agents
        .current_dir(temp_home);
    cmd
}

/// Create a temp directory for isolated HOME/XDG.
fn temp_home(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lattice-release-smoke-{}-{}",
        prefix,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp home");
    dir
}

/// Seed a session with one committed turn in the temp session store.
/// Returns the session ID used.
fn seed_session(temp_home: &Path, session_id: &str) {
    let session_dir = temp_home.join(".config").join("lattice").join("sessions");
    std::fs::create_dir_all(&session_dir).expect("create session dir");
    let git_repo_dir = session_dir.join("sessions.git");
    std::fs::create_dir_all(&git_repo_dir).expect("create git repo dir");

    let store = lattice_session::GitSessionGraphStore::new(&git_repo_dir);
    store
        .create_session(session_id, Some("Smoke test session"), "default", "main")
        .expect("create session");

    let messages = vec![
        lattice_session::GraphMessage {
            role: "user".to_string(),
            content: "hello from smoke test".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        lattice_session::GraphMessage {
            role: "assistant".to_string(),
            content: "smoke test response".to_string(),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    ];
    let input = lattice_session::TurnCommitInput {
        session_id: session_id.to_string(),
        parent_ids: vec![],
        messages,
        agent_name: "default".to_string(),
        profile_snapshot_hash: "test-profile".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "sonnet".to_string(),
        provider: "anthropic".to_string(),
        status: lattice_session::TurnCommitStatus::Completed,
        disposition: lattice_session::WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: "Smoke test turn".to_string(),
        usage: None,
        terminal_reason: None,
    };
    store.commit_turn(input).expect("commit turn");
}

/// Create a minimal .lattice/agents/default/agent.toml in the project dir.
// ─── models --auth ─────────────────────────────────────────────────────────
// ─── resolve sonnet ─────────────────────────────────────────────────────────
// ─── session status ─────────────────────────────────────────────────────────

// The two `smoke_session_status_*` tests that previously lived here were
// superseded by `fast_session_new_creates_session_and_appears_in_list` and
// the `fast_session_status_*` scenarios in
// `crates/lattice-system-harness/tests/fast_session_cli.rs` (issue #23
// slice 2). The seeded-session list/specific status surface is now asserted
// through the public `lattice` binary in the harness; the in-crate smoke
// version was redundant.

// ─── session log ─────────────────────────────────────────────────────────────

#[test]
fn smoke_session_log_shows_seeded_turn() {
    let home = temp_home("session-log");
    let session_id = "smoke-session-log-003";
    seed_session(&home, session_id);

    let output = lattice(&home)
        .args(["session", "log", session_id])
        .output()
        .expect("session log should run");

    assert!(
        output.status.success(),
        "session log should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should show the turn with the commit message
    assert!(
        stdout.contains("Smoke test turn") || stdout.contains("turn 0"),
        "session log should show the seeded turn, got:\n{stdout}"
    );
}

// ─── release gate wiring ────────────────────────────────────────────────────

#[test]
fn release_smoke_release_gate_sh_is_retired() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let gate_path = manifest_dir.join("../../scripts/release-gate.sh");
    let output = Command::new(&gate_path)
        .output()
        .unwrap_or_else(|err| panic!("run {}: {err}", gate_path.display()));

    assert!(
        !output.status.success(),
        "retired release gate should exit nonzero"
    );
    assert!(
        output.stdout.is_empty(),
        "retired release gate should not write stdout, got:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("release-gate.sh is retired. Run scripts/release-gate-v1.4.sh (current)"),
        "retired release gate should point to the current v1.4 gate, got:\n{stderr}"
    );
}

#[test]
fn release_smoke_release_gate_v1_4_includes_lancet_boundary_check() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let gate_path = manifest_dir.join("../../scripts/release-gate-v1.4.sh");
    let gate = std::fs::read_to_string(&gate_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", gate_path.display()));
    assert!(
        gate.contains("cargo xtask lancet-boundary"),
        "v1.4 release gate must invoke LANCET boundary check"
    );
    assert!(
        gate.contains("lancet-authority"),
        "v1.4 release gate must cover lancet-authority"
    );
    assert!(
        !gate.contains("v1_4_command_artifact_dogfood"),
        "v1.4 release gate must not reference the retired command artifact dogfood lane"
    );
    assert!(
        gate.contains("docs/v1.4-alpha-release-record.md")
            && gate.contains("does not write or enforce that record requirement"),
        "v1.4 release gate must name but neither write nor enforce the release record requirement"
    );
}

// #37: the `session resume` and `session prune-snapshots` subcommands were
// removed with the Upper/retained-snapshot lifecycle. The smoke test that
// asserted `prune-snapshots` rejected a missing session_id was deleted at the
// same time — the subcommand no longer exists, so the assertion no longer
// reflects an intentional contract.
