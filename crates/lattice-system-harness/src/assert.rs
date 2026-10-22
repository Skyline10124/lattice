// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use thiserror::Error;

use crate::artifacts::{
    ArtifactClaim, EvidenceError, FailureReport, capture_from_output, checked_failure_dir,
    clear_stale_bundle, render_command_text, scan_bundle_into_report,
    write_failure_artifacts_collecting, write_world_context,
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
pub fn failure_dir(name: &str) -> Result<PathBuf, EvidenceError> {
    checked_failure_dir(name)
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
/// `finalize_with_failure_artifacts` so a semantic failure also captures
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
///
/// The raw stdout/stderr are scanned for every secret whatever the assertion
/// result, so a leak is reported next to the primary assertion instead of
/// depending on it passing first.
pub fn finalize_with_failure_artifacts(
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    output: &CommandOutcome,
    secrets: &[&str],
    result: Result<(), AssertError>,
) -> Result<(), FailureReport> {
    let target = failure_dir(scenario_name);
    finalize_with_failure_artifacts_at_target(
        world,
        scenario_name,
        command_summary,
        output,
        secrets,
        result,
        target,
    )
}

fn finalize_with_failure_artifacts_at_target(
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    output: &CommandOutcome,
    secrets: &[&str],
    result: Result<(), AssertError>,
    target: Result<PathBuf, EvidenceError>,
) -> Result<(), FailureReport> {
    let primary_assertion = result.err().map(|error| error.to_string());
    let mut failed = primary_assertion.is_some();
    let mut report = FailureReport::new(primary_assertion, secrets);
    // 产物里的副本已脱敏，扫描产物发现不了泄漏，所以在这里扫描原始输出。
    for (stream, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        if contains_secret(bytes, secrets) {
            report.add_secret_location(format!("command {stream}"), secrets);
            failed = true;
        }
    }
    let cleanup = world.stop_daemon();
    let failed = failed || cleanup.is_err();
    if let Err(errors) = cleanup {
        for error in errors {
            report.add_evidence_error("daemon cleanup", error, secrets);
        }
    }
    report.add_command_diagnostic(
        render_command_text(command_summary, output, secrets),
        secrets,
    );

    let dir = match target {
        Ok(dir) => dir,
        Err(error) => {
            report.add_evidence_error("validate artifact target", error, secrets);
            return report.finish();
        }
    };
    let _claim = match ArtifactClaim::acquire(&dir) {
        Ok(claim) => claim,
        Err(error) => {
            report.add_evidence_error("claim artifact target", error, secrets);
            return report.finish();
        }
    };
    if let Err(error) = clear_stale_bundle(&dir) {
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

pub(crate) fn contains_secret(bytes: &[u8], secrets: &[&str]) -> bool {
    secrets.iter().any(|secret| {
        !secret.is_empty()
            && bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes())
    })
}

fn persist_fast_failure_bundle(
    world: &ScenarioWorld,
    scenario_name: &str,
    command_summary: &str,
    artifacts: &crate::artifacts::FailureArtifacts,
    secrets: &[&str],
    report: &mut FailureReport,
) {
    write_failure_artifacts_collecting(artifacts, secrets, report);
    if let Err(error) = write_world_context(
        &artifacts.dir,
        world,
        scenario_name,
        command_summary,
        secrets,
    ) {
        report.add_evidence_error("write world.txt", error, secrets);
    }
    scan_bundle_into_report(&artifacts.dir, secrets, report);
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

    #[test]
    fn fast_failure_report_preserves_primary_two_write_errors_and_scan_results() {
        let secret = "fast-report-secret";
        let world = ScenarioWorld::new("fast-report-errors").unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let dir = root.path().join("scenario");
        let _claim = ArtifactClaim::acquire(&dir).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir(dir.join("stdout.txt")).unwrap();
        std::fs::create_dir(dir.join("stderr.txt")).unwrap();
        std::fs::write(dir.join("leak.txt"), format!("leak {secret}")).unwrap();
        std::os::unix::fs::symlink(dir.join("leak.txt"), dir.join("invalid-link")).unwrap();

        let mut value = outcome(ProcessTermination::Exited(1));
        value.stdout = format!("stdout {secret}").into_bytes();
        value.stderr = format!("stderr {secret}").into_bytes();
        let command_summary = format!("lattice {secret}");
        let artifacts = capture_from_output(dir, &value, &command_summary, &[secret]);
        let mut report = FailureReport::new(Some(format!("primary {secret}")), &[secret]);
        report.add_command_diagnostic(
            render_command_text(&command_summary, &value, &[secret]),
            &[secret],
        );

        persist_fast_failure_bundle(
            &world,
            "scenario",
            &command_summary,
            &artifacts,
            &[secret],
            &mut report,
        );
        let report = report.finish().unwrap_err();
        let text = report.to_string();
        assert!(!text.contains(secret));
        assert!(text.contains("primary [REDACTED]"));
        assert!(text.contains("leak.txt"));
        assert!(text.contains("invalid-link"));
        assert!(text.contains("command diagnostics:"));
        let stdout_error = text.find("write stdout.txt").unwrap();
        let stderr_error = text.find("write stderr.txt").unwrap();
        assert!(stdout_error < stderr_error);
        assert_eq!(
            std::fs::read_to_string(artifacts.dir.join("leak.txt")).unwrap(),
            "leak [REDACTED]"
        );
    }

    #[test]
    fn fast_finalizer_scans_raw_output_whatever_the_assertion_result() {
        let secret = "fast-finalizer-secret";
        let world = ScenarioWorld::new("fast-finalizer-raw-scan").unwrap();
        let root = tempfile::TempDir::new().unwrap();
        for (primary, stream) in [(None, "stdout"), (Some("business failed"), "stderr")] {
            let mut value = outcome(ProcessTermination::Exited(0));
            let leaked = format!("leaked {secret}").into_bytes();
            if stream == "stdout" {
                value.stdout = leaked;
            } else {
                value.stderr = leaked;
            }
            let result = primary.map_or(Ok(()), |expected: &str| {
                Err(AssertError::StdoutMissing {
                    expected: expected.into(),
                })
            });

            let report = finalize_with_failure_artifacts_at_target(
                &world,
                "scenario",
                "lattice doctor",
                &value,
                &[secret],
                result,
                Ok(root.path().join(stream)),
            )
            .unwrap_err();

            let text = report.to_string();
            assert!(text.contains(&format!("- command {stream}")), "{text}");
            assert_eq!(
                text.contains("business failed"),
                primary.is_some(),
                "{text}"
            );
            assert!(!text.contains(secret), "{text}");
        }
    }

    #[test]
    fn fast_success_clears_stale_bundle_without_writing_new_evidence() {
        let world = ScenarioWorld::new("fast-success-stale-cleanup").unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let dir = root.path().join("scenario");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stale.txt"), "stale failure").unwrap();
        let value = outcome(ProcessTermination::Exited(0));

        finalize_with_failure_artifacts_at_target(
            &world,
            "scenario",
            "lattice --version",
            &value,
            &[],
            Ok(()),
            Ok(dir.clone()),
        )
        .unwrap();

        assert!(!dir.exists());
        let claims = root.path().join(".claims");
        assert!(claims.is_dir());
        assert!(claims.join("scenario.lock").is_file());
    }
}
