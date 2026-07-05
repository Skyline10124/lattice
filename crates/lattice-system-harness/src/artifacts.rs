// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::process::Output;

/// Redacted failure bundle written when a scenario fails (minimal tracer bullet).
#[derive(Debug, Clone)]
pub struct FailureArtifacts {
    pub dir: PathBuf,
    pub stdout: String,
    pub stderr: String,
    pub command_summary: String,
}

const REDACTED: &str = "[REDACTED]";

pub fn redact(text: &str, secrets: &[&str]) -> String {
    let mut out = text.to_string();
    for secret in secrets {
        if !secret.is_empty() {
            out = out.replace(secret, REDACTED);
        }
    }
    out
}

pub fn capture_from_output(
    dir: PathBuf,
    output: &Output,
    command_summary: impl Into<String>,
    secrets: &[&str],
) -> FailureArtifacts {
    let stdout = redact(&String::from_utf8_lossy(&output.stdout), secrets);
    let stderr = redact(&String::from_utf8_lossy(&output.stderr), secrets);
    // Redact the command summary too: a caller may interpolate a secret-bearing
    // argument (e.g. an API key passed on the command line) into the summary.
    let command_summary = redact(&command_summary.into(), secrets);
    FailureArtifacts {
        dir,
        stdout,
        stderr,
        command_summary,
    }
}

pub fn write_failure_artifacts(artifacts: &FailureArtifacts) -> std::io::Result<()> {
    std::fs::create_dir_all(&artifacts.dir)?;
    std::fs::write(artifacts.dir.join("stdout.txt"), &artifacts.stdout)?;
    std::fs::write(artifacts.dir.join("stderr.txt"), &artifacts.stderr)?;
    std::fs::write(
        artifacts.dir.join("command.txt"),
        &artifacts.command_summary,
    )?;
    Ok(())
}

/// Render an `(env name, env value)` pair for the `world.txt` context file,
/// with secret leakage defenses:
///
/// - If the variable's *name* matches a secret-name pattern (contains `KEY`,
///   `SECRET`, `TOKEN`, `PASSWORD`, `CREDENTIAL`, `AUTH`, `PRIVATE`), the
///   value is replaced with `[REDACTED-ENV]` regardless of the `secrets`
///   list — a test that `set_env("ANTHROPIC_API_KEY", "sk-...")` must not
///   leak the key into a persisted artifact even if the caller forgot to add
///   it to `secrets`.
/// - Otherwise the value is substring-redacted against the caller-supplied
///   `secrets` list (e.g. a secret embedded in a `PATH` or custom var).
///
/// The name itself is never redacted (it carries no secret and is needed for
/// post-mortem reproduction).
pub fn redact_env_pair(name: &str, value: &str, secrets: &[&str]) -> String {
    const SECRET_NAME_MARKERS: &[&str] = &[
        "KEY",
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "CREDENTIAL",
        "AUTH",
        "PRIVATE",
    ];
    let upper = name.to_ascii_uppercase();
    let looks_secret = SECRET_NAME_MARKERS.iter().any(|m| upper.contains(m));
    if looks_secret {
        return format!("  {name}=[REDACTED-ENV]");
    }
    format!("  {name}={}", redact(value, secrets))
}
