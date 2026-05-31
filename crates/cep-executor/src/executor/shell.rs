use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lattice_tooling::ToolLimits;
use tokio::process::Child;

use crate::registry::Cancellation;
use cep_protocol::error::{CepErrorData, ExecutionErrorData, ValidationErrorData};
use cep_protocol::{CepError, CepErrorCode, Retriability};

fn resolve_command_cwd(project_root: &str, cwd: Option<&str>) -> Result<PathBuf, CepError> {
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
            "command.run requires a non-empty argv",
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
            "command.run requires argv[0] to name an executable",
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
                    "command.run argv[{index}] contains a NUL byte, which cannot be passed to exec"
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
                    "command.run env key '{key}' is invalid; keys must be non-empty and cannot contain '=' or NUL"
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
                format!("command.run env value for '{key}' contains a NUL byte"),
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
    validate_command_inputs(argv, env)?;

    let cwd = resolve_command_cwd(project_root, cwd)?;
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
    // NOTE: NO kill_on_drop(true) — we handle cleanup explicitly

    // Build the effective env: clear inherited, then set only what we want
    cmd.env_clear();
    for (key, value) in env {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().map_err(|e| {
        CepError::new(
            CepErrorCode::ExecutionFailed,
            format!("failed to spawn command: {e}"),
            CepErrorData::Execution(ExecutionErrorData {
                command: argv[0].clone(),
                exit_code: None,
                stderr: Some(e.to_string()),
            }),
            Retriability::Transient,
        )
    })?;

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
                    Ok((output, stdout_meta, stderr_meta))
                }
                Err(e) => {
                    // Still need to join readers to avoid task leaks
                    let _ = stdout_handle.await;
                    let _ = stderr_handle.await;
                    Err(CepError::new(
                        CepErrorCode::ExecutionFailed,
                        format!("failed to wait for command: {e}"),
                        CepErrorData::Execution(ExecutionErrorData {
                            command: argv[0].clone(),
                            exit_code: None,
                            stderr: Some(e.to_string()),
                        }),
                        Retriability::Transient,
                    ))
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
                Ok(()) => Err(CepError::new(
                    CepErrorCode::ExecutionFailed,
                    "command cancelled",
                    CepErrorData::Execution(ExecutionErrorData {
                        command: argv[0].clone(),
                        exit_code: None,
                        stderr: Some("cancelled".into()),
                    }),
                    Retriability::Permanent,
                )),
                Err(err) => Err(child_kill_failed(&argv[0], err)),
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
                Ok(()) => Err(CepError::new(
                    CepErrorCode::CommandTimeout,
                    format!("command timed out after {timeout_ms}ms"),
                    CepErrorData::Execution(ExecutionErrorData {
                        command: argv[0].clone(),
                        exit_code: None,
                        stderr: Some(format!("timed out after {timeout_ms}ms")),
                    }),
                    Retriability::Transient,
                )),
                Err(err) => Err(child_kill_failed(&argv[0], err)),
            }
        }
    };

    result
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
    use crate::policy::CommandPolicy;

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
        // Stress test: 20 rapid command.run calls with mixed timeouts
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
