use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lattice_tooling::ToolLimits;

use super::DefaultToolExecutor;

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

fn validate_command_inputs(argv: &[String], env: &BTreeMap<String, String>) -> Result<(), String> {
    if argv.is_empty() {
        return Err("command.run requires a non-empty argv".to_string());
    }
    if argv[0].is_empty() {
        return Err("command.run requires argv[0] to name an executable".to_string());
    }
    for (index, arg) in argv.iter().enumerate() {
        if arg.contains('\0') {
            return Err(format!(
                "command.run argv[{index}] contains a NUL byte, which cannot be passed to exec"
            ));
        }
    }
    for (key, value) in env {
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(format!(
                "command.run env key '{key}' is invalid; keys must be non-empty and cannot contain '=' or NUL"
            ));
        }
        if value.contains('\0') {
            return Err(format!(
                "command.run env value for '{key}' contains a NUL byte"
            ));
        }
    }
    Ok(())
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
    validate_command_inputs(argv, env)?;

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
