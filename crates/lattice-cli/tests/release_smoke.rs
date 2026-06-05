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
//! Threat model: T-11-20 (information disclosure), T-11-21 (tampering).

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

    let store = lattice_session::SqliteSessionGraphStore::new(&session_dir);
    store
        .create_session(session_id, Some("Smoke test session"), "default")
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
        commit_message: "Smoke test turn".to_string(),
        usage: None,
        terminal_reason: None,
    };
    store.commit_turn(input).expect("commit turn");
}

/// Create a minimal .lattice/agents/default/agent.toml in the project dir.
fn seed_default_profile(project_dir: &Path) {
    let agents_dir = project_dir.join(".lattice").join("agents").join("default");
    std::fs::create_dir_all(&agents_dir).expect("create agents dir");
    let profile = lattice_profile::default_agent_template("default", "sonnet");
    std::fs::write(agents_dir.join("agent.toml"), profile).expect("write agent.toml");
}

// ─── models --auth ─────────────────────────────────────────────────────────

#[test]
fn smoke_models_auth_lists_aliases_without_secrets() {
    let home = temp_home("models-auth");
    // Set a fake credential to ensure at least one alias appears
    let output = lattice(&home)
        .args(["models", "--auth"])
        .env("ANTHROPIC_API_KEY", "sk-smoke-test-secret")
        .output()
        .expect("models --auth should run");

    assert!(
        output.status.success(),
        "models --auth should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should contain at least one alias (sonnet or haiku with Anthropic key set)
    assert!(
        !stdout.trim().is_empty(),
        "models --auth should produce output"
    );
    // Must NOT contain the fake secret value (T-11-20)
    assert!(
        !stdout.contains("sk-smoke-test-secret"),
        "models --auth must not leak secret value"
    );
}

// ─── resolve sonnet ─────────────────────────────────────────────────────────

#[test]
fn smoke_resolve_sonnet_json_shows_env_var_not_secret() {
    let home = temp_home("resolve");
    let output = lattice(&home)
        .args(["resolve", "sonnet", "--json"])
        .env("ANTHROPIC_API_KEY", "sk-smoke-resolve-secret")
        .output()
        .expect("resolve sonnet should run");

    assert!(
        output.status.success(),
        "resolve sonnet should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should contain the env var name
    assert!(
        stdout.contains("ANTHROPIC_API_KEY"),
        "resolve --json should contain env var name, got:\n{stdout}"
    );
    // Must NOT contain the secret value (T-11-20)
    assert!(
        !stdout.contains("sk-smoke-resolve-secret"),
        "resolve --json must not leak secret value"
    );
}

#[test]
fn smoke_resolve_missing_credential_names_env_var() {
    let home = temp_home("resolve-missing");
    // No ANTHROPIC_API_KEY set -- resolve should still work but show Missing status
    let output = lattice(&home)
        .args(["resolve", "sonnet", "--json"])
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("resolve sonnet should run");

    assert!(
        output.status.success(),
        "resolve sonnet without credential should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should name the env var so missing-credential failures are actionable.
    assert!(
        stdout.contains("ANTHROPIC_API_KEY"),
        "resolve should mention the env var name, got:\n{stdout}"
    );
}

// ─── session status ─────────────────────────────────────────────────────────

#[test]
fn smoke_session_status_list_shows_seeded_session() {
    let home = temp_home("session-status");
    let session_id = "smoke-session-status-001";
    seed_session(&home, session_id);

    let output = lattice(&home)
        .args(["session", "status"])
        .output()
        .expect("session status should run");

    assert!(
        output.status.success(),
        "session status should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should show the seeded session
    assert!(
        stdout.contains(session_id) || stdout.contains("1 session"),
        "session status should show seeded session, got:\n{stdout}"
    );
}

#[test]
fn smoke_session_status_specific_session() {
    let home = temp_home("session-status-specific");
    let session_id = "smoke-session-specific-002";
    seed_session(&home, session_id);

    let output = lattice(&home)
        .args(["session", "status", session_id])
        .output()
        .expect("session status <id> should run");

    assert!(
        output.status.success(),
        "session status <id> should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The CLI truncates long IDs to 12 chars + ".."
    let truncated = if session_id.len() > 12 {
        &session_id[..12]
    } else {
        session_id
    };
    assert!(
        stdout.contains(truncated),
        "session status <id> should contain the session id (or prefix), got:\n{stdout}"
    );
}

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

// ─── doctor ──────────────────────────────────────────────────────────────────

#[test]
fn smoke_doctor_runs_without_leaking_secrets() {
    let home = temp_home("doctor");
    let output = lattice(&home)
        .args(["doctor"])
        .env("ANTHROPIC_API_KEY", "sk-smoke-doctor-secret")
        .output()
        .expect("doctor should run");

    assert!(
        output.status.success(),
        "doctor should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    // Must NOT contain the fake secret value (T-11-20)
    assert!(
        !combined.contains("sk-smoke-doctor-secret"),
        "doctor must not leak secret value"
    );
    // Should mention the env var name
    assert!(
        combined.contains("ANTHROPIC_API_KEY"),
        "doctor should name the env var, got:\n{combined}"
    );
}

#[test]
fn smoke_doctor_json_does_not_leak_secrets() {
    let home = temp_home("doctor-json");
    let output = lattice(&home)
        .args(["doctor", "--json"])
        .env("OPENAI_API_KEY", "sk-smoke-doctor-json-secret")
        .output()
        .expect("doctor --json should run");

    assert!(
        output.status.success(),
        "doctor --json should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Must NOT contain the fake secret value
    assert!(
        !stdout.contains("sk-smoke-doctor-json-secret"),
        "doctor --json must not leak secret value"
    );
    // Should contain the env var name
    assert!(
        stdout.contains("OPENAI_API_KEY"),
        "doctor --json should name the env var"
    );
}

// ─── session send ────────────────────────────────────────────────────────────

#[test]
fn smoke_session_send_missing_credentials_fails_without_secrets() {
    let home = temp_home("send-missing-creds");
    let session_id = "smoke-send-missing-creds";
    seed_default_profile(&home);
    seed_session(&home, session_id);

    let output = lattice(&home)
        .args(["session", "send", session_id, "hello world"])
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .expect("lattice session send should execute");

    assert!(
        !output.status.success(),
        "session send without credentials should fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("sk-"),
        "session send error must not contain secret values, got:\n{stderr}"
    );
}

#[test]
fn smoke_session_send_no_profile_fails_with_clear_message() {
    let home = temp_home("send-no-profile");
    let session_id = "smoke-send-no-profile";
    seed_session(&home, session_id);

    let output = lattice(&home)
        .args(["session", "send", session_id, "hello"])
        .env("ANTHROPIC_API_KEY", "sk-smoke-send-noprofile")
        .output()
        .expect("lattice session send should execute");

    assert!(
        !output.status.success(),
        "session send without profile should fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("agent.toml") || stderr.contains("profile") || stderr.contains("not found"),
        "session send error should mention missing profile, got:\n{stderr}"
    );
    assert!(
        !stderr.contains("sk-smoke-send-noprofile"),
        "session send error must not leak secret value"
    );
}

// ─── session nonexistent ─────────────────────────────────────────────────────

#[test]
fn smoke_session_status_nonexistent_returns_error() {
    let home = temp_home("session-nonexistent");

    let output = lattice(&home)
        .args(["session", "status", "nonexistent-smoke-id"])
        .output()
        .expect("session status should run");

    assert!(
        !output.status.success(),
        "session status with nonexistent id should fail"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent-smoke-id"),
        "error should mention the missing session id, got:\n{stderr}"
    );
}

// ─── release gate wiring ────────────────────────────────────────────────────

#[test]
fn release_smoke_release_gate_includes_lancet_boundary_check() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let gate_path = manifest_dir.join("../../scripts/release-gate.sh");
    let gate = std::fs::read_to_string(&gate_path)
        .unwrap_or_else(|err| panic!("read {}: {err}", gate_path.display()));
    assert!(
        gate.contains("scripts/lancet-boundary-check.sh"),
        "release gate must invoke LANCET boundary check"
    );
}
