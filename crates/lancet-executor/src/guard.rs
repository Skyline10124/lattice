//! Guard L1 path scope enforcement for workspace mutations.
//!
//! Implements D-31, D-33, D-34, D-35, D-39, D-40, D-41, D-42.
//!
//! Guard L1 checks whether a target path falls within the active envelope's
//! `WritePath` capability scope. It distinguishes between:
//! - `ReplanRequired`: target is inside project root but outside active scope
//! - `GuardDenied`: target escapes project root or violates hard security boundary
//! - `EnvelopeMissing`: no active envelope authority exists

use lancet_protocol::envelope::Capability;
use lancet_protocol::error::{CepErrorData, GuardErrorData, ValidationErrorData};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

/// Normalized write path pattern for scope matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WritePathPattern {
    /// Matches a single file exactly (e.g., "src/main.rs").
    ExactFile(PathBuf),
    /// Matches all files under a directory (e.g., "src/**" -> base "src").
    DirectoryGlob(PathBuf),
}

/// Normalize a WritePath pattern string into a typed pattern.
///
/// Supported forms:
/// - Exact file: "src/main.rs" -> `ExactFile("src/main.rs")`
/// - Directory glob: "src/**" -> `DirectoryGlob("src")`
/// - Simple prefix: "src" -> `DirectoryGlob("src")` (normalized to dir/**)
///
/// Rejected:
/// - Patterns containing ".." -> Validation error
/// - Patterns containing NUL -> Validation error
/// - Empty patterns -> Validation error
/// - Absolute paths as pattern syntax -> Validation error
pub fn normalize_write_path_pattern(pattern: &str) -> Result<WritePathPattern, CepError> {
    // Reject empty
    if pattern.is_empty() {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "WritePath pattern must not be empty",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "must not be empty".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject NUL bytes
    if pattern.contains('\0') {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            "WritePath pattern contains NUL byte",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "contains NUL byte".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject ".." traversal
    if pattern.contains("..") {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            "WritePath pattern contains '..' traversal",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "contains '..' traversal".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject absolute paths as pattern syntax
    if Path::new(pattern).is_absolute() {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            "WritePath pattern must be project-root-relative, not absolute",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "absolute path not allowed as pattern".into(),
            }),
            Retriability::Permanent,
        ));
    }

    if pattern.starts_with('!') {
        return Err(CepError::new(
            CepErrorCode::SchemaMismatch,
            "WritePath pattern negation is not supported in Phase 22",
            CepErrorData::Validation(ValidationErrorData {
                field: "capabilities.WritePath.path_pattern".into(),
                reason: "negation is not supported".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Check for directory glob suffix FIRST, then normalize the base.
    if let Some(base) = pattern.strip_suffix("/**") {
        if base.is_empty() {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                "WritePath pattern '/**' is not valid; use a directory name before /**",
                CepErrorData::Validation(ValidationErrorData {
                    field: "capabilities.WritePath.path_pattern".into(),
                    reason: "bare '/**' not valid".into(),
                }),
                Retriability::Permanent,
            ));
        }
        let normalized = normalize_lexical(base)?;
        Ok(WritePathPattern::DirectoryGlob(normalized))
    } else {
        if pattern.contains('*')
            || pattern.contains('?')
            || pattern.contains('[')
            || pattern.contains(']')
            || pattern.contains('{')
            || pattern.contains('}')
        {
            return Err(CepError::new(
                CepErrorCode::SchemaMismatch,
                "WritePath pattern uses unsupported glob syntax",
                CepErrorData::Validation(ValidationErrorData {
                    field: "capabilities.WritePath.path_pattern".into(),
                    reason: "unsupported glob syntax".into(),
                }),
                Retriability::Permanent,
            ));
        }

        if let Some(base) = pattern.strip_suffix('/') {
            if base.is_empty() {
                return Err(CepError::new(
                    CepErrorCode::SchemaMismatch,
                    "WritePath directory prefix must not be empty",
                    CepErrorData::Validation(ValidationErrorData {
                        field: "capabilities.WritePath.path_pattern".into(),
                        reason: "empty directory prefix".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
            return Ok(WritePathPattern::DirectoryGlob(normalize_lexical(base)?));
        }

        // Normalize lexical path (resolve ".", reject "..")
        let normalized = normalize_lexical(pattern)?;

        // Determine if this looks like a directory or a file.
        // Simple single-component names without extensions -> DirectoryGlob.
        // Otherwise -> ExactFile.
        let path = Path::new(pattern);
        let is_single_component = !pattern.contains('/');
        let has_no_extension = path.extension().is_none();
        if is_single_component && has_no_extension {
            // Simple directory name like "src" -> DirectoryGlob
            Ok(WritePathPattern::DirectoryGlob(normalized))
        } else {
            Ok(WritePathPattern::ExactFile(normalized))
        }
    }
}

/// Normalize a path lexically: resolve `.` components, reject `..` and NUL.
fn normalize_lexical(path: &str) -> Result<PathBuf, CepError> {
    let mut result = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {} // skip .
            Component::ParentDir => {
                return Err(CepError::new(
                    CepErrorCode::PathContainment,
                    format!("path contains '..' traversal: '{path}'"),
                    CepErrorData::Validation(ValidationErrorData {
                        field: "path".into(),
                        reason: "contains '..'".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(CepError::new(
                    CepErrorCode::PathContainment,
                    format!("path must be relative: '{path}'"),
                    CepErrorData::Validation(ValidationErrorData {
                        field: "path".into(),
                        reason: "absolute path not allowed".into(),
                    }),
                    Retriability::Permanent,
                ));
            }
        }
    }
    Ok(result)
}

/// Extract all WritePath path_patterns from a capability set.
pub fn extract_write_path_patterns(capabilities: &BTreeSet<Capability>) -> Vec<String> {
    capabilities
        .iter()
        .filter_map(|cap| match cap {
            Capability::WritePath { path_pattern } => Some(path_pattern.clone()),
            _ => None,
        })
        .collect()
}

/// Check if any RunCommand capability's argv_allowlist matches the given argv (prefix match).
pub fn has_run_command_capability(capabilities: &BTreeSet<Capability>, argv: &[String]) -> bool {
    for cap in capabilities {
        if let Capability::RunCommand { argv_allowlist } = cap {
            for entry in argv_allowlist {
                let tokens: Vec<&str> = entry.split_whitespace().collect();
                if argv.len() >= tokens.len()
                    && argv
                        .iter()
                        .zip(tokens.iter())
                        .all(|(a, t)| a.as_str() == *t)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Check whether a target path is within the WritePath scope of the active envelope.
///
/// Returns `Ok(())` if the target matches any WritePath pattern.
/// Returns `Err(ReplanRequired)` if target is inside project root but outside scope.
/// Returns `Err(GuardDenied)` if target escapes project root or symlink escape detected.
///
/// All paths are resolved relative to `project_root`.
///
/// D-36: For new file creation, walks up ancestors until finding an existing directory,
/// canonicalizes it, and checks containment.
pub fn check_write_scope(
    target_path: &str,
    project_root: &str,
    write_path_patterns: &[String],
) -> Result<(), CepError> {
    // Reject NUL in target
    if target_path.contains('\0') {
        return Err(CepError::new(
            CepErrorCode::PathContainment,
            "target path contains NUL byte",
            CepErrorData::Guard(GuardErrorData {
                method: "guard.l1".into(),
                reason: "target path contains NUL byte".into(),
            }),
            Retriability::Permanent,
        ));
    }

    // Reject absolute target paths
    if Path::new(target_path).is_absolute() {
        return Err(guard_denied("target path must be project-root-relative"));
    }

    // Check for '..' in target — this is a GuardDenied (escape attempt), not validation
    if target_path.contains("..") {
        return Err(guard_denied(&format!(
            "target path contains '..' traversal: '{target_path}'"
        )));
    }

    // Lexically normalize target
    let target_normalized = normalize_lexical(target_path)?;

    // Canonicalize project root
    let project_root_canonical = std::fs::canonicalize(project_root).map_err(|e| {
        guard_denied(&format!(
            "cannot canonicalize project root '{}': {e}",
            project_root
        ))
    })?;

    // Resolve target as project_root / target_normalized
    let target_resolved = project_root_canonical.join(&target_normalized);

    // Case A or B: canonicalize target
    let target_canonical = canonicalize_target(&target_resolved, &project_root_canonical)?;

    // Check target is inside project root
    if target_canonical != project_root_canonical
        && !target_canonical.starts_with(&project_root_canonical)
    {
        return Err(guard_denied(&format!(
            "target '{}' escapes project root '{}'",
            target_canonical.display(),
            project_root_canonical.display()
        )));
    }

    // Check each WritePath pattern
    for pattern_str in write_path_patterns {
        let pattern = normalize_write_path_pattern(pattern_str)?;
        match pattern {
            WritePathPattern::ExactFile(pattern_path) => {
                let pattern_resolved = project_root_canonical.join(&pattern_path);
                let pattern_canonical =
                    canonicalize_pattern_path(&pattern_resolved, &project_root_canonical)?;
                if target_canonical == pattern_canonical {
                    return Ok(());
                }
            }
            WritePathPattern::DirectoryGlob(dir_path) => {
                let dir_resolved = project_root_canonical.join(&dir_path);
                let dir_canonical =
                    canonicalize_pattern_dir(&dir_resolved, &project_root_canonical)?;
                if target_canonical.starts_with(&dir_canonical) {
                    return Ok(());
                }
            }
        }
    }

    // No pattern matched — target is inside project root but outside scope
    Err(replan_required(target_path, write_path_patterns))
}

/// Canonicalize a target path, handling both existing and non-existing files.
///
/// Case A: file exists -> canonicalize directly, check for symlinks.
/// Case B: file doesn't exist -> walk up to find existing ancestor, canonicalize that,
///         then construct canonical path from ancestor + remaining relative path.
fn canonicalize_target(target: &Path, project_root_canonical: &Path) -> Result<PathBuf, CepError> {
    // Try direct canonicalization (Case A: file exists)
    if let Ok(canonical) = std::fs::canonicalize(target) {
        // Check for symlink at target itself
        if let Ok(meta) = std::fs::symlink_metadata(target) {
            if meta.file_type().is_symlink() {
                return Err(permission_denied(&format!(
                    "target '{}' is a symlink",
                    target.display()
                )));
            }
        }
        return Ok(canonical);
    }

    // Case B: file doesn't exist — walk up ancestors
    let mut current = target.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&current) {
            // Found an existing ancestor
            // Check if this ancestor is inside project root
            if canonical != *project_root_canonical
                && !canonical.starts_with(project_root_canonical)
            {
                return Err(guard_denied(&format!(
                    "ancestor '{}' of target '{}' escapes project root",
                    canonical.display(),
                    target.display()
                )));
            }
            // Construct canonical target from canonical ancestor + remaining path
            if let Ok(relative) = target.strip_prefix(&current) {
                return Ok(canonical.join(relative));
            } else {
                return Ok(canonical);
            }
        }
        // Try parent
        if !current.pop() {
            return Err(guard_denied(&format!(
                "cannot resolve any ancestor of target '{}'",
                target.display()
            )));
        }
    }
}

/// Canonicalize a pattern path for exact file matching.
fn canonicalize_pattern_path(
    pattern: &Path,
    project_root_canonical: &Path,
) -> Result<PathBuf, CepError> {
    // If file exists, canonicalize it
    if let Ok(canonical) = std::fs::canonicalize(pattern) {
        return Ok(canonical);
    }
    // File doesn't exist — canonicalize parent + filename
    if let Some(parent) = pattern.parent() {
        if parent.as_os_str().is_empty() {
            return Ok(project_root_canonical.join(pattern));
        }
        if let Ok(canonical_parent) = std::fs::canonicalize(parent) {
            if let Some(filename) = pattern.file_name() {
                return Ok(canonical_parent.join(filename));
            }
        }
    }
    Ok(project_root_canonical.join(
        pattern
            .strip_prefix(project_root_canonical)
            .unwrap_or(pattern),
    ))
}

/// Canonicalize a pattern directory for glob matching.
fn canonicalize_pattern_dir(
    dir: &Path,
    _project_root_canonical: &Path,
) -> Result<PathBuf, CepError> {
    if let Ok(canonical) = std::fs::canonicalize(dir) {
        // Check for symlink in pattern base
        if let Ok(meta) = std::fs::symlink_metadata(dir) {
            if meta.file_type().is_symlink() {
                return Err(permission_denied(&format!(
                    "WritePath pattern base '{}' is a symlink pointing outside root",
                    dir.display()
                )));
            }
        }
        return Ok(canonical);
    }
    // Directory doesn't exist yet — use resolved path
    Ok(dir.to_path_buf())
}

fn guard_denied(reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        reason,
        CepErrorData::Guard(GuardErrorData {
            method: "guard.l1".into(),
            reason: reason.to_string(),
        }),
        Retriability::Permanent,
    )
}

fn permission_denied(reason: &str) -> CepError {
    CepError::new(
        CepErrorCode::GuardDenied,
        reason,
        CepErrorData::Guard(GuardErrorData {
            method: "guard.l1".into(),
            reason: reason.to_string(),
        }),
        Retriability::Permanent,
    )
}

fn replan_required(target: &str, patterns: &[String]) -> CepError {
    CepError::new(
        CepErrorCode::ReplanRequired,
        format!(
            "target '{}' is not within active WritePath scope: {:?}",
            target, patterns
        ),
        CepErrorData::Guard(GuardErrorData {
            method: "guard.l1".into(),
            reason: format!(
                "ReplanRequired: target '{}' outside scope {:?}",
                target, patterns
            ),
        }),
        Retriability::RequiresReplan,
    )
}

#[cfg(test)]
mod guard_l1_tests {
    use super::*;
    use tempfile::TempDir;

    // --- normalize_write_path_pattern tests ---

    #[test]
    fn exact_file_pattern() {
        let pattern = normalize_write_path_pattern("src/main.rs").unwrap();
        assert!(matches!(pattern, WritePathPattern::ExactFile(_)));
    }

    #[test]
    fn dir_glob_pattern() {
        let pattern = normalize_write_path_pattern("src/**").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("src"))
        );
    }

    #[test]
    fn simple_prefix_normalized_to_dir_glob() {
        let pattern = normalize_write_path_pattern("src").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("src"))
        );
    }

    #[test]
    fn reject_dot_dot() {
        let err = normalize_write_path_pattern("../etc/passwd").unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[test]
    fn reject_nul() {
        let err = normalize_write_path_pattern("src/\0file").unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[test]
    fn reject_empty() {
        let err = normalize_write_path_pattern("").unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[test]
    fn reject_absolute_pattern() {
        let err = normalize_write_path_pattern("/etc/passwd").unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[test]
    fn nested_dir_glob() {
        let pattern = normalize_write_path_pattern("src/lib/**").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("src/lib"))
        );
    }

    #[test]
    fn trailing_slash_directory_prefix_normalized_to_dir_glob() {
        let pattern = normalize_write_path_pattern("crates/lancet-executor/").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("crates/lancet-executor"))
        );
    }

    #[test]
    fn reject_unsupported_glob_syntax() {
        let err = normalize_write_path_pattern("**/*.rs").unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    #[test]
    fn reject_negation_syntax() {
        let err = normalize_write_path_pattern("!src/**").unwrap_err();
        assert_eq!(err.code, CepErrorCode::SchemaMismatch);
    }

    // --- check_write_scope tests ---

    #[test]
    fn exact_file_match() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let result = check_write_scope("main.rs", root, &["main.rs".into()]);
        assert!(result.is_ok());
    }

    #[test]
    fn dir_glob_matches_child() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src/lib")).unwrap();
        std::fs::write(dir.path().join("src/lib/mod.rs"), "pub mod foo;").unwrap();

        let result = check_write_scope("src/lib/mod.rs", root, &["src/**".into()]);
        assert!(result.is_ok());
    }

    #[test]
    fn simple_prefix_matches_child() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let result = check_write_scope("src/main.rs", root, &["src".into()]);
        assert!(result.is_ok());
    }

    #[test]
    fn target_outside_scope_returns_replan_required() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("README.md"), "# Test").unwrap();

        let err = check_write_scope("README.md", root, &["src/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        assert_eq!(err.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn target_outside_project_root_returns_guard_denied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();

        let err = check_write_scope("../outside.txt", root, &["src/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn pattern_with_dot_dot_rejected() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();

        let err = check_write_scope("src/main.rs", root, &["../etc/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[test]
    fn nul_in_target_rejected() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();

        let err = check_write_scope("src/\0file", root, &["src/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_returns_guard_denied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("outside.txt");
        std::fs::write(&outside_file, "outside").unwrap();

        // Create a symlink inside project root pointing outside
        std::os::unix::fs::symlink(&outside_file, dir.path().join("link.txt")).unwrap();

        let err = check_write_scope("link.txt", root, &["link.txt".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn multiple_patterns_first_match_wins() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let patterns = vec!["tests/**".into(), "src/**".into()];
        let result = check_write_scope("src/main.rs", root, &patterns);
        assert!(result.is_ok());
    }

    #[test]
    fn no_patterns_returns_replan_required() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("file.txt"), "data").unwrap();

        let err = check_write_scope("file.txt", root, &[]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
    }

    #[test]
    fn new_file_in_scope_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();

        let result = check_write_scope("src/new_file.rs", root, &["src/**".into()]);
        assert!(result.is_ok());
    }

    // --- extract_write_path_patterns tests ---

    #[test]
    fn extracts_write_path_patterns() {
        let mut caps = BTreeSet::new();
        caps.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });
        caps.insert(Capability::ReadPath {
            path_pattern: ".".into(),
        });
        caps.insert(Capability::WritePath {
            path_pattern: "tests/**".into(),
        });

        let patterns = extract_write_path_patterns(&caps);
        assert_eq!(patterns.len(), 2);
        assert!(patterns.contains(&"src/**".to_string()));
        assert!(patterns.contains(&"tests/**".to_string()));
    }

    // --- has_run_command_capability tests ---

    #[test]
    fn has_run_command_prefix_match() {
        let mut caps = BTreeSet::new();
        caps.insert(Capability::RunCommand {
            argv_allowlist: vec!["cargo test".into(), "cargo check".into()],
        });

        assert!(has_run_command_capability(
            &caps,
            &["cargo".into(), "test".into(), "--nocapture".into()]
        ));
        assert!(has_run_command_capability(
            &caps,
            &["cargo".into(), "check".into()]
        ));
        assert!(!has_run_command_capability(
            &caps,
            &["cargo".into(), "fmt".into()]
        ));
    }

    #[test]
    fn no_run_command_capability() {
        let mut caps = BTreeSet::new();
        caps.insert(Capability::WritePath {
            path_pattern: "src/**".into(),
        });

        assert!(!has_run_command_capability(
            &caps,
            &["cargo".into(), "test".into()]
        ));
    }
}
