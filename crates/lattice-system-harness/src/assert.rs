// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use thiserror::Error;

use crate::artifacts::{
    FailureReport, capture_from_output, render_command_text, scan_artifacts_for_secrets,
    write_failure_artifacts, write_world_context,
};
use crate::driver::CommandOutcome;
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

pub fn failure_dir(scenario_name: &str) -> PathBuf {
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
    #[error("expected clean exit success, got {actual}")]
    ExitNotSuccess { actual: String },
    #[error("expected a clean non-zero exit, got {actual}")]
    ExitNotFailure { actual: String },
    #[error("expected clean exit code {expected}, got {actual}")]
    ExitCodeMismatch { expected: i32, actual: String },
    #[error("stdout missing expected substring(s): {expected}")]
    StdoutMissing { expected: String },
    #[error("stderr missing expected substring: {expected}")]
    StderrMissing { expected: String },
    #[error("output contained forbidden secret sentinel at {location}")]
    SecretLeak { location: String },
    #[error("failed to write failure artifacts: {0}")]
    ArtifactIo(#[from] std::io::Error),
    #[error(transparent)]
    FailureReport(FailureReport),
}

pub fn assert_exit_success(output: &CommandOutcome) -> Result<(), AssertError> {
    if output.is_clean_exit(0) {
        Ok(())
    } else {
        Err(AssertError::ExitNotSuccess {
            actual: output.summary().to_string(),
        })
    }
}

pub fn assert_exit_failure(output: &CommandOutcome) -> Result<(), AssertError> {
    if output.clean_exit_code().is_some_and(|code| code != 0) {
        Ok(())
    } else {
        Err(AssertError::ExitNotFailure {
            actual: output.summary().to_string(),
        })
    }
}

/// Assert the process exited with a specific non-zero code. Use this instead
/// of `assert_exit_failure` when the documented public contract pins a
/// particular exit code (e.g. `cmd_send_api` returns `ExitCode::from(2)` for
/// session/profile lookup errors); `assert_exit_failure` only asserts nonzero
/// and would silently pass a regression from 2 to 1.
pub fn assert_exit_code(output: &CommandOutcome, expected: i32) -> Result<(), AssertError> {
    if output.is_clean_exit(expected) {
        Ok(())
    } else {
        Err(AssertError::ExitCodeMismatch {
            expected,
            actual: output.summary().to_string(),
        })
    }
}

pub fn assert_stdout_contains(output: &CommandOutcome, needle: &str) -> Result<(), AssertError> {
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
pub fn assert_stdout_contains_all(
    output: &CommandOutcome,
    needles: &[&str],
) -> Result<(), AssertError> {
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

pub fn assert_stderr_contains(output: &CommandOutcome, needle: &str) -> Result<(), AssertError> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains(needle) {
        Ok(())
    } else {
        Err(AssertError::StderrMissing {
            expected: needle.into(),
        })
    }
}

pub fn assert_no_secret(output: &CommandOutcome, secret: &str) -> Result<(), AssertError> {
    if !secret.is_empty()
        && (String::from_utf8_lossy(&output.stdout).contains(secret)
            || String::from_utf8_lossy(&output.stderr).contains(secret))
    {
        Err(AssertError::SecretLeak {
            location: "command stdout/stderr".into(),
        })
    } else {
        Ok(())
    }
}

/// Finalize one fast-path assertion without letting artifact failures replace it.
pub fn finalize_with_failure_artifacts(
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    output: &CommandOutcome,
    secrets: &[&str],
    result: Result<(), AssertError>,
) -> Result<(), FailureReport> {
    let primary_assertion = result.err().map(|error| error.to_string());
    let failed = primary_assertion.is_some();
    let mut report = FailureReport::new(primary_assertion, secrets);
    report.add_command_diagnostic(
        render_command_text(command_summary, output, secrets),
        secrets,
    );

    let dir = failure_dir(scenario_name);
    if dir.exists()
        && let Err(error) = std::fs::remove_dir_all(&dir)
    {
        report.add_evidence_error("clean stale artifact bundle", error, secrets);
        return report.finish();
    }
    if !failed {
        return report.finish();
    }
    if let Err(error) = std::fs::create_dir_all(&dir) {
        report.add_evidence_error("create artifact root", error, secrets);
        return report.finish();
    }

    let artifacts = capture_from_output(dir, output, command_summary, secrets);
    persist_fast_failure_bundle(
        world,
        scenario_name,
        command_summary,
        &artifacts,
        secrets,
        &mut report,
    );
    report.finish()
}

fn persist_fast_failure_bundle(
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    artifacts: &crate::artifacts::FailureArtifacts,
    secrets: &[&str],
    report: &mut FailureReport,
) {
    if let Err(error) = write_failure_artifacts(artifacts) {
        report.add_evidence_error("write failure artifacts", error, secrets);
        return;
    }
    if let Err(error) = write_world_context(
        &artifacts.dir,
        world,
        scenario_name,
        command_summary,
        secrets,
    ) {
        report.add_evidence_error("write world.txt", error, secrets);
        return;
    }
    match scan_artifacts_for_secrets(&artifacts.dir, secrets) {
        Err(error) => report.add_evidence_error("secret scan", error, secrets),
        Ok(leaked) => {
            let locations: Vec<String> = leaked
                .iter()
                .map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "<artifact>".into())
                })
                .collect();
            for location in &locations {
                report.add_secret_location(location, secrets);
            }
            if !locations.is_empty()
                && let Err(error) = std::fs::write(
                    artifacts.dir.join("sentinel_scan.txt"),
                    format!("post-write sentinel scan redacted: {locations:?}\n"),
                )
            {
                report.add_evidence_error("write sentinel_scan.txt", error, secrets);
            }
        }
    }
}

pub fn assert_with_failure_artifacts(
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    output: &CommandOutcome,
    secrets: &[&str],
    result: Result<(), AssertError>,
) -> Result<(), AssertError> {
    finalize_with_failure_artifacts(
        world,
        scenario_name,
        command_summary,
        output,
        secrets,
        result,
    )
    .map_err(AssertError::FailureReport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver::{
        CleanupError, CleanupOperation, HarnessIntervention, ProcessGroupCleanup,
        ProcessInspectionOperation, ProcessTermination,
    };
    use std::time::Duration;

    fn outcome(termination: ProcessTermination) -> CommandOutcome {
        CommandOutcome {
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_complete: true,
            stderr_complete: true,
            termination: Some(termination),
            interventions: Vec::new(),
            cleanup: ProcessGroupCleanup::default(),
        }
    }

    fn cleanup_error(operation: CleanupOperation) -> CleanupError {
        CleanupError {
            operation,
            kind: std::io::ErrorKind::Other,
            raw_os_error: None,
            message: "synthetic cleanup failure".into(),
        }
    }

    fn assert_secret_forms_absent(text: &str) {
        for form in [
            "sk-issue185-secret",
            "[115, 107, 45, 105, 115, 115, 117, 101, 49, 56, 53, 45, 115, 101, 99, 114, 101, 116]",
            "736b2d69737375653138352d736563726574",
            "c2staXNzdWUxODUtc2VjcmV0",
        ] {
            assert!(!text.contains(form), "diagnostic leaked {form:?}: {text}");
        }
    }

    #[test]
    fn exit_assertion_errors_exclude_output_payload_encodings() {
        let mut value = outcome(ProcessTermination::Exited(1));
        value.stdout = b"sk-issue185-secret".to_vec();
        value.stderr = b"sk-issue185-secret".to_vec();

        let error = assert_exit_code(&value, 0).unwrap_err().to_string();

        assert_secret_forms_absent(&error);
        assert!(
            error.contains("stdout_bytes: 18"),
            "missing byte count: {error}"
        );
        assert!(
            error.contains("stderr_bytes: 18"),
            "missing byte count: {error}"
        );
    }

    #[test]
    fn exact_exit_code_accepts_only_a_matching_uncontaminated_exit() {
        let clean = outcome(ProcessTermination::Exited(2));
        assert_exit_code(&clean, 2).expect("matching clean exit");
        assert_exit_failure(&clean).expect("clean nonzero exit");

        let success = outcome(ProcessTermination::Exited(0));
        assert_exit_success(&success).expect("clean zero exit");
    }

    #[test]
    fn exact_exit_code_rejects_signals_and_contamination() {
        let signal = outcome(ProcessTermination::Signaled(libc::SIGTERM));
        assert!(assert_exit_failure(&signal).is_err());
        let mut unobserved = outcome(ProcessTermination::Exited(2));
        unobserved.termination = None;
        let mut contaminated = vec![outcome(ProcessTermination::Exited(1)), signal, unobserved];
        for intervention in [
            HarnessIntervention::InterruptRequested,
            HarnessIntervention::TimedOut(Duration::from_secs(1)),
            HarnessIntervention::ForcedKill,
        ] {
            let mut value = outcome(ProcessTermination::Exited(2));
            value.interventions.push(intervention);
            contaminated.push(value);
        }

        let mut contaminated_success = outcome(ProcessTermination::Exited(0));
        contaminated_success
            .interventions
            .push(HarnessIntervention::InterruptRequested);
        assert!(assert_exit_success(&contaminated_success).is_err());

        let mut forced_cleanup = outcome(ProcessTermination::Exited(2));
        forced_cleanup.cleanup.forced = true;
        contaminated.push(forced_cleanup);

        for operation in [
            CleanupOperation::SignalProcessGroup(libc::SIGKILL),
            CleanupOperation::KillChild,
            CleanupOperation::WaitChild,
            CleanupOperation::InspectProcessGroup(ProcessInspectionOperation::ReadProc),
        ] {
            let mut value = outcome(ProcessTermination::Exited(2));
            value.cleanup.errors.push(cleanup_error(operation));
            contaminated.push(value);
        }

        let mut survivor = outcome(ProcessTermination::Exited(2));
        survivor.cleanup.surviving_pids.push(4242);
        contaminated.push(survivor);

        for value in contaminated {
            assert!(
                assert_exit_code(&value, 2).is_err(),
                "accepted contaminated outcome: {value:?}"
            );
        }
    }

    #[test]
    fn assert_no_secret_reports_location_without_storing_secret() {
        let secret = "assert-secret-value";
        let mut value = outcome(ProcessTermination::Exited(0));
        value.stdout = format!("leaked {secret}").into_bytes();

        let error = assert_no_secret(&value, secret).unwrap_err();
        assert!(matches!(
            error,
            AssertError::SecretLeak { ref location }
                if location == "command stdout/stderr"
        ));
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(!display.contains(secret));
        assert!(!debug.contains(secret));
        assert!(display.contains("command stdout/stderr"));
    }
}
