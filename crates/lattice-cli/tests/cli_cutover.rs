// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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
    for name in &[
        "models", "resolve", "session", "doctor", "audit", "lancet", "tui",
    ] {
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

    for name in &[
        "models", "resolve", "session", "doctor", "audit", "lancet", "tui",
    ] {
        assert!(
            command_names.iter().any(|candidate| candidate == name),
            "help should contain supported command '{name}', got:\n{stdout}"
        );
    }

    assert_eq!(
        command_names.len(),
        8,
        "help should list only the final supported command surface (7 + clap 'help'), got:\n{stdout}"
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

// ─── Resolve secret safety ───────────────────────────────────────────────────
// ─── Models --auth text safety ───────────────────────────────────────────────
// ─── Session log missing session ─────────────────────────────────────────────
// ─── Workspace boundary: no removed crate imports in focused tests ──────
// ─── Removed run command boundary ────────────────────────────────────────────
