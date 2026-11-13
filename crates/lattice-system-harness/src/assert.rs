// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::path::PathBuf;

use thiserror::Error;

use crate::artifacts::{
    ArtifactClaim, EvidenceError, FailureReport, capture_from_output, checked_failure_dir,
    clear_stale_bundle, render_command_text, render_failed_command_text, scan_bundle_into_report,
    write_failure_artifacts_collecting, write_world_context,
};
use crate::driver::CommandOutcome;
use crate::world::ScenarioWorld;

/// 持久化失败产物的根目录。
///
/// 可通过环境变量 `LATTICE_HARNESS_FAILURE_DIR` 覆盖（支持绝对路径或相对于测试进程当前目录的路径）。
/// 默认锚定在本 crate 清单目录下的 `<harness crate>/target/harness-failures`，保证路径为绝对路径，
/// 不受测试进程运行目录影响。`target/` 已被版本控制忽略，失败产物可供事后排查且不污染工作树；
/// `cargo xtask ci` 会将其重定向到工作区级的 `$(pwd)/target/harness-failures`，集中收集整轮场景的失败产物。
pub fn failure_dir(name: &str) -> Result<PathBuf, EvidenceError> {
    checked_failure_dir(name)
}

#[derive(Debug, Error)]
pub enum AssertError {
    #[error("预期干净退出码为 0，实际为 {actual}")]
    ExitNotSuccess { actual: String },
    #[error("预期干净非零退出，实际为 {actual}")]
    ExitNotFailure { actual: String },
    #[error("预期干净退出码为 {expected}，实际为 {actual}")]
    ExitCodeMismatch { expected: i32, actual: String },
    #[error("标准输出缺少预期子串: {expected}")]
    StdoutMissing { expected: String },
    #[error("标准错误缺少预期子串: {expected}")]
    StderrMissing { expected: String },
    #[error("输出在 {location} 处包含禁止泄露的秘密哨兵")]
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

/// 断言进程以指定的精确退出码干净退出。
/// 当公开合同明确固定了某个非零退出码（例如会话或智能体配置查找错误返回退出码 `2`）时，
/// 应使用本函数而非 `assert_exit_failure`；后者只断言非零，会放过退出码从 `2` 退化为 `1` 的问题。
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

/// 断言标准输出同时包含给定的全部子串；失败时返回以逗号拼接的缺失子串列表。
/// 该语义断言辅助函数与 `assert_exit_success` 配合传入 `finalize_with_failure_artifacts`，
/// 使语义断言失败时同样落盘标准输出与标准错误供事后排查。
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
            location: "命令标准输出/标准错误".into(),
        })
    } else {
        Ok(())
    }
}

/// 收尾单条快速场景断言，不让失败产物落盘错误覆盖原始主断言。
///
/// 无论业务断言是否通过，都会对原始标准输出与标准错误扫描全部已知秘密，
/// 使秘密泄露与主断言并列报告，而不依赖主断言先行通过。
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
            report.add_secret_location(format!("命令 {stream}"), secrets);
            failed = true;
        }
    }
    let cleanup = world.stop_daemon();
    let failed = failed || cleanup.is_err();
    if let Err(errors) = cleanup {
        for error in errors {
            report.add_evidence_error("清理后台", error, secrets);
        }
    }
    let diagnostic = match (&target, failed) {
        // 失败时显示已脱敏输出的开头部分，并指向证据目录里的完整脱敏副本。
        (Ok(dir), true) => render_failed_command_text(
            command_summary,
            output,
            secrets,
            &dir.join("stdout.txt"),
            &dir.join("stderr.txt"),
        ),
        _ => render_command_text(command_summary, output, secrets),
    };
    report.add_command_diagnostic(diagnostic, secrets);

    let dir = match target {
        Ok(dir) => dir,
        Err(error) => {
            report.add_evidence_error("校验失败产物目录", error, secrets);
            return report.finish();
        }
    };
    let _claim = match ArtifactClaim::acquire(&dir) {
        Ok(claim) => claim,
        Err(error) => {
            report.add_evidence_error("占用失败产物目录", error, secrets);
            return report.finish();
        }
    };
    if let Err(error) = clear_stale_bundle(&dir) {
        report.add_evidence_error("清理旧失败产物", error, secrets);
        return report.finish();
    }
    if !failed {
        return report.finish();
    }
    if let Err(error) = std::fs::create_dir_all(&dir) {
        report.add_evidence_error("创建失败产物根目录", error, secrets);
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
        report.add_evidence_error("写入 world.txt", error, secrets);
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
            message: "模拟清理故障".into(),
        }
    }

    fn assert_secret_forms_absent(text: &str) {
        for form in [
            "sk-issue185-secret",
            "[115, 107, 45, 105, 115, 115, 117, 101, 49, 56, 53, 45, 115, 101, 99, 114, 101, 116]",
            "736b2d69737375653138352d736563726574",
            "c2staXNzdWUxODUtc2VjcmV0",
        ] {
            assert!(
                !text.contains(form),
                "诊断信息泄露了秘密形式 {form:?}: {text}"
            );
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
            "缺少标准输出字节计数: {error}"
        );
        assert!(
            error.contains("stderr_bytes: 18"),
            "缺少标准错误字节计数: {error}"
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
                "不应接受受污染的命令结果: {value:?}"
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
                if location == "命令标准输出/标准错误"
        ));
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(!display.contains(secret));
        assert!(!debug.contains(secret));
        assert!(display.contains("命令标准输出/标准错误"));
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
        assert!(text.contains("命令诊断:"));
        let stdout_error = text.find("写入 stdout.txt").unwrap();
        let stderr_error = text.find("写入 stderr.txt").unwrap();
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
                "lattice models",
                &value,
                &[secret],
                result,
                Ok(root.path().join(stream)),
            )
            .unwrap_err();

            let text = report.to_string();
            assert!(text.contains(&format!("- 命令 {stream}")), "{text}");
            assert_eq!(
                text.contains("business failed"),
                primary.is_some(),
                "{text}"
            );
            assert!(!text.contains(secret), "{text}");
        }
    }

    #[test]
    fn failed_exit_diagnostic_shows_redacted_output_head_and_evidence_path() {
        let secret = "fast-excerpt-secret";
        let world = ScenarioWorld::new("fast-failure-excerpt").unwrap();
        let root = tempfile::TempDir::new().unwrap();
        let dir = root.path().join("scenario");
        let mut value = outcome(ProcessTermination::Exited(2));
        value.stdout = format!("stdout head {secret}\n{}", "x".repeat(5_000)).into_bytes();
        value.stderr = b"error: profile rejected".to_vec();

        let report = finalize_with_failure_artifacts_at_target(
            &world,
            "scenario",
            "lattice send",
            &value,
            &[secret],
            assert_exit_success(&value),
            Ok(dir.clone()),
        )
        .unwrap_err()
        .to_string();

        assert!(!report.contains(secret), "诊断输出必须保持脱敏: {report}");
        assert!(
            report.contains("stdout head [REDACTED]"),
            "诊断输出必须包含脱敏后的标准输出开头: {report}"
        );
        assert!(
            report.contains("error: profile rejected"),
            "诊断输出必须包含标准错误内容: {report}"
        );
        // 2048 字节内是 `"stdout head [REDACTED]\n"`（23 字节）加 2025 个 `x`，余下 2975 个 `x`。
        assert!(
            report.contains("... [其余 2975 字节已截断]"),
            "超长标准输出必须截断为有界开头: {report}"
        );
        assert!(
            !report.contains(&"x".repeat(2_026)),
            "诊断摘要不得超过 2048 字节上限: {report}"
        );
        assert!(
            report.contains(&format!(
                "完整脱敏副本: {}",
                dir.join("stdout.txt").display()
            )),
            "诊断输出必须指向证据目录中的完整副本: {report}"
        );
        let saved = std::fs::read_to_string(dir.join("stdout.txt")).unwrap();
        assert!(
            saved.starts_with("stdout head [REDACTED]\n"),
            "证据目录副本应使用相同的脱敏结果"
        );
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
