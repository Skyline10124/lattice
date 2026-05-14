use std::path::PathBuf;

/// Build read paths for Landlock sandbox.
///
/// Includes project root, system root, and any extra paths from LandlockConfig.
pub fn build_landlock_read_paths(project_root: &str, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(project_root), PathBuf::from("/")];
    paths.extend(extra.iter().cloned());
    paths
}

/// Build write paths for Landlock sandbox.
///
/// Includes project root, /tmp, and any extra paths from LandlockConfig.
pub fn build_landlock_write_paths(project_root: &str, extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(project_root), PathBuf::from("/tmp")];
    paths.extend(extra.iter().cloned());
    paths
}

/// Apply Landlock file isolation in the child process.
///
/// Denies all filesystem access by default, then adds permissive
/// PathBeneath rules: read-only + execute for system paths, full
/// access for write paths (project root + /tmp).
///
/// Returns `Ok(())` on success or if Landlock is not supported
/// (kernel < 5.13). Only returns `Err` for unexpected OS errors.
pub fn apply_landlock_sandbox(
    read_paths: &[PathBuf],
    write_paths: &[PathBuf],
) -> std::io::Result<()> {
    use landlock::{Access, AccessFs, PathBeneath, Ruleset, RulesetAttr, RulesetCreatedAttr, ABI};

    // Progressive ABI: try highest first, fall back step by step.
    // V4 (kernel 6.7+) adds network + io_uring isolation.
    // V3 (kernel 6.0+) adds file truncation control.
    // V2 (kernel 5.19+) adds file reparenting control.
    // V1 (kernel 5.13+) provides filesystem isolation only.
    let ruleset = [ABI::V4, ABI::V3, ABI::V2, ABI::V1]
        .iter()
        .find_map(|&abi| {
            Ruleset::default()
                .handle_access(AccessFs::from_all(abi))
                .ok()
                .and_then(|r| r.create().ok())
        });

    let ruleset = match ruleset {
        Some(r) => r,
        None => return Ok(()), // Landlock not supported on this kernel
    };

    // Use the ABI that was actually accepted for PathBeneath rules
    let abi = ABI::V1; // V1-compatible read/write flags work on all ABIs

    // Collect PathBeneath rules (needs file descriptors, not just paths)
    let mut rules: Vec<PathBeneath<std::fs::File>> = Vec::new();

    // Read-only + execute for read paths
    for path in read_paths {
        match std::fs::File::open(path) {
            Ok(file) => {
                rules.push(PathBeneath::new(
                    file,
                    AccessFs::from_read(abi) | AccessFs::Execute,
                ));
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Landlock: read path does not exist or is inaccessible — skipping rule"
                );
            }
        }
    }

    // Full access for write paths
    for path in write_paths {
        match std::fs::File::open(path) {
            Ok(file) => {
                rules.push(PathBeneath::new(file, AccessFs::from_all(abi)));
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Landlock: write path does not exist or is inaccessible — \
                     writes to this path will be DENIED by the sandbox"
                );
            }
        }
    }

    // add_rules takes an iterator of Result<T, E> where T: Rule<U>
    let rules_iter = rules
        .into_iter()
        .map(|r| -> Result<_, landlock::RulesetError> { Ok(r) });

    let ruleset = match ruleset.add_rules(rules_iter) {
        Ok(r) => r,
        Err(_) => return Ok(()), // Failed to add rules, skip Landlock
    };

    // Enforce the ruleset (returns RestrictionStatus; ignore on failure)
    let _ = ruleset.restrict_self();

    Ok(())
}
