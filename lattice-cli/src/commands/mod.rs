pub mod bus_status;
pub mod config_cmd;
pub mod debug;
pub mod doctor;
pub mod list_agents;
pub mod models;
pub mod new_agent;
pub mod resolve;
pub mod run;
pub mod sessions;
pub mod stats;
pub mod validate;

use std::path::{Path, PathBuf};

/// Resolve the agents directory from override, env var, HOME, or default.
/// Validates that the resolved path is not in a system-critical location.
pub fn safe_agents_dir(override_path: Option<&str>) -> PathBuf {
    let path = if let Some(p) = override_path {
        PathBuf::from(p)
    } else if let Ok(dir) = std::env::var("LATTICE_AGENTS_DIR") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".lattice").join("agents")
    } else {
        PathBuf::from(".lattice/agents")
    };

    if let Err(e) = validate_agents_dir(&path) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
    path
}

/// Reject agent directories in system-critical or world-writable paths.
pub fn validate_agents_dir(path: &Path) -> Result<(), String> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let path_str = canonical.to_string_lossy();

    let dangerous_prefixes = [
        "/etc", "/usr", "/var", "/sys", "/proc", "/dev", "/boot", "/root",
    ];
    for prefix in &dangerous_prefixes {
        if path_str.starts_with(prefix) {
            return Err(format!(
                "Agents directory '{}' is inside a system path ({}). \
                 Set LATTICE_AGENTS_DIR to a safe location (e.g., under $HOME or your project).",
                path.display(),
                prefix
            ));
        }
    }

    if path_str.starts_with("/tmp") {
        return Err(format!(
            "Agents directory '{}' is inside /tmp which is world-writable and unsafe. \
             Set LATTICE_AGENTS_DIR to a safe location.",
            path.display()
        ));
    }

    Ok(())
}
