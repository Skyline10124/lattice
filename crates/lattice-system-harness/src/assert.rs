// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;
use std::process::Output;

use thiserror::Error;

use crate::artifacts::{
    capture_from_output, scan_artifacts_for_secrets, write_failure_artifacts, write_world_context,
};
use crate::world::ScenarioWorld;

/// Base directory for persisted failure artifacts.
///
/// Override with the `LATTICE_HARNESS_FAILURE_DIR` env var (absolute or
/// relative to the test process cwd). The default is anchored to the harness
/// crate's manifest dir so it is absolute and independent of whatever cwd the
/// test process happens to run with: `<harness crate>/target/harness-failures`.
/// `target/` is gitignored, so artifacts persist for post-mortem inspection
/// without polluting the tree. `cargo xtask ci` redirects this to the
/// workspace-level `$(pwd)/target/harness-failures` so a full local run
/// collects every scenario's bundle in one place.
const FAILURE_DIR_ENV: &str = "LATTICE_HARNESS_FAILURE_DIR";

fn failure_dir(scenario_name: &str) -> PathBuf {
    match std::env::var(FAILURE_DIR_ENV) {
        Ok(base) => PathBuf::from(base).join(scenario_name),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("harness-failures")
            .join(scenario_name),
    }
}

#[derive(Debug, Error)]
pub enum AssertError {
    #[error("expected exit success, got {0:?}")]
    ExitNotSuccess(i32),
    #[error("expected non-zero exit, but command exited successfully")]
    ExitNotFailure,
    #[error("expected exit code {expected}, got {actual}")]
    ExitCodeMismatch { expected: i32, actual: i32 },
    #[error("stdout missing expected substring(s): {expected}")]
    StdoutMissing { expected: String },
    #[error("stderr missing expected substring: {expected}")]
    StderrMissing { expected: String },
    #[error("output contained forbidden secret sentinel: {0}")]
    SecretLeak(String),
    #[error("failed to write failure artifacts: {0}")]
    ArtifactIo(#[from] std::io::Error),
}

pub fn assert_exit_success(output: &Output) -> Result<(), AssertError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(AssertError::ExitNotSuccess(
            output.status.code().unwrap_or(-1),
        ))
    }
}

pub fn assert_exit_failure(output: &Output) -> Result<(), AssertError> {
    if output.status.success() {
        Err(AssertError::ExitNotFailure)
    } else {
        Ok(())
    }
}

/// Assert the process exited with a specific non-zero code. Use this instead
/// of `assert_exit_failure` when the documented public contract pins a
/// particular exit code (e.g. `cmd_send_api` returns `ExitCode::from(2)` for
/// session/profile lookup errors); `assert_exit_failure` only asserts nonzero
/// and would silently pass a regression from 2 to 1.
pub fn assert_exit_code(output: &Output, expected: i32) -> Result<(), AssertError> {
    let got = output.status.code().unwrap_or(-1);
    if got == expected {
        Ok(())
    } else {
        Err(AssertError::ExitCodeMismatch {
            expected,
            actual: got,
        })
    }
}

pub fn assert_stdout_contains(output: &Output, needle: &str) -> Result<(), AssertError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains(needle) {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: needle.into(),
        })
    }
}

/// Assert stdout contains every needle, returning the comma-joined list of
/// missing needles on failure. This is the semantic-assertion helper meant to
/// be combined with `assert_exit_success` and fed to
/// `assert_with_failure_artifacts` so a semantic failure also captures
/// stdout/stderr for post-mortem.
pub fn assert_stdout_contains_all(output: &Output, needles: &[&str]) -> Result<(), AssertError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let missing: Vec<&str> = needles
        .iter()
        .copied()
        .filter(|n| !stdout.contains(n))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(AssertError::StdoutMissing {
            expected: missing.join(", "),
        })
    }
}

pub fn assert_stderr_contains(output: &Output, needle: &str) -> Result<(), AssertError> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains(needle) {
        Ok(())
    } else {
        Err(AssertError::StderrMissing {
            expected: needle.into(),
        })
    }
}

pub fn assert_no_secret(output: &Output, secret: &str) -> Result<(), AssertError> {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if combined.contains(secret) {
        Err(AssertError::SecretLeak(secret.into()))
    } else {
        Ok(())
    }
}

/// On assertion failure, write redacted stdout/stderr plus a scenario-world
/// context file to a **persistent** directory so the bundle survives test
/// teardown. The previous implementation wrote under `world.root()`, which is a
/// `tempfile::TempDir` deleted the instant the world drops — so a failed
/// scenario left nothing to inspect. The bundle now lands under
/// `$LATTICE_HARNESS_FAILURE_DIR/<scenario_name>/` (default
/// `target/harness-failures/<scenario_name>/`).
pub fn assert_with_failure_artifacts(
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    output: &Output,
    secrets: &[&str],
    result: Result<(), AssertError>,
) -> Result<(), AssertError> {
    if let Err(err) = result {
        let dir = failure_dir(scenario_name);
        let artifacts = capture_from_output(dir.clone(), output, command_summary, secrets);
        write_failure_artifacts(&artifacts)?;
        // Persist the scenario-world context so the post-mortem can reproduce
        // the isolated HOME/project/port/env the binary ran under. Env values
        // are redacted via `write_world_context` (secret-name vars masked
        // entirely; others substring-redacted against `secrets`) so a
        // secret-bearing env var set via `ScenarioWorld::set_env` cannot leak
        // into the artifact.
        write_world_context(
            &dir,
            world,
            scenario_name,
            &artifacts.command_summary,
            secrets,
        )?;
        // RFC #19 decision eight: redact before writing (done above), then
        // scan after writing as defense-in-depth. Any leaked file is
        // best-effort redacted in place by `scan_artifacts_for_secrets` and
        // recorded in `sentinel_scan.txt` so the operator can see the
        // pre-write redaction missed it. The scan never masks the original
        // assertion error: we still return `err` below.
        let leaked = scan_artifacts_for_secrets(&dir, secrets);
        if !leaked.is_empty() {
            let _ = std::fs::write(
                dir.join("sentinel_scan.txt"),
                format!(
                    "post-write sentinel scan redacted {} file(s): {leaked:?}\n",
                    leaked.len()
                ),
            );
        }
        return Err(err);
    }
    Ok(())
}
