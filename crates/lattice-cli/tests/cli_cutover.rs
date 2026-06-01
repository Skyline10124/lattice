//! CLI boundary smoke integration tests.
//!
//! These tests verify the completed CLI CLI surface cutover from the
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
    for name in &["models", "resolve", "session", "doctor", "audit", "cep"] {
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

    let mut command_names = Vec::new();
    for line in stdout.lines() {
        let Some(rest) = line.strip_prefix("  ") else {
            continue;
        };
        let Some((name, _)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        if name.chars().all(|ch| ch.is_ascii_lowercase()) {
            command_names.push(name.to_string());
        }
    }

    for name in &["models", "resolve", "session", "doctor", "audit", "cep"] {
        assert!(
            command_names.iter().any(|candidate| candidate == name),
            "help should contain supported command '{name}', got:\n{stdout}"
        );
    }

    assert_eq!(
        command_names.len(),
        7,
        "help should list only the final supported command surface (6 + clap 'help'), got:\n{stdout}"
    );
    assert!(
        command_names.contains(&"help".to_string()),
        "clap adds 'help' subcommand by default, got:\n{stdout}"
    );
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
    // Bare invocation should print help (which contains at least 'session').
    assert!(
        stdout.contains("session"),
        "bare output should include help with 'session', got:\n{stdout}"
    );
}

// ─── Unknown command rejection ────────────────────────────────────────────────

#[test]
fn cli_cutover_rejects_unknown_command_with_clap_error() {
    let output = lattice().arg("unsupported-command").output().unwrap();
    assert!(
        !output.status.success(),
        "unknown command should return non-zero exit"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unrecognized subcommand"),
        "unknown command should be rejected by clap, got stderr:\n{stderr}"
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

// ─── Workspace boundary: no removed crate imports in focused tests ──────

#[test]
fn focused_tests_import_no_removed_crate_names() {
    // Verify that no source or test file under the active workspace crates
    // imports a removed crate name. This proves focused tests do not
    // preserve old architecture fixtures.
    let crate_dirs = [
        "lattice-llm",
        "lattice-session",
        "lattice-profile",
        "lattice-agent",
        "lattice-tooling",
        "lattice-cep-protocol",
        "lattice-cep-catalog",
        "lattice-cep-dispatch",
        "lattice-cep-audit",
        "lattice-cep-native",
        "lattice-api",
        "lattice-cli",
    ];
    let forbidden = [
        "lattice_core",
        "lattice_runtime",
        "lattice_prompt",
        "lattice_sandbox",
        "lattice_policy",
        "cep_protocol",
        "cep_executor",
    ];

    let mut violations: Vec<String> = Vec::new();
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("cli crate should be under crates/");

    for crate_name in &crate_dirs {
        let crate_dir = workspace_root.join(crate_name);
        if !crate_dir.is_dir() {
            continue;
        }
        let mut stack = vec![crate_dir];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Skip target directories
                    if path.file_name().is_some_and(|n| n == "target") {
                        continue;
                    }
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                for (i, line) in content.lines().enumerate() {
                    let trimmed = line.trim();
                    // Skip comments
                    if trimmed.starts_with("//") {
                        continue;
                    }
                    // Only check use/import lines and string patterns that would
                    // actually create a dependency
                    if !trimmed.starts_with("use ") && !trimmed.contains("extern crate") {
                        continue;
                    }
                    for forbidden_name in &forbidden {
                        let bare_use = format!("use {forbidden_name};");
                        let qualified_use = format!("use {forbidden_name}::");
                        let extern_crate = format!("extern crate {forbidden_name}");
                        if trimmed == bare_use
                            || trimmed.starts_with(&qualified_use)
                            || trimmed.starts_with(&extern_crate)
                        {
                            violations.push(format!(
                                "{}:{}: {}",
                                path.strip_prefix(workspace_root).unwrap_or(&path).display(),
                                i + 1,
                                trimmed
                            ));
                        }
                    }
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "found removed crate import(s) in focused tests:\n{}",
        violations.join("\n")
    );
}

// ─── Removed run command boundary ────────────────────────────────────────────

#[test]
fn cli_cutover_run_command_is_removed() {
    let output = lattice().arg("run").output().unwrap();
    assert!(!output.status.success(), "run command should be removed");
}
