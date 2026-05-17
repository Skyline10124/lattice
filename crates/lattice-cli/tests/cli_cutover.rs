//! Phase 10 Plan 05: CLI boundary smoke integration tests.
//!
//! These tests verify the completed Phase 10 CLI surface cutover from the
//! binary/user perspective. They exercise help output, command parsing,
//! secret-safe diagnostics, and failure behavior without requiring live
//! provider credentials or adding new dev dependencies.

use std::process::Command;

/// Build a `Command` for the `lattice` binary.
fn lattice() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lattice"))
}

// ─── Help text boundary ───────────────────────────────────────────────────────

#[test]
fn cli_cutover_help_contains_supported_commands() {
    let output = lattice().arg("--help").output().unwrap();
    assert!(output.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in &["run", "models", "resolve", "session", "doctor"] {
        assert!(
            stdout.contains(name),
            "help should contain supported command '{name}', got:\n{stdout}"
        );
    }
}

#[test]
fn cli_cutover_help_omits_removed_commands() {
    let output = lattice().arg("--help").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in &[
        "tui",
        "debug",
        "config",
        "stats",
        "validate",
        "new",
        "list",
        "run-v11",
        "quick",
        "sessions",
    ] {
        // Match the command name as a subcommand table entry (name followed by
        // whitespace). This avoids false positives from words appearing in
        // descriptions or help text.
        let pattern = format!("\n  {name} ");
        assert!(
            !stdout.contains(&pattern),
            "help should not contain removed command '{name}' as subcommand, got:\n{stdout}"
        );
    }
}

// ─── Bare invocation ──────────────────────────────────────────────────────────

#[test]
fn cli_cutover_bare_lattice_prints_help_and_exits_zero() {
    let output = lattice().output().unwrap();
    assert!(
        output.status.success(),
        "bare 'lattice' should exit 0, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Bare invocation should print help (which contains at least 'run').
    assert!(
        stdout.contains("run"),
        "bare output should include help with 'run', got:\n{stdout}"
    );
}

// ─── Old command rejection ────────────────────────────────────────────────────

#[test]
fn cli_cutover_rejects_tui() {
    let output = lattice().arg("tui").output().unwrap();
    assert!(!output.status.success(), "tui should return non-zero exit");
}

#[test]
fn cli_cutover_rejects_debug() {
    let output = lattice().arg("debug").output().unwrap();
    assert!(!output.status.success(), "debug should return non-zero exit");
}

#[test]
fn cli_cutover_rejects_config() {
    let output = lattice().arg("config").output().unwrap();
    assert!(!output.status.success(), "config should return non-zero exit");
}

#[test]
fn cli_cutover_rejects_stats() {
    let output = lattice().arg("stats").output().unwrap();
    assert!(!output.status.success(), "stats should return non-zero exit");
}

#[test]
fn cli_cutover_rejects_validate() {
    let output = lattice().arg("validate").output().unwrap();
    assert!(!output.status.success(), "validate should return non-zero exit");
}

#[test]
fn cli_cutover_rejects_new() {
    let output = lattice().arg("new").output().unwrap();
    assert!(!output.status.success(), "new should return non-zero exit");
}

#[test]
fn cli_cutover_rejects_list() {
    let output = lattice().arg("list").output().unwrap();
    assert!(!output.status.success(), "list should return non-zero exit");
}

#[test]
fn cli_cutover_rejects_run_v11() {
    let output = lattice().arg("run-v11").arg("hello").output().unwrap();
    assert!(
        !output.status.success(),
        "run-v11 should return non-zero exit"
    );
}

#[test]
fn cli_cutover_rejects_sessions_plural() {
    let output = lattice().arg("sessions").output().unwrap();
    assert!(
        !output.status.success(),
        "sessions (plural) should return non-zero exit"
    );
}

// Old commands produce clap error (exit 2), not a compatibility message.
// Clap returns exit code 2 for usage errors. We verify stderr does NOT
// contain compatibility shim text like "deprecated" or "use instead".
#[test]
fn cli_cutover_old_commands_reject_with_clap_error_not_compatibility_message() {
    for name in &["tui", "debug", "config", "stats", "validate", "new", "list"] {
        let output = lattice().arg(name).output().unwrap();
        assert!(!output.status.success(), "{name} should fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.to_lowercase().contains("deprecated"),
            "{name} should not emit deprecation message, got: {stderr}"
        );
        assert!(
            !stderr.to_lowercase().contains("use instead"),
            "{name} should not emit 'use instead' message, got: {stderr}"
        );
    }
}

// ─── Old global flag rejection ────────────────────────────────────────────────

#[test]
fn cli_cutover_rejects_global_config_flag() {
    let output = lattice()
        .args(["--config", "foo.toml", "doctor"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--config should return non-zero exit"
    );
}

#[test]
fn cli_cutover_rejects_global_model_flag() {
    let output = lattice()
        .args(["--model", "sonnet", "doctor"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--model should return non-zero exit"
    );
}

#[test]
fn cli_cutover_rejects_global_provider_flag() {
    let output = lattice()
        .args(["--provider", "openai", "doctor"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--provider should return non-zero exit"
    );
}

#[test]
fn cli_cutover_rejects_global_session_flag() {
    let output = lattice()
        .args(["--session", "sess1", "doctor"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--session should return non-zero exit"
    );
}

#[test]
fn cli_cutover_rejects_global_no_save_flag() {
    let output = lattice()
        .args(["--no-save", "doctor"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--no-save should return non-zero exit"
    );
}

#[test]
fn cli_cutover_rejects_global_continue_flag() {
    let output = lattice()
        .args(["--continue", "doctor"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--continue should return non-zero exit"
    );
}

// ─── Resolve secret safety (T-10-01) ─────────────────────────────────────────

#[test]
fn cli_cutover_resolve_json_excludes_secret_value() {
    let output = lattice()
        .args(["resolve", "sonnet", "--json"])
        .env("ANTHROPIC_API_KEY", "sk-secret-cli-cutover")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "resolve sonnet --json should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    // JSON should contain the env var name (not the value).
    assert!(
        stdout.contains("ANTHROPIC_API_KEY"),
        "JSON should contain env var name 'ANTHROPIC_API_KEY', got:\n{stdout}"
    );
    // JSON should contain provider and model fields.
    assert!(
        stdout.contains("provider"),
        "JSON should contain 'provider' field, got:\n{stdout}"
    );
    assert!(
        stdout.contains("provider_model"),
        "JSON should contain 'provider_model' field, got:\n{stdout}"
    );
    // JSON must NOT contain the actual secret value.
    assert!(
        !stdout.contains("sk-secret-cli-cutover"),
        "JSON must not contain secret value 'sk-secret-cli-cutover', got:\n{stdout}"
    );
}

// ─── Models --auth text safety (T-10-01) ─────────────────────────────────────

#[test]
fn cli_cutover_models_auth_one_alias_per_line_without_icons_or_secrets() {
    let output = lattice()
        .args(["models", "--auth"])
        .env("ANTHROPIC_API_KEY", "sk-secret-cli-cutover")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "models --auth should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Output should have at least one alias line.
    assert!(
        !stdout.trim().is_empty(),
        "models --auth should produce output"
    );

    // No secret value should appear.
    assert!(
        !stdout.contains("sk-secret-cli-cutover"),
        "models --auth must not contain secret value, got:\n{stdout}"
    );

    // No status icons (checkmark/cross used by doctor).
    assert!(
        !stdout.contains('\u{2713}'),
        "models --auth must not contain checkmark icon, got:\n{stdout}"
    );
    assert!(
        !stdout.contains('\u{2717}'),
        "models --auth must not contain cross icon, got:\n{stdout}"
    );

    // No provider URLs.
    assert!(
        !stdout.contains("https://"),
        "models --auth must not contain provider URLs, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("http://"),
        "models --auth must not contain provider URLs, got:\n{stdout}"
    );
}

// ─── Session log missing session (T-10-03, T-10-05) ──────────────────────────

#[test]
fn cli_cutover_session_log_missing_session_fails_with_id_in_message() {
    let output = lattice()
        .args(["session", "log", "nonexistent-cli-cutover-id"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "session log with missing id should fail"
    );
    // Error message should include the missing session id.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent-cli-cutover-id"),
        "error should mention the missing session id, got stderr:\n{stderr}"
    );
}

// ─── Run missing prompt failure (T-10-05) ────────────────────────────────────

#[test]
fn cli_cutover_run_without_prompt_or_file_fails() {
    // 'lattice run' with no prompt argument should fail.
    // The Run command requires a prompt positional arg.
    let output = lattice().arg("run").output().unwrap();
    assert!(
        !output.status.success(),
        "run without prompt should fail"
    );
}
