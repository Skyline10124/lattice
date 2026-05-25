use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lattice_tooling::ToolLimits;

use super::DefaultToolExecutor;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

fn resolve_command_cwd(project_root: &str, cwd: Option<&str>) -> Result<PathBuf, String> {
    let cwd = cwd.unwrap_or(".");
    DefaultToolExecutor::reject_parent_traversal(cwd)?;
    let root = Path::new(project_root);
    let path = if Path::new(cwd).is_absolute() {
        PathBuf::from(cwd)
    } else {
        root.join(cwd)
    };
    let canonical =
        std::fs::canonicalize(&path).map_err(|e| format!("invalid cwd '{cwd}': {e}"))?;
    let root_canonical = std::fs::canonicalize(project_root)
        .map_err(|e| format!("cannot canonicalize project root: {e}"))?;
    if canonical != root_canonical && !canonical.starts_with(&root_canonical) {
        return Err(format!(
            "cwd '{}' is outside the project root '{}'",
            canonical.display(),
            project_root
        ));
    }
    Ok(canonical)
}

fn command_timeout_ms(limits: &ToolLimits, timeout_ms: Option<u64>) -> u64 {
    let max_ms = std::cmp::max(limits.max_command_timeout, 1) as u64 * 1000;
    match timeout_ms {
        Some(requested) => std::cmp::min(requested, max_ms),
        None => max_ms,
    }
}

/// Execute `argv[0]` with `argv[1..]` as arguments without shell interpolation.
pub(crate) async fn execute_argv(
    project_root: &str,
    limits: &ToolLimits,
    argv: &[String],
    cwd: Option<&str>,
    env: &BTreeMap<String, String>,
    timeout_ms: Option<u64>,
) -> Result<std::process::Output, String> {
    if argv.is_empty() {
        return Err("command.run requires a non-empty argv".to_string());
    }

    let cwd = resolve_command_cwd(project_root, cwd)?;
    let timeout_ms = command_timeout_ms(limits, timeout_ms);

    let mut cmd = tokio::process::Command::new(&argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }
    cmd.current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in env {
        cmd.env(key, value);
    }

    let child = cmd.spawn().map_err(|e| e.to_string())?;
    match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err(format!("command timed out after {timeout_ms}ms")),
    }
}

pub(crate) async fn execute_bash(
    project_root: &str,
    limits: &ToolLimits,
    command: &str,
) -> Result<std::process::Output, String> {
    check_command(command)?;
    execute_bash_command(command, limits, project_root).await
}

fn check_command(command: &str) -> Result<(), String> {
    let command = command.trim();

    if command.is_empty() {
        return Err("empty command".to_string());
    }

    if command.contains('\0') {
        return Err("command contains NUL byte".to_string());
    }

    Ok(())
}

/// Platform-specific bash execution helpers
async fn wait_with_timeout(
    child: tokio::process::Child,
    timeout_secs: u64,
    configured_timeout: u32,
) -> Result<std::process::Output, String> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(o)) => Ok(o),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!(
            "bash command timed out after {}s",
            configured_timeout
        )),
    }
}

/// Execute a bash command with platform-specific resource limits (Unix).
#[cfg(unix)]
async fn execute_bash_command(
    command: &str,
    limits: &ToolLimits,
    project_root: &str,
) -> Result<std::process::Output, String> {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(project_root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    #[cfg(target_os = "linux")]
    // SAFETY: `pre_exec` runs in the forked child process before `exec`.
    // The closure only calls `apply_resource_limits_linux` (POSIX setrlimit),
    // which is async-signal-safe. No allocations occur after fork. The
    // closure returns `Ok(())` on success or logs a warning on failure.
    unsafe {
        cmd.pre_exec(move || {
            apply_resource_limits_linux();
            Ok(())
        });
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    // SAFETY: `pre_exec` runs in the forked child process before `exec`.
    // The closure only calls `apply_resource_limits_unix` (POSIX setrlimit),
    // which is async-signal-safe. No allocations occur after fork. The
    // closure returns `Ok(())` on success or logs a warning on failure —
    // it never panics.
    unsafe {
        cmd.pre_exec(move || {
            apply_resource_limits_unix();
            Ok(())
        });
    }

    let mut tokio_cmd: tokio::process::Command = cmd.into();
    tokio_cmd.kill_on_drop(true);
    let timeout_secs = std::cmp::max(limits.max_command_timeout, 1) as u64;
    let child = tokio_cmd.spawn().map_err(|e| e.to_string())?;
    wait_with_timeout(child, timeout_secs, limits.max_command_timeout).await
}

/// Execute a bash command on non-Unix platforms.
#[cfg(not(unix))]
async fn execute_bash_command(
    command: &str,
    limits: &ToolLimits,
    project_root: &str,
) -> Result<std::process::Output, String> {
    let timeout_secs = std::cmp::max(limits.max_command_timeout, 1) as u64;
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", command])
        .current_dir(project_root)
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    wait_with_timeout(child, timeout_secs, limits.max_command_timeout).await
}

/// Apply resource limits in the child process (Linux).
///
/// - RLIMIT_AS: 512 MB virtual memory
/// - RLIMIT_NPROC: 1 process
/// - RLIMIT_CPU: 30 seconds
///
/// Failures are silently ignored (logged at trace level).
#[cfg(target_os = "linux")]
fn apply_resource_limits_linux() {
    let rlim_as = libc::rlimit {
        rlim_cur: 512 * 1024 * 1024,
        rlim_max: 512 * 1024 * 1024,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values. The return
    // value is checked; a non-zero return is logged but does not panic. This
    // call is safe because: (1) we pass stack-allocated structs with valid
    // fields, (2) the kernel validates the values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &rlim_as) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_AS) failed in child process");
    }

    let rlim_nproc = libc::rlimit {
        rlim_cur: 1,
        rlim_max: 1,
    };
    // SAFETY: Same as above — valid struct, checked return, non-fatal failure.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &rlim_nproc) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_NPROC) failed in child process");
    }

    let rlim_cpu = libc::rlimit {
        rlim_cur: 30,
        rlim_max: 30,
    };
    // SAFETY: Same as above — valid struct, checked return, non-fatal failure.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &rlim_cpu) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_CPU) failed in child process");
    }
}

/// Apply resource limits in the child process (macOS / other Unix).
///
/// - RLIMIT_AS: 512 MB virtual memory
/// - RLIMIT_NPROC: 32 (macOS limits entire process tree, not individual)
/// - RLIMIT_CPU: 30 seconds
///
/// Failures are silently ignored (logged at trace level).
#[cfg(all(unix, not(target_os = "linux")))]
fn apply_resource_limits_unix() {
    let rlim_as = libc::rlimit {
        rlim_cur: 512 * 1024 * 1024,
        rlim_max: 512 * 1024 * 1024,
    };
    // SAFETY: `setrlimit` is a POSIX syscall that sets process resource limits.
    // The `rlimit` struct fields are initialized with valid values. The return
    // value is checked; a non-zero return is logged but does not panic. This
    // call is safe because: (1) we pass stack-allocated structs with valid
    // fields, (2) the kernel validates the values, (3) failure is non-fatal.
    if unsafe { libc::setrlimit(libc::RLIMIT_AS, &rlim_as) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_AS) failed in child process");
    }

    let rlim_nproc = libc::rlimit {
        rlim_cur: 32,
        rlim_max: 32,
    };
    // SAFETY: Same as above — valid struct, checked return, non-fatal failure.
    if unsafe { libc::setrlimit(libc::RLIMIT_NPROC, &rlim_nproc) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_NPROC) failed in child process");
    }

    let rlim_cpu = libc::rlimit {
        rlim_cur: 30,
        rlim_max: 30,
    };
    // SAFETY: Same as above — valid struct, checked return, non-fatal failure.
    if unsafe { libc::setrlimit(libc::RLIMIT_CPU, &rlim_cpu) } != 0 {
        tracing::warn!("setrlimit(RLIMIT_CPU) failed in child process");
    }
}
