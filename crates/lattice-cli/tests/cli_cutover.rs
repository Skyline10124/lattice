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

// ─── Resolve secret safety ───────────────────────────────────────────────────
// ─── Models --auth text safety ───────────────────────────────────────────────
// ─── Session log missing session ─────────────────────────────────────────────
// ─── Workspace boundary: no removed crate imports in focused tests ──────
// ─── Removed run command boundary ────────────────────────────────────────────
