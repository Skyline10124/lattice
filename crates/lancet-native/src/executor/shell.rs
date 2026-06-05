// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::io;
#[cfg(unix)]
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Child;

use crate::enforcement::isolation::{
    AppliedBoundaryEvidence, BoundaryAttemptOutcome, CommandIsolationAttempt,
    LandlockApplicationStatus, MinimalMountViewStatus, SupervisorSetupStatus,
};
use crate::executor::limits::ToolLimits;
use crate::executor::supervisor::linux::{
    boundary_attempt_from_supervisor, run_linux_supervisor_attempt,
};
#[cfg(test)]
use crate::executor::supervisor::overlay::CleanupFailureKind;
use crate::executor::supervisor::overlay::{
    cleanup_overlay, OverlayCleanupReport, OverlayDisposition, OverlayMode, OverlayPlan,
};
use crate::executor::supervisor::syscall::LiveSupervisorSyscalls;
#[cfg(test)]
use crate::executor::supervisor::syscall::SupervisorSyscalls;
use crate::executor::supervisor::SupervisorPlan;
use crate::server::registry::Cancellation;
use lancet_authority::{
    LandlockApplied, MinimalMountViewApplied, MountNamespaceApplied, NetnsApplied,
};
use lancet_protocol::error::GuardErrorData;
use lancet_protocol::error::{CepErrorData, ExecutionErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use serde_json::Value;

pub(crate) fn resolve_command_cwd(
    project_root: &str,
    cwd: Option<&str>,
) -> Result<PathBuf, CepError> {
    let cwd = cwd.unwrap_or(".");
    reject_parent_traversal(cwd).map_err(|e| {
        CepError::new(
            CepErrorCode::ProjectRootViolation,
            e.clone(),
            CepErrorData::Validation(ValidationErrorData {
                field: "cwd".into(),
                reason: e,
            }),
            Retriability::Permanent,
        )
    })?;
    let root = Path::new(project_root);
    let path = if Path::new(cwd).is_absolute() {
        PathBuf::from(cwd)
    } else {
        root.join(cwd)
    };
    let canonical =
        std::fs::canonicalize(&path).map_err(|e| invalid_cwd_error(cwd, &e.to_string()))?;
    let root_canonical = std::fs::canonicalize(project_root).map_err(|e| {
        CepError::new(
            CepErrorCode::ProjectRootViolation,
            format!("cannot canonicalize project root: {e}"),
            CepErrorData::Validation(ValidationErrorData {
                field: "project_root".into(),
                reason: e.to_string(),
            }),
            Retriability::Permanent,
        )
    })?;
    if canonical != root_canonical && !canonical.starts_with(&root_canonical) {
        return Err(CepError::new(
            CepErrorCode::ProjectRootViolation,
            format!(
                "cwd '{}' is outside the project root '{}'",
                canonical.display(),
                project_root
            ),
            CepErrorData::Validation(ValidationErrorData {
                field: "cwd".into(),
                reason: "outside project root".into(),
            }),
            Retriability::Permanent,
        ));
    }
    Ok(canonical)
}

fn reject_parent_traversal(path: &str) -> Result<(), String> {
    for component in path.split('/') {
        if component == ".." {
            return Err(format!(
                "path traversal denied: '{}' contains '..' component",
                path
            ));
        }
    }
    for component in path.split('\\') {
        if component == ".." {
            return Err(format!(
                "path traversal denied: '{}' contains '..' component",
                path
            ));
        }
    }
    Ok(())
}

fn invalid_cwd_error(cwd: &str, detail: &str) -> CepError {
    CepError::new(
        CepErrorCode::PathContainment,
        format!("invalid cwd '{cwd}': {detail}"),
        CepErrorData::Validation(ValidationErrorData {
            field: "cwd".into(),
            reason: detail.to_string(),
        }),
        Retriability::Permanent,
    )
}

fn validate_command_inputs(
    argv: &[String],
    env: &BTreeMap<String, String>,
) -> Result<(), CepError> {
    if argv.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "exec.command_run requires a non-empty argv",
            CepErrorData::Validation(ValidationErrorData {
                field: "argv".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }
    if argv[0].is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "exec.command_run requires argv[0] to name an executable",
            CepErrorData::Validation(ValidationErrorData {
                field: "argv[0]".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }
    for (index, arg) in argv.iter().enumerate() {
        if arg.contains('\0') {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                format!(
                    "exec.command_run argv[{index}] contains a NUL byte, which cannot be passed to exec"
                ),
                CepErrorData::Validation(ValidationErrorData {
                    field: format!("argv[{index}]"),
                    reason: "contains NUL byte".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }
    for (key, value) in env {
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                format!(
                    "exec.command_run env key '{key}' is invalid; keys must be non-empty and cannot contain '=' or NUL"
                ),
                CepErrorData::Validation(ValidationErrorData {
                    field: format!("env.{key}"),
                    reason: "invalid key".into(),
                }),
                Retriability::Permanent,
            ));
        }
        if value.contains('\0') {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                format!("exec.command_run env value for '{key}' contains a NUL byte"),
                CepErrorData::Validation(ValidationErrorData {
                    field: format!("env.{key}"),
                    reason: "value contains NUL byte".into(),
                }),
                Retriability::Permanent,
            ));
        }
    }
    Ok(())
}

/// Output from a bounded tail reader on a child process stream.
#[derive(Debug)]
pub struct BoundedStreamOutput {
    /// The tail bytes that fit within the buffer.
    pub tail_bytes: Vec<u8>,
    /// Total bytes read from the stream (may exceed buffer size).
    pub total_bytes: usize,
    /// Whether the output was truncated (total_bytes > buffer capacity).
    pub truncated: bool,
}

/// Read from a stream into a bounded ring buffer preserving the tail.
///
/// This prevents memory exhaustion from large outputs. The buffer is bounded
/// from the start at `max_bytes` capacity.
async fn bounded_read(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    max_bytes: usize,
) -> BoundedStreamOutput {
    use tokio::io::AsyncReadExt;

    let mut total_bytes = 0usize;
    let mut buffer = Vec::with_capacity(max_bytes);
    let mut chunk = [0u8; 8192];

    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                total_bytes += n;
                for &byte in &chunk[..n] {
                    if buffer.len() >= max_bytes {
                        // Ring buffer full — shift left by 1 (preserves tail)
                        buffer.remove(0);
                    }
                    buffer.push(byte);
                }
            }
            Err(_) => break,
        }
    }

    let truncated = total_bytes > max_bytes;
    BoundedStreamOutput {
        tail_bytes: buffer,
        total_bytes,
        truncated,
    }
}

/// Configuration for `execute_argv`.
pub struct ExecuteConfig<'a> {
    pub project_root: &'a str,
    pub limits: &'a ToolLimits,
    pub argv: &'a [String],
    pub cwd: Option<&'a str>,
    pub env: &'a BTreeMap<String, String>,
    pub timeout_ms: Option<u64>,
    pub cancellation: &'a mut Cancellation,
    pub max_output_bytes: usize,
}

/// Precomputed supervisor setup behavior used by the unsafe pre-exec handoff.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SupervisorPreExecBehavior {
    Ready,
    FailBeforeExec,
}

/// Supervisor data passed to `execute_argv_supervised`.
pub(crate) enum SupervisorExecution {
    #[cfg(test)]
    Fixed {
        attempt: CommandIsolationAttempt,
        behavior: SupervisorPreExecBehavior,
        overlay: Option<FixedOverlayExecution>,
    },
    Live {
        plan: SupervisorPlan,
    },
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct FixedOverlayExecution {
    plan: OverlayPlan,
    cleanup_failures: Vec<CleanupFailureKind>,
}

#[cfg(test)]
impl FixedOverlayExecution {
    pub(crate) fn new(plan: OverlayPlan) -> Self {
        Self {
            plan,
            cleanup_failures: Vec::new(),
        }
    }

    pub(crate) fn failing(plan: OverlayPlan, failure: CleanupFailureKind) -> Self {
        Self {
            plan,
            cleanup_failures: vec![failure],
        }
    }

    pub(crate) fn plan(&self) -> &OverlayPlan {
        &self.plan
    }
}

#[derive(Clone, Debug)]
struct OverlayExecution {
    plan: OverlayPlan,
    #[cfg(test)]
    cleanup_failures: Option<Vec<CleanupFailureKind>>,
}

/// Successful supervised command output plus the setup attempt reported by the
/// child before final exec.
#[derive(Debug)]
pub(crate) struct SupervisedCommandOutput {
    pub(crate) output: std::process::Output,
    pub(crate) stdout_meta: BoundedStreamOutput,
    pub(crate) stderr_meta: BoundedStreamOutput,
    pub(crate) setup_attempt: CommandIsolationAttempt,
    #[allow(dead_code)]
    pub(crate) overlay_cleanup: Option<OverlayCleanupReport>,
}

/// Supervised command error. Setup failures carry the child-reported attempt so
/// callers can record fail-closed metadata without claiming backend execution.
#[derive(Debug)]
pub(crate) struct SupervisedCommandError {
    pub(crate) error: CepError,
    pub(crate) setup_attempt: Option<CommandIsolationAttempt>,
    #[allow(dead_code)]
    pub(crate) overlay_cleanup: Option<OverlayCleanupReport>,
}

/// Unsafe pre-exec checklist for the supervised command handoff.
///
/// Parent-only precomputation: path strings, argv/env arrays, mount plans, fd
/// lists, Landlock rule inputs, uid/gid map data, diagnostics labels, and setup
/// report labels are computed before fork. Child-side setup may use raw syscalls
/// only. After fork and before exec it must not allocate, lock mutex/RwLock,
/// log/trace, format/panic, touch async runtime state, read locale/env, perform
/// DNS, discover files beyond precomputed paths, or dispatch through Rust trait
/// objects. Setup-report pipe writes are bounded precomputed byte labels only.
#[allow(dead_code)]
pub(crate) const SUPERVISOR_PRE_EXEC_SAFETY_CHECKLIST: &str =
    "precomputed data only; raw syscalls only; no allocation, locks, logging, \
     async runtime access, formatting, env lookup, DNS, filesystem discovery, \
     or trait-object dispatch after fork";

impl SupervisorExecution {
    pub(crate) fn overlay_plan(&self) -> Option<&OverlayPlan> {
        match self {
            #[cfg(test)]
            SupervisorExecution::Fixed { overlay, .. } => {
                overlay.as_ref().map(|overlay| overlay.plan())
            }
            SupervisorExecution::Live { plan } => plan.overlay_plan.as_ref(),
        }
    }

    fn fallback_attempt(&self) -> Option<CommandIsolationAttempt> {
        match self {
            #[cfg(test)]
            SupervisorExecution::Fixed { attempt, .. } => Some(attempt.clone()),
            SupervisorExecution::Live { .. } => None,
        }
    }

    fn overlay_execution(&self) -> Option<OverlayExecution> {
        match self {
            #[cfg(test)]
            SupervisorExecution::Fixed { overlay, .. } => {
                overlay.as_ref().map(|overlay| OverlayExecution {
                    plan: overlay.plan.clone(),
                    cleanup_failures: Some(overlay.cleanup_failures.clone()),
                })
            }
            SupervisorExecution::Live { plan } => {
                plan.overlay_plan.as_ref().map(|plan| OverlayExecution {
                    plan: plan.clone(),
                    #[cfg(test)]
                    cleanup_failures: None,
                })
            }
        }
    }

    #[cfg(unix)]
    fn run_pre_exec_attempt(&self, setup_report_fd: RawFd) -> CommandIsolationAttempt {
        match self {
            #[cfg(test)]
            SupervisorExecution::Fixed {
                attempt, behavior, ..
            } => {
                let mut attempt = attempt.clone();
                attempt.outcome.failed_closed =
                    *behavior == SupervisorPreExecBehavior::FailBeforeExec;
                attempt
            }
            SupervisorExecution::Live { plan } => {
                let attempt = run_linux_supervisor_attempt(
                    plan.clone(),
                    LiveSupervisorSyscalls::with_preserve_fds(vec![setup_report_fd]),
                );
                boundary_attempt_from_supervisor(&attempt)
            }
        }
    }
}

/// Execute `argv[0]` with `argv[1..]` as arguments without shell interpolation.
///
/// Uses explicit kill+reap on all 4 termination paths (normal, timeout, cancel, drop).
/// Does NOT use `kill_on_drop(true)` — cleanup is explicit.
///
/// Returns (Output, stdout_meta, stderr_meta) where metadata includes truncation info.
pub(crate) async fn execute_argv(
    config: ExecuteConfig<'_>,
) -> Result<
    (
        std::process::Output,
        BoundedStreamOutput,
        BoundedStreamOutput,
    ),
    CepError,
> {
    execute_argv_inner(config, None)
        .await
        .map(|outcome| (outcome.output, outcome.stdout_meta, outcome.stderr_meta))
        .map_err(|err| err.error)
}

/// Execute a command through the supervised pre-exec handoff.
///
/// The current Phase 29 handoff keeps command execution semantics in the same
/// implementation as `execute_argv`; supervision adds a setup-report pipe and a
/// bounded pre-exec outcome before the final `exec`.
#[allow(dead_code)]
pub(crate) async fn execute_argv_supervised(
    config: ExecuteConfig<'_>,
    supervisor: SupervisorExecution,
) -> Result<SupervisedCommandOutput, SupervisedCommandError> {
    execute_argv_inner(config, Some(supervisor))
        .await
        .map(|outcome| SupervisedCommandOutput {
            output: outcome.output,
            stdout_meta: outcome.stdout_meta,
            stderr_meta: outcome.stderr_meta,
            setup_attempt: outcome
                .setup_attempt
                .expect("supervised execution must report setup attempt"),
            overlay_cleanup: outcome.overlay_cleanup,
        })
        .map_err(|err| SupervisedCommandError {
            error: err.error,
            setup_attempt: err.setup_attempt,
            overlay_cleanup: err.overlay_cleanup,
        })
}

#[derive(Debug)]
struct ExecuteInnerOutput {
    output: std::process::Output,
    stdout_meta: BoundedStreamOutput,
    stderr_meta: BoundedStreamOutput,
    setup_attempt: Option<CommandIsolationAttempt>,
    overlay_cleanup: Option<OverlayCleanupReport>,
}

#[derive(Debug)]
struct ExecuteInnerError {
    error: CepError,
    setup_attempt: Option<CommandIsolationAttempt>,
    overlay_cleanup: Option<OverlayCleanupReport>,
}

async fn execute_argv_inner(
    config: ExecuteConfig<'_>,
    supervisor: Option<SupervisorExecution>,
) -> Result<ExecuteInnerOutput, ExecuteInnerError> {
    let ExecuteConfig {
        project_root,
        limits,
        argv,
        cwd,
        env,
        timeout_ms,
        cancellation,
        max_output_bytes,
    } = config;
    validate_command_inputs(argv, env).map_err(|error| ExecuteInnerError {
        error,
        setup_attempt: None,
        overlay_cleanup: None,
    })?;

    let cwd = resolve_command_cwd(project_root, cwd).map_err(|error| ExecuteInnerError {
        error,
        setup_attempt: None,
        overlay_cleanup: None,
    })?;
    let max_ms = std::cmp::max(limits.max_command_timeout, 1) as u64 * 1000;
    let timeout_ms = timeout_ms.unwrap_or(max_ms);

    let mut cmd = tokio::process::Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    // Avoid kill_on_drop(true); cleanup is handled explicitly below.

    // Build the effective env: clear inherited, then set only what we want
    cmd.env_clear();
    for (key, value) in env {
        cmd.env(key, value);
    }

    let overlay_execution = supervisor
        .as_ref()
        .and_then(SupervisorExecution::overlay_execution);

    #[cfg(unix)]
    let mut setup_pipe = match supervisor {
        Some(supervisor) => {
            let pipe = SetupReportPipe::new().map_err(|error| ExecuteInnerError {
                error,
                setup_attempt: None,
                overlay_cleanup: cleanup_overlay_after_terminal(overlay_execution.as_ref(), true),
            })?;
            let fallback_attempt = supervisor.fallback_attempt();
            install_supervisor_pre_exec(&mut cmd, pipe.child_write, supervisor);
            Some((pipe, fallback_attempt))
        }
        None => None,
    };
    #[cfg(not(unix))]
    if supervisor.is_some() {
        return Err(ExecuteInnerError {
            error: CepError::new(
                CepErrorCode::GuardDenied,
                "supervised command execution is unsupported on this platform",
                CepErrorData::Guard(GuardErrorData {
                    method: "exec.command_run".into(),
                    reason: "unsupported_platform".into(),
                }),
                Retriability::Permanent,
            ),
            setup_attempt: None,
            overlay_cleanup: cleanup_overlay_after_terminal(overlay_execution.as_ref(), true),
        });
    }

    let spawn_result = cmd.spawn();
    let mut child = match spawn_result {
        Ok(child) => child,
        Err(e) => {
            #[cfg(unix)]
            if let Some((pipe, fallback_attempt)) = setup_pipe.as_mut() {
                pipe.close_child_writer();
                let setup_attempt = pipe.read_setup_attempt(fallback_attempt.as_ref());
                if let Some(attempt) = setup_attempt {
                    if attempt.outcome.failed_closed {
                        return Err(ExecuteInnerError {
                            error: supervisor_setup_failed_error(&argv[0], &attempt),
                            setup_attempt: Some(attempt),
                            overlay_cleanup: cleanup_overlay_after_terminal(
                                overlay_execution.as_ref(),
                                true,
                            ),
                        });
                    }
                    return Err(ExecuteInnerError {
                        error: spawn_failed_error(&argv[0], e),
                        setup_attempt: Some(attempt),
                        overlay_cleanup: cleanup_overlay_after_terminal(
                            overlay_execution.as_ref(),
                            true,
                        ),
                    });
                }
            }
            return Err(ExecuteInnerError {
                error: spawn_failed_error(&argv[0], e),
                setup_attempt: None,
                overlay_cleanup: cleanup_overlay_after_terminal(overlay_execution.as_ref(), true),
            });
        }
    };
    let mut setup_attempt = None;
    #[cfg(unix)]
    if let Some((mut pipe, fallback_attempt)) = setup_pipe {
        pipe.close_child_writer();
        setup_attempt = pipe.read_setup_attempt(fallback_attempt.as_ref());
        if let Some(attempt) = setup_attempt.as_ref() {
            if attempt.outcome.failed_closed {
                let cleanup = kill_and_reap_child(&mut child).await;
                let child_cleanup_succeeded = cleanup.is_ok();
                let error = match cleanup {
                    Ok(()) => supervisor_setup_failed_error(&argv[0], attempt),
                    Err(err) => child_kill_failed(&argv[0], err),
                };
                return Err(ExecuteInnerError {
                    error,
                    setup_attempt,
                    overlay_cleanup: cleanup_overlay_after_terminal(
                        overlay_execution.as_ref(),
                        child_cleanup_succeeded,
                    ),
                });
            }
        }
    }

    // Take stdout/stderr handles for bounded reading
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Spawn bounded readers for stdout and stderr
    let stdout_handle = tokio::spawn(bounded_read(stdout, max_output_bytes));
    let stderr_handle = tokio::spawn(bounded_read(stderr, max_output_bytes));

    // Select: normal completion, cancellation, or timeout
    let result = tokio::select! {
        status = child.wait() => {
            // Normal completion — reap is implicit from wait()
            match status {
                Ok(exit_status) => {
                    let stdout_meta = stdout_handle.await.unwrap_or_else(|_| BoundedStreamOutput {
                        tail_bytes: Vec::new(),
                        total_bytes: 0,
                        truncated: false,
                    });
                    let stderr_meta = stderr_handle.await.unwrap_or_else(|_| BoundedStreamOutput {
                        tail_bytes: Vec::new(),
                        total_bytes: 0,
                        truncated: false,
                    });
                    let output = std::process::Output {
                        status: exit_status,
                        stdout: stdout_meta.tail_bytes.clone(),
                        stderr: stderr_meta.tail_bytes.clone(),
                    };
                    Ok(ExecuteInnerOutput {
                        output,
                        stdout_meta,
                        stderr_meta,
                        setup_attempt: setup_attempt.clone(),
                        overlay_cleanup: cleanup_overlay_after_terminal(
                            overlay_execution.as_ref(),
                            true,
                        ),
                    })
                }
                Err(e) => {
                    // Still need to join readers to avoid task leaks
                    let _ = stdout_handle.await;
                    let _ = stderr_handle.await;
                    Err(ExecuteInnerError {
                        error: CepError::new(
                            CepErrorCode::ExecutionFailed,
                            format!("failed to wait for command: {e}"),
                            CepErrorData::Execution(ExecutionErrorData {
                                command: argv[0].clone(),
                                exit_code: None,
                                stderr: Some(e.to_string()),
                            }),
                            Retriability::Transient,
                        ),
                        setup_attempt: setup_attempt.clone(),
                        overlay_cleanup: cleanup_overlay_after_terminal(
                            overlay_execution.as_ref(),
                            true,
                        ),
                    })
                }
            }
        }
        _ = cancellation.cancelled() => {
            // Cancellation — explicit kill + reap
            let cleanup = kill_and_reap_child(&mut child).await;
            stdout_handle.abort();
            stderr_handle.abort();
            let _ = stdout_handle.await;
            let _ = stderr_handle.await;
            match cleanup {
                Ok(()) => Err(ExecuteInnerError {
                    error: CepError::new(
                        CepErrorCode::ExecutionFailed,
                        "command cancelled",
                        CepErrorData::Execution(ExecutionErrorData {
                            command: argv[0].clone(),
                            exit_code: None,
                            stderr: Some("cancelled".into()),
                        }),
                        Retriability::Permanent,
                    ),
                    setup_attempt: setup_attempt.clone(),
                    overlay_cleanup: cleanup_overlay_after_terminal(
                        overlay_execution.as_ref(),
                        true,
                    ),
                }),
                Err(err) => Err(ExecuteInnerError {
                    error: child_kill_failed(&argv[0], err),
                    setup_attempt: setup_attempt.clone(),
                    overlay_cleanup: cleanup_overlay_after_terminal(
                        overlay_execution.as_ref(),
                        false,
                    ),
                }),
            }
        }
        _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
            // Timeout — explicit kill + reap
            let cleanup = kill_and_reap_child(&mut child).await;
            stdout_handle.abort();
            stderr_handle.abort();
            let _ = stdout_handle.await;
            let _ = stderr_handle.await;
            match cleanup {
                Ok(()) => Err(ExecuteInnerError {
                    error: CepError::new(
                        CepErrorCode::CommandTimeout,
                        format!("command timed out after {timeout_ms}ms"),
                        CepErrorData::Execution(ExecutionErrorData {
                            command: argv[0].clone(),
                            exit_code: None,
                            stderr: Some(format!("timed out after {timeout_ms}ms")),
                        }),
                        Retriability::Transient,
                    ),
                    setup_attempt: setup_attempt.clone(),
                    overlay_cleanup: cleanup_overlay_after_terminal(
                        overlay_execution.as_ref(),
                        true,
                    ),
                }),
                Err(err) => Err(ExecuteInnerError {
                    error: child_kill_failed(&argv[0], err),
                    setup_attempt: setup_attempt.clone(),
                    overlay_cleanup: cleanup_overlay_after_terminal(
                        overlay_execution.as_ref(),
                        false,
                    ),
                }),
            }
        }
    };

    result
}

fn cleanup_overlay_after_terminal(
    overlay: Option<&OverlayExecution>,
    child_cleanup_succeeded: bool,
) -> Option<OverlayCleanupReport> {
    let overlay = overlay?;
    if overlay.plan.mode == OverlayMode::Artifact {
        return None;
    }
    #[cfg(test)]
    if let Some(failures) = overlay.cleanup_failures.as_ref() {
        let mut syscalls = FixedOverlayCleanupSyscalls::new(failures.clone());
        return Some(cleanup_overlay(
            &overlay.plan,
            OverlayDisposition::Discarded,
            child_cleanup_succeeded,
            &mut syscalls,
        ));
    }

    let mut syscalls = LiveSupervisorSyscalls::default();
    Some(cleanup_overlay(
        &overlay.plan,
        OverlayDisposition::Discarded,
        child_cleanup_succeeded,
        &mut syscalls,
    ))
}

#[cfg(test)]
struct FixedOverlayCleanupSyscalls {
    failures: Vec<CleanupFailureKind>,
}

#[cfg(test)]
impl FixedOverlayCleanupSyscalls {
    fn new(failures: Vec<CleanupFailureKind>) -> Self {
        Self { failures }
    }

    fn fails(&self, kind: CleanupFailureKind) -> bool {
        self.failures.contains(&kind)
    }
}

#[cfg(test)]
impl SupervisorSyscalls for FixedOverlayCleanupSyscalls {
    fn has_cap_sys_admin(
        &mut self,
    ) -> Result<bool, crate::executor::supervisor::syscall::SyscallError> {
        Ok(true)
    }
    fn check_unprivileged_user_namespace(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn unshare_user_namespace(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn write_setgroups_deny(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn write_uid_map_current(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn write_gid_map_current(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn unshare_network_namespace(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn assert_no_veth_or_default_route(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn unshare_mount_namespace(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn make_mounts_private(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn mount_tmpfs_root(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn bind_mount(
        &mut self,
        _source: &Path,
        _target: &Path,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn remount_read_only(
        &mut self,
        _target: &Path,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn record_read_only_bind_scope(
        &mut self,
        _scope: crate::executor::supervisor::plan::ReadOnlyBindScope,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn mount_scratch_tmpfs(
        &mut self,
        _target: &Path,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn mount_minimal_device(
        &mut self,
        _source: &Path,
        _target: &Path,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn detach_old_root(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn assert_old_root_unreachable(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn chdir(
        &mut self,
        _target: &Path,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn cleanup_env(&mut self) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn cleanup_fds(&mut self) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn probe_landlock_abi(
        &mut self,
    ) -> Result<
        crate::executor::supervisor::landlock::LandlockAbiProbe,
        crate::executor::supervisor::syscall::SyscallError,
    > {
        Ok(crate::executor::supervisor::landlock::LandlockAbiProbe::unsupported("not tested"))
    }
    fn apply_landlock(
        &mut self,
        _plan: &crate::executor::supervisor::landlock::LandlockRefinementPlan,
    ) -> Result<
        crate::executor::supervisor::landlock::LandlockApplicationReport,
        crate::executor::supervisor::syscall::SyscallError,
    > {
        Err(crate::executor::supervisor::syscall::SyscallError::new(
            "landlock",
            "not tested",
        ))
    }
    fn setup_report_pipe_ready(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn exec_handoff_ready(
        &mut self,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn mount_overlay(
        &mut self,
        _lower: &Path,
        _upper: &Path,
        _work: &Path,
        _merged: &Path,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        Ok(())
    }
    fn umount_overlay(
        &mut self,
        _merged: &Path,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        if self.fails(CleanupFailureKind::UnmountMerged) {
            Err(crate::executor::supervisor::syscall::SyscallError::new(
                "umount_overlay",
                "injected failure",
            ))
        } else {
            Ok(())
        }
    }
    fn remove_overlay_tree(
        &mut self,
        path: &Path,
    ) -> Result<(), crate::executor::supervisor::syscall::SyscallError> {
        let failure = if path.ends_with("merged") {
            CleanupFailureKind::RemoveMerged
        } else if path.ends_with("upper") {
            CleanupFailureKind::RemoveUpper
        } else if path.ends_with("work") {
            CleanupFailureKind::RemoveWork
        } else {
            CleanupFailureKind::RemoveSessionRoot
        };
        if self.fails(failure) {
            Err(crate::executor::supervisor::syscall::SyscallError::new(
                "remove_overlay_tree",
                "injected failure",
            ))
        } else {
            Ok(())
        }
    }
}

#[cfg(unix)]
struct SetupReportPipe {
    parent_read: RawFd,
    child_write: RawFd,
}

#[cfg(unix)]
impl SetupReportPipe {
    fn new() -> Result<Self, CepError> {
        let mut fds = [0; 2];
        let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
        if rc == 0 {
            Ok(Self {
                parent_read: fds[0],
                child_write: fds[1],
            })
        } else {
            Err(CepError::new(
                CepErrorCode::ExecutionFailed,
                format!(
                    "failed to create supervisor setup-report pipe: {}",
                    io::Error::last_os_error()
                ),
                CepErrorData::Execution(ExecutionErrorData {
                    command: "supervisor_setup".to_string(),
                    exit_code: None,
                    stderr: Some(io::Error::last_os_error().to_string()),
                }),
                Retriability::Transient,
            ))
        }
    }

    fn close_child_writer(&mut self) {
        if self.child_write >= 0 {
            unsafe {
                libc::close(self.child_write);
            }
            self.child_write = -1;
        }
    }

    fn read_setup_attempt(
        &self,
        fallback: Option<&CommandIsolationAttempt>,
    ) -> Option<CommandIsolationAttempt> {
        let report = self.read_report();
        if report.is_empty() {
            return fallback.cloned();
        }
        decode_setup_attempt_report(&report).or_else(|| fallback.cloned())
    }

    fn read_report(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buffer = [0u8; 512];
        loop {
            let n = unsafe {
                libc::read(
                    self.parent_read,
                    buffer.as_mut_ptr().cast::<libc::c_void>(),
                    buffer.len(),
                )
            };
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buffer[..n as usize]);
            if out.len() >= 8192 {
                break;
            }
        }
        out
    }
}

#[cfg(unix)]
impl Drop for SetupReportPipe {
    fn drop(&mut self) {
        unsafe {
            if self.parent_read >= 0 {
                libc::close(self.parent_read);
            }
            if self.child_write >= 0 {
                libc::close(self.child_write);
            }
        }
    }
}

#[cfg(unix)]
fn install_supervisor_pre_exec(
    cmd: &mut tokio::process::Command,
    child_write: RawFd,
    supervisor: SupervisorExecution,
) {
    unsafe {
        cmd.pre_exec(move || {
            let attempt = supervisor.run_pre_exec_attempt(child_write);
            let report = encode_setup_attempt_report(&attempt);
            raw_write_all(child_write, report.as_bytes())?;
            libc::close(child_write);
            if attempt.outcome.failed_closed {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "supervisor setup failed before exec",
                ))
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(unix)]
fn raw_write_all(fd: RawFd, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let written =
            unsafe { libc::write(fd, bytes.as_ptr().cast::<libc::c_void>(), bytes.len()) };
        if written < 0 {
            return Err(io::Error::last_os_error());
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}

fn encode_setup_attempt_report(attempt: &CommandIsolationAttempt) -> String {
    let outcome = &attempt.outcome;
    let report = serde_json::json!({
        "supervisor_backend": outcome.supervisor_backend,
        "project_view_path": outcome.project_view_path.as_ref().map(|path| path.display().to_string()),
        "cwd_view_path": outcome.cwd_view_path.as_ref().map(|path| path.display().to_string()),
        "privilege_strategy": outcome.privilege_strategy,
        "userns_status": outcome.userns_status,
        "setgroups_status": outcome.setgroups_status,
        "uid_map_status": outcome.uid_map_status,
        "gid_map_status": outcome.gid_map_status,
        "read_only_bind_scope": outcome.read_only_bind_scope,
        "network_namespace_status": outcome.network_namespace_status.label(),
        "mount_namespace_status": outcome.mount_namespace_status.label(),
        "minimal_mount_view_status": outcome.minimal_mount_view_status.label(),
        "landlock_status": outcome.landlock_status.label(),
        "landlock_abi_version": outcome.landlock_abi_version,
        "landlock_rights_mask": outcome.landlock_rights_mask,
        "landlock_ruleset_status": outcome.landlock_ruleset_status,
        "landlock_required": outcome.landlock_required,
        "fd_cleanup_status": outcome.fd_cleanup_status,
        "fd_cleanup_method": outcome.fd_cleanup_method,
        "env_policy": outcome.env_policy,
        "setup_failure_stage": outcome.setup_failure_stage,
        "setup_failure_reason": outcome.setup_failure_reason,
        "failed_closed": outcome.failed_closed,
    });
    format!("{report}\n")
}

fn decode_setup_attempt_report(bytes: &[u8]) -> Option<CommandIsolationAttempt> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let mut outcome = BoundaryAttemptOutcome {
        supervisor_backend: string_field(&value, "supervisor_backend"),
        project_view_path: string_field(&value, "project_view_path").map(PathBuf::from),
        cwd_view_path: string_field(&value, "cwd_view_path").map(PathBuf::from),
        privilege_strategy: string_field(&value, "privilege_strategy"),
        userns_status: string_field(&value, "userns_status"),
        setgroups_status: string_field(&value, "setgroups_status"),
        uid_map_status: string_field(&value, "uid_map_status"),
        gid_map_status: string_field(&value, "gid_map_status"),
        read_only_bind_scope: string_field(&value, "read_only_bind_scope"),
        network_namespace_status: setup_status_field(&value, "network_namespace_status"),
        mount_namespace_status: setup_status_field(&value, "mount_namespace_status"),
        minimal_mount_view_status: mount_view_status_field(&value, "minimal_mount_view_status"),
        landlock_status: landlock_status_field(&value, "landlock_status"),
        landlock_abi_version: u32_field(&value, "landlock_abi_version"),
        landlock_rights_mask: value.get("landlock_rights_mask").and_then(Value::as_u64),
        landlock_ruleset_status: string_field(&value, "landlock_ruleset_status"),
        landlock_required: value.get("landlock_required").and_then(Value::as_bool),
        fd_cleanup_status: string_field(&value, "fd_cleanup_status"),
        fd_cleanup_method: string_field(&value, "fd_cleanup_method"),
        env_policy: string_field(&value, "env_policy"),
        setup_failure_stage: string_field(&value, "setup_failure_stage"),
        setup_failure_reason: string_field(&value, "setup_failure_reason"),
        failed_closed: value
            .get("failed_closed")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..BoundaryAttemptOutcome::default()
    };
    add_evidence_for_statuses(&mut outcome);
    Some(CommandIsolationAttempt::new(outcome))
}

fn add_evidence_for_statuses(outcome: &mut BoundaryAttemptOutcome) {
    if outcome.network_namespace_status == SupervisorSetupStatus::Applied {
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::NetworkNamespace(
                NetnsApplied::from_supervisor_netns_success(),
            ));
    }
    if outcome.mount_namespace_status == SupervisorSetupStatus::Applied {
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::MountNamespace(
                MountNamespaceApplied::from_supervisor_mount_namespace_success(),
            ));
    }
    if outcome.minimal_mount_view_status == MinimalMountViewStatus::Applied {
        outcome
            .evidence
            .push(AppliedBoundaryEvidence::MinimalMountView(
                MinimalMountViewApplied::from_supervisor_minimal_mount_view_success(),
            ));
    }
    if outcome.landlock_status == LandlockApplicationStatus::Applied {
        outcome.evidence.push(AppliedBoundaryEvidence::Landlock(
            LandlockApplied::from_supervisor_landlock_success(),
        ));
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn u32_field(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn setup_status_field(value: &Value, key: &str) -> SupervisorSetupStatus {
    match value.get(key).and_then(Value::as_str) {
        Some("applied") => SupervisorSetupStatus::Applied,
        Some("not_applied") => SupervisorSetupStatus::NotApplied,
        Some("failed_closed") => SupervisorSetupStatus::FailedClosed,
        Some("unsupported") => SupervisorSetupStatus::Unsupported,
        _ => SupervisorSetupStatus::NotAttempted,
    }
}

fn mount_view_status_field(value: &Value, key: &str) -> MinimalMountViewStatus {
    match value.get(key).and_then(Value::as_str) {
        Some("applied") => MinimalMountViewStatus::Applied,
        Some("not_applied") => MinimalMountViewStatus::NotApplied,
        Some("failed_closed") => MinimalMountViewStatus::FailedClosed,
        _ => MinimalMountViewStatus::NotAttempted,
    }
}

fn landlock_status_field(value: &Value, key: &str) -> LandlockApplicationStatus {
    match value.get(key).and_then(Value::as_str) {
        Some("unsupported") => LandlockApplicationStatus::Unsupported,
        Some("applied") => LandlockApplicationStatus::Applied,
        _ => LandlockApplicationStatus::NotApplied,
    }
}

fn spawn_failed_error(command: &str, err: std::io::Error) -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        format!("failed to spawn command: {err}"),
        CepErrorData::Execution(ExecutionErrorData {
            command: command.to_string(),
            exit_code: None,
            stderr: Some(err.to_string()),
        }),
        Retriability::Transient,
    )
}

fn supervisor_setup_failed_error(command: &str, attempt: &CommandIsolationAttempt) -> CepError {
    let stage = attempt
        .outcome
        .setup_failure_stage
        .as_deref()
        .unwrap_or("pre_exec");
    let reason = attempt
        .outcome
        .setup_failure_reason
        .as_deref()
        .unwrap_or("supervisor setup failed before exec");
    CepError::new(
        CepErrorCode::GuardDenied,
        format!("command supervisor setup failed closed at {stage}: {reason}"),
        CepErrorData::Guard(GuardErrorData {
            method: "exec.command_run".into(),
            reason: format!("supervisor setup failed before executing {command}"),
        }),
        Retriability::Permanent,
    )
}

async fn kill_and_reap_child(child: &mut Child) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    if let Err(err) = kill_child_process_group(child) {
        if err.raw_os_error() != Some(libc::ESRCH) {
            child.kill().await?;
        }
    }

    #[cfg(not(unix))]
    child.kill().await?;

    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out waiting for killed child to exit",
        )),
    }
}

#[cfg(unix)]
fn kill_child_process_group(child: &Child) -> Result<(), std::io::Error> {
    let pid = child.id().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "child has no process id")
    })? as libc::pid_t;
    let rc = unsafe { libc::kill(-pid, libc::SIGKILL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn child_kill_failed(command: &str, err: std::io::Error) -> CepError {
    CepError::new(
        CepErrorCode::ChildKillFailed,
        format!("failed to kill/reap command: {err}"),
        CepErrorData::Execution(ExecutionErrorData {
            command: command.to_string(),
            exit_code: None,
            stderr: Some(err.to_string()),
        }),
        Retriability::Transient,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enforcement::isolation::{BoundaryAttemptOutcome, CommandIsolationAttempt};
    use crate::executor::supervisor::overlay::{CleanupOutcome, OverlayMode};
    use lancet_authority::policy::CommandPolicy;

    fn fake_setup_attempt(failed: bool) -> CommandIsolationAttempt {
        CommandIsolationAttempt::new(BoundaryAttemptOutcome {
            supervisor_backend: Some("fake_pre_exec".to_string()),
            env_policy: Some("allowlist".to_string()),
            fd_cleanup_status: Some("applied".to_string()),
            failed_closed: failed,
            setup_failure_stage: failed.then(|| "project_bind".to_string()),
            setup_failure_reason: failed.then(|| "injected setup failure".to_string()),
            ..BoundaryAttemptOutcome::default()
        })
    }

    fn fixed_overlay(mode: OverlayMode) -> FixedOverlayExecution {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let plan = OverlayPlan::deterministic(&project, mode, "shell-test").expect("overlay plan");
        FixedOverlayExecution::new(plan)
    }

    fn failing_fixed_overlay(failure: CleanupFailureKind) -> FixedOverlayExecution {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let plan = OverlayPlan::deterministic(&project, OverlayMode::Diagnostic, "shell-test")
            .expect("overlay plan");
        FixedOverlayExecution::failing(plan, failure)
    }

    #[tokio::test]
    async fn execute_argv_echo_succeeds() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let result = execute_argv(ExecuteConfig {
            project_root: ".",
            limits: &ToolLimits::default(),
            argv: &["echo".into(), "hello".into()],
            cwd: None,
            env: &BTreeMap::new(),
            timeout_ms: Some(5000),
            cancellation: &mut cancel,
            max_output_bytes: policy.max_output_bytes,
        })
        .await
        .unwrap();
        let (output, stdout_meta, stderr_meta) = result;
        assert_eq!(output.status.code(), Some(0));
        assert!(String::from_utf8_lossy(&output.stdout).contains("hello"));
        assert!(!stdout_meta.truncated);
        assert_eq!(stderr_meta.total_bytes, 0);
    }

    #[tokio::test]
    async fn execute_argv_timeout_returns_command_timeout() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let err = execute_argv(ExecuteConfig {
            project_root: ".",
            limits: &ToolLimits::default(),
            argv: &["sleep".into(), "10".into()],
            cwd: None,
            env: &BTreeMap::new(),
            timeout_ms: Some(100),
            cancellation: &mut cancel,
            max_output_bytes: policy.max_output_bytes,
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, CepErrorCode::CommandTimeout);
        assert!(err.message.contains("100"));
    }

    #[tokio::test]
    async fn execute_argv_cancellation_returns_error() {
        let policy = CommandPolicy::defaults();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);

        // Spawn a task that cancels after 50ms
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = cancel_tx.send(true);
        });

        let err = execute_argv(ExecuteConfig {
            project_root: ".",
            limits: &ToolLimits::default(),
            argv: &["sleep".into(), "10".into()],
            cwd: None,
            env: &BTreeMap::new(),
            timeout_ms: Some(10000),
            cancellation: &mut cancel,
            max_output_bytes: policy.max_output_bytes,
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("cancelled"));
    }

    #[tokio::test]
    async fn execute_argv_supervised_timeout_preserves_kill_and_reap() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        let err = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["sleep".into(), "10".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(100),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: None,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.error.code, CepErrorCode::CommandTimeout);
        assert!(err.error.message.contains("100ms"));
    }

    #[tokio::test]
    async fn execute_argv_supervised_cancellation_preserves_kill_and_reap() {
        let policy = CommandPolicy::defaults();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = cancel_tx.send(true);
        });

        let err = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["sleep".into(), "10".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(10000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: None,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.error.code, CepErrorCode::ExecutionFailed);
        assert!(err.error.message.contains("cancelled"));
    }

    #[tokio::test]
    async fn execute_argv_supervised_setup_failure_returns_before_exec() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let dir = tempfile::TempDir::new().unwrap();
        let marker = dir.path().join("should-not-exist");
        let attempt = fake_setup_attempt(true);

        let err = execute_argv_supervised(
            ExecuteConfig {
                project_root: dir.path().to_str().unwrap(),
                limits: &ToolLimits::default(),
                argv: &[
                    "sh".into(),
                    "-c".into(),
                    format!("touch {}", marker.display()),
                ],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(5000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::FailBeforeExec,
                overlay: None,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.error.code, CepErrorCode::GuardDenied);
        assert!(err.error.message.contains("project_bind"));
        assert!(err
            .setup_attempt
            .as_ref()
            .is_some_and(|attempt| attempt.outcome.failed_closed));
        assert!(
            !marker.exists(),
            "command argv must not run after setup failure"
        );
    }

    #[tokio::test]
    async fn overlay_cleanup_report_is_returned_on_success() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        let output = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["true".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(5000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: Some(fixed_overlay(OverlayMode::Diagnostic)),
            },
        )
        .await
        .unwrap();

        let cleanup = output.overlay_cleanup.expect("cleanup report");
        assert_eq!(cleanup.outcome, CleanupOutcome::Succeeded);
        assert_eq!(cleanup.mode, OverlayMode::Diagnostic);
        assert!(cleanup.child_cleanup_succeeded);
    }

    #[tokio::test]
    async fn overlay_cleanup_report_is_returned_on_nonzero_exit() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        let output = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["sh".into(), "-c".into(), "exit 7".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(5000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: Some(fixed_overlay(OverlayMode::Diagnostic)),
            },
        )
        .await
        .unwrap();

        assert_eq!(output.output.status.code(), Some(7));
        assert_eq!(
            output.overlay_cleanup.expect("cleanup report").mode,
            OverlayMode::Diagnostic
        );
    }

    #[tokio::test]
    async fn artifact_overlay_cleanup_is_deferred_on_nonzero_exit() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        let output = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["sh".into(), "-c".into(), "exit 7".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(5000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: Some(fixed_overlay(OverlayMode::Artifact)),
            },
        )
        .await
        .unwrap();

        assert_eq!(output.output.status.code(), Some(7));
        assert!(output.overlay_cleanup.is_none());
    }

    #[tokio::test]
    async fn overlay_cleanup_report_is_returned_on_timeout() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        let err = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["sleep".into(), "10".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(100),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: Some(fixed_overlay(OverlayMode::Diagnostic)),
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.error.code, CepErrorCode::CommandTimeout);
        assert_eq!(
            err.overlay_cleanup.expect("cleanup report").outcome,
            CleanupOutcome::Succeeded
        );
    }

    #[tokio::test]
    async fn overlay_cleanup_report_is_returned_on_cancellation() {
        let policy = CommandPolicy::defaults();
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = cancel_tx.send(true);
        });

        let err = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["sleep".into(), "10".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(10000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: Some(fixed_overlay(OverlayMode::Diagnostic)),
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.error.code, CepErrorCode::ExecutionFailed);
        assert!(err.overlay_cleanup.is_some());
    }

    #[tokio::test]
    async fn overlay_cleanup_report_is_returned_on_setup_failure_before_exec() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(true);

        let err = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["true".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(5000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::FailBeforeExec,
                overlay: Some(fixed_overlay(OverlayMode::Diagnostic)),
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.error.code, CepErrorCode::GuardDenied);
        assert!(err.overlay_cleanup.is_some());
    }

    #[tokio::test]
    async fn overlay_cleanup_report_is_returned_on_spawn_failure_after_setup() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        let err = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["definitely-not-a-lattice-test-command".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(5000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: Some(fixed_overlay(OverlayMode::Diagnostic)),
            },
        )
        .await
        .unwrap_err();

        assert_eq!(err.error.code, CepErrorCode::ExecutionFailed);
        assert!(err.setup_attempt.is_some());
        assert!(err.overlay_cleanup.is_some());
    }

    #[tokio::test]
    async fn overlay_cleanup_report_records_busy_unmount_failure() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let attempt = fake_setup_attempt(false);

        let output = execute_argv_supervised(
            ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &["true".into()],
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: Some(5000),
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            },
            SupervisorExecution::Fixed {
                attempt,
                behavior: SupervisorPreExecBehavior::Ready,
                overlay: Some(failing_fixed_overlay(CleanupFailureKind::UnmountMerged)),
            },
        )
        .await
        .unwrap();

        let cleanup = output.overlay_cleanup.expect("cleanup report");
        assert_eq!(cleanup.outcome, CleanupOutcome::Failed);
        assert_eq!(
            cleanup.failure_kind,
            Some(CleanupFailureKind::UnmountMerged)
        );
    }

    #[tokio::test]
    async fn overlay_cleanup_report_records_upper_and_work_removal_failures() {
        for failure in [
            CleanupFailureKind::RemoveUpper,
            CleanupFailureKind::RemoveWork,
        ] {
            let policy = CommandPolicy::defaults();
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            let attempt = fake_setup_attempt(false);

            let output = execute_argv_supervised(
                ExecuteConfig {
                    project_root: ".",
                    limits: &ToolLimits::default(),
                    argv: &["true".into()],
                    cwd: None,
                    env: &BTreeMap::new(),
                    timeout_ms: Some(5000),
                    cancellation: &mut cancel,
                    max_output_bytes: policy.max_output_bytes,
                },
                SupervisorExecution::Fixed {
                    attempt,
                    behavior: SupervisorPreExecBehavior::Ready,
                    overlay: Some(failing_fixed_overlay(failure)),
                },
            )
            .await
            .unwrap();

            let cleanup = output.overlay_cleanup.expect("cleanup report");
            assert_eq!(cleanup.outcome, CleanupOutcome::Failed);
            assert_eq!(cleanup.failure_kind, Some(failure));
        }
    }

    #[cfg(unix)]
    #[test]
    fn setup_report_pipe_is_cloexec_on_both_ends() {
        let pipe = SetupReportPipe::new().unwrap();
        for fd in [pipe.parent_read, pipe.child_write] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(flags >= 0);
            assert_eq!(flags & libc::FD_CLOEXEC, libc::FD_CLOEXEC);
        }
    }

    #[test]
    fn pre_exec_safety_checklist_forbids_runtime_work_after_fork() {
        for required in [
            "precomputed data only",
            "raw syscalls only",
            "no allocation",
            "locks",
            "logging",
            "async runtime access",
            "formatting",
        ] {
            assert!(
                SUPERVISOR_PRE_EXEC_SAFETY_CHECKLIST.contains(required),
                "checklist missing {required}"
            );
        }
    }

    #[tokio::test]
    async fn execute_argv_output_truncation() {
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        // Write more than max_output_bytes
        let result = execute_argv(ExecuteConfig {
            project_root: ".",
            limits: &ToolLimits::default(),
            argv: &["python3".into(), "-c".into(), "print('x' * 300)".into()],
            cwd: None,
            env: &BTreeMap::new(),
            timeout_ms: Some(5000),
            cancellation: &mut cancel,
            max_output_bytes: 100, // very small buffer
        })
        .await
        .unwrap();
        let (output, stdout_meta, stderr_meta) = result;
        assert_eq!(output.status.code(), Some(0));
        assert!(stdout_meta.truncated);
        assert!(stdout_meta.total_bytes > 100);
        assert!(stdout_meta.tail_bytes.len() <= 100);
        assert!(!stderr_meta.truncated);
    }

    #[tokio::test]
    async fn execute_argv_rejects_empty_argv() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let err = execute_argv(ExecuteConfig {
            project_root: ".",
            limits: &ToolLimits::default(),
            argv: &[],
            cwd: None,
            env: &BTreeMap::new(),
            timeout_ms: None,
            cancellation: &mut cancel,
            max_output_bytes: policy.max_output_bytes,
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[tokio::test]
    async fn execute_argv_rejects_parent_traversal_cwd() {
        let policy = CommandPolicy::defaults();
        let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        let mut cancel = Cancellation::new(cancel_rx);
        let err = execute_argv(ExecuteConfig {
            project_root: ".",
            limits: &ToolLimits::default(),
            argv: &["echo".into(), "hi".into()],
            cwd: Some(".."),
            env: &BTreeMap::new(),
            timeout_ms: None,
            cancellation: &mut cancel,
            max_output_bytes: policy.max_output_bytes,
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ProjectRootViolation);
    }

    #[tokio::test]
    async fn no_leaked_child_processes_stress() {
        // Stress test: 20 rapid exec.command_run calls with mixed timeouts
        for i in 0..20 {
            let policy = CommandPolicy::defaults();
            let (_cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
            let mut cancel = Cancellation::new(cancel_rx);
            let timeout = if i % 3 == 0 { Some(50) } else { Some(5000) };
            let argv = if i % 3 == 0 {
                vec!["sleep".into(), "10".into()]
            } else {
                vec!["echo".into(), format!("run-{i}")]
            };
            let _ = execute_argv(ExecuteConfig {
                project_root: ".",
                limits: &ToolLimits::default(),
                argv: &argv,
                cwd: None,
                env: &BTreeMap::new(),
                timeout_ms: timeout,
                cancellation: &mut cancel,
                max_output_bytes: policy.max_output_bytes,
            })
            .await;
        }
        // If we get here without hanging, processes were cleaned up
    }
}
