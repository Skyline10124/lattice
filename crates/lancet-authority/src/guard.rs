// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Guard L1 path scope enforcement for workspace mutations.
//!
//! Guard L1 checks whether a target path falls within the authorized
//! write-path scope. It distinguishes between:
//! - `ReplanRequired`: target is inside project root but outside active scope
//! - `GuardDenied`: target escapes project root or violates hard security boundary

use crate::policy::default::{DefaultPolicyHook, PolicyEvidence};
use lancet_protocol::error::{
    CepErrorData, GuardErrorData, PermissionErrorData, ReservedErrorData, ValidationErrorData,
};
use lancet_protocol::policy::GuardVerdict;
use lancet_protocol::policy::{PolicyDecision, PolicyHook, PolicyInput, PolicyReasonCode};
use lancet_protocol::{CepError, CepErrorCode, Retriability};
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
            "WritePath pattern negation is not supported in CEP",
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

/// Pure lexical coverage check: does `pattern` cover `relative_path` under the
/// same `ExactFile`/`DirectoryGlob` classification used by [`check_write_scope`],
/// but without any filesystem access (no canonicalization, no symlink checks)?
///
/// Both inputs must be project-root-relative. This is the single source of
/// truth for WritePath scope semantics shared by the capability matcher
/// (pure, pre-filesystem) and Guard L1 (which additionally canonicalizes for
/// symlink-escape safety). Keeping both layers on this function prevents the
/// classification drift where one layer treats `crates/foo` as a directory
/// prefix while the other treats it as an exact file.
///
/// Returns `Err` for malformed inputs (`..`, NUL, absolute, bad glob syntax),
/// matching the rejection these would also receive at envelope-declare time.
pub fn write_pattern_covers_relative(pattern: &str, relative_path: &str) -> Result<bool, CepError> {
    if relative_path.contains('\0') {
        return Err(guard_denied("target path contains NUL byte"));
    }
    if relative_path.contains("..") {
        return Err(guard_denied(&format!(
            "target path contains '..' traversal: '{relative_path}'"
        )));
    }
    if Path::new(relative_path).is_absolute() {
        return Err(guard_denied("target path must be project-root-relative"));
    }
    let target = normalize_lexical(relative_path)?;
    Ok(match normalize_write_path_pattern(pattern)? {
        WritePathPattern::ExactFile(expected) => target == expected,
        // The "." sentinel means the entire workspace — it covers every path.
        WritePathPattern::DirectoryGlob(ref dir) if dir.as_os_str() == "." => true,
        WritePathPattern::DirectoryGlob(dir) => target == dir || target.starts_with(&dir),
    })
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
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    Ok(result)
}

/// Check whether a target path is denied by the write-scope blacklist.
///
/// Returns `Ok(())` if the target does NOT match any denied write pattern.
/// Returns `Err(ReplanRequired)` if target matches a denied pattern (agent should try another path).
/// Returns `Err(GuardDenied)` if target escapes project root or symlink escape detected.
///
/// All paths are resolved relative to `project_root`.
///
/// For new file creation, walks up ancestors until finding an existing directory,
/// canonicalizes that directory, and checks containment.
pub fn check_write_scope(
    target_path: &str,
    project_root: &str,
    denied_write_patterns: &[String],
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

    // Check each denied write pattern — if any matches, the path is denied
    for pattern_str in denied_write_patterns {
        let pattern = normalize_write_path_pattern(pattern_str)?;
        match pattern {
            WritePathPattern::ExactFile(pattern_path) => {
                let pattern_resolved = project_root_canonical.join(&pattern_path);
                let pattern_canonical =
                    canonicalize_pattern_path(&pattern_resolved, &project_root_canonical)?;
                if target_canonical == pattern_canonical {
                    return Err(replan_required(target_path, denied_write_patterns));
                }
            }
            WritePathPattern::DirectoryGlob(dir_path) => {
                let dir_resolved = project_root_canonical.join(&dir_path);
                let dir_canonical =
                    canonicalize_pattern_dir(&dir_resolved, &project_root_canonical)?;
                if target_canonical.starts_with(&dir_canonical) {
                    return Err(replan_required(target_path, denied_write_patterns));
                }
            }
        }
    }

    // No denied pattern matched — target is allowed
    Ok(())
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
        if let Ok(meta) = std::fs::symlink_metadata(target)
            && meta.file_type().is_symlink()
        {
            return Err(permission_denied(&format!(
                "target '{}' is a symlink",
                target.display()
            )));
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
        if let Ok(canonical_parent) = std::fs::canonicalize(parent)
            && let Some(filename) = pattern.file_name()
        {
            return Ok(canonical_parent.join(filename));
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
        if let Ok(meta) = std::fs::symlink_metadata(dir)
            && meta.file_type().is_symlink()
        {
            return Err(permission_denied(&format!(
                "WritePath pattern base '{}' is a symlink pointing outside root",
                dir.display()
            )));
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
            "target '{target}' is not within the allowed WritePath scope: {patterns:?}. \
             A WritePath pattern matches a directory only with a trailing '/**' \
             (e.g. 'crates/foo/**'); a bare 'crates/foo' matches that exact path \
             only. Use a pattern that covers '{target}'."
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

/// Adapter-reported filesystem facts validated against the write-deny blacklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathFacts {
    pub normalized_path: String,
    pub exists: bool,
    pub kind: Option<PathKind>,
    pub parent_exists: bool,
    pub parent_kind: Option<PathKind>,
    pub canonical_under_workspace: bool,
}

/// Kind of filesystem object observed by the execution adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathKind {
    File,
    Directory,
    Symlink,
}

/// Validate adapter-reported path facts against a policy-produced authorized region.
///
/// Policy already decided whether the write is authorized; this function only
/// checks that returned facts match the request and are not in the denied region.
/// No system-level denies are hardcoded — the deny list is entirely user-controlled.
pub fn validate_path_facts(
    path: &str,
    facts: &PathFacts,
    authorized_region: &crate::effect::region::AuthorizedEffectRegion,
) -> Result<(), CepError> {
    if path.contains('\0') || path.contains("..") || Path::new(path).is_absolute() {
        return Err(permission_denied("path is malformed"));
    }

    let normalized = normalize_lexical(path)?;
    let request = normalized.to_string_lossy();
    if facts.normalized_path != request {
        return Err(permission_denied("path facts do not match requested path"));
    }

    if !facts.canonical_under_workspace {
        return Err(permission_denied("path escapes workspace containment"));
    }

    // Blacklist semantics: deny if the path matches any denied pattern.
    // No hardcoded system-level denies — the deny list is user-controlled.
    let normalized_path = Path::new(&facts.normalized_path);
    let denied = authorized_region
        .denied_write_paths
        .iter()
        .any(|p| p.as_os_str() == facts.normalized_path.as_str())
        || authorized_region
            .denied_write_path_prefixes
            .iter()
            .any(|prefix| {
                crate::effect::region::relative_path_within_prefix(normalized_path, prefix)
            });
    if denied {
        return Err(permission_denied("path matches a denied write pattern"));
    }

    let _ = facts.exists;
    let _ = facts.kind;
    let _ = facts.parent_exists;
    let _ = facts.parent_kind;
    Ok(())
}

/// Result of evaluating a policy hook.
pub struct PolicyEvaluation {
    pub decision: PolicyDecision,
    pub evidence: Option<PolicyEvidence>,
}

/// Evaluate the policy hook, returning a stable decision and optional evidence.
///
/// If the default hook is installed, calls `decide_with_evidence` to capture
/// rich executor-owned evidence for audit. Otherwise calls the public
/// `PolicyHook::decide` and returns `evidence: None`.
pub fn evaluate_policy(
    hook: &dyn PolicyHook,
    default_hook: Option<&DefaultPolicyHook>,
    input: &PolicyInput,
) -> Result<PolicyEvaluation, CepError> {
    if let Some(default) = default_hook {
        let outcome = default.decide_with_evidence(input);
        Ok(PolicyEvaluation {
            decision: outcome.decision,
            evidence: Some(outcome.evidence),
        })
    } else {
        let decision = hook.decide(input);
        Ok(PolicyEvaluation {
            decision,
            evidence: None,
        })
    }
}

/// Map a `PolicyDecision` to a CEP error if denied/human-review, or `Ok(())` if allowed.
///
/// Every `PolicyReasonCode` maps to an existing `CepErrorCode`. This is the single
/// mapping path: PolicyDecision → CEP typed error → event → audit.
pub fn map_decision_to_cep_error(decision: &PolicyDecision, method: &str) -> Result<(), CepError> {
    match decision {
        PolicyDecision::Allowed => Ok(()),
        PolicyDecision::Denied {
            reason_code,
            message,
            action_required: _,
        } => {
            let (code, data, retriability) = match reason_code {
                PolicyReasonCode::MissingCapability => (
                    CepErrorCode::CapabilityNotGranted,
                    CepErrorData::Permission(PermissionErrorData {
                        required_capability: method.into(),
                        active_envelope_id: None,
                        granted_capabilities: vec![],
                    }),
                    Retriability::Permanent,
                ),
                PolicyReasonCode::CapabilitySourceNotAllowed => (
                    CepErrorCode::CapabilityNotGranted,
                    CepErrorData::Permission(PermissionErrorData {
                        required_capability: method.into(),
                        active_envelope_id: None,
                        granted_capabilities: vec![],
                    }),
                    Retriability::Permanent,
                ),
                PolicyReasonCode::PathOutsideScope | PolicyReasonCode::ReplanRequired => (
                    CepErrorCode::ReplanRequired,
                    CepErrorData::Guard(GuardErrorData {
                        method: method.into(),
                        reason: message.clone(),
                    }),
                    Retriability::RequiresReplan,
                ),
                PolicyReasonCode::NetworkDenied => (
                    CepErrorCode::GuardDenied,
                    CepErrorData::Guard(GuardErrorData {
                        method: method.into(),
                        reason: message.clone(),
                    }),
                    Retriability::Permanent,
                ),
                PolicyReasonCode::BudgetExceeded => (
                    CepErrorCode::Reserved,
                    CepErrorData::Reserved(ReservedErrorData {
                        code: "budget_exceeded".into(),
                        detail: message.clone(),
                    }),
                    Retriability::RequiresReplan,
                ),
                PolicyReasonCode::PolicyConfigInvalid => (
                    CepErrorCode::Reserved,
                    CepErrorData::Reserved(ReservedErrorData {
                        code: "policy_config_invalid".into(),
                        detail: message.clone(),
                    }),
                    Retriability::Permanent,
                ),
                PolicyReasonCode::NonDeterminable => (
                    CepErrorCode::GuardDenied,
                    CepErrorData::Guard(GuardErrorData {
                        method: method.into(),
                        reason: message.clone(),
                    }),
                    Retriability::Unknown,
                ),
            };
            Err(CepError::new(code, message.clone(), data, retriability))
        }
        PolicyDecision::HumanReview {
            reason_code: _,
            message,
        } => Err(CepError::new(
            CepErrorCode::GuardDenied,
            message.clone(),
            CepErrorData::Guard(GuardErrorData {
                method: method.into(),
                reason: message.clone(),
            }),
            Retriability::Unknown,
        )),
    }
}

/// Map a `PolicyDecision` to a `GuardVerdict` for event compatibility.
///
/// This is a projection for event emission only — `PolicyDecision` and
/// `GuardVerdict` remain distinct types.
pub fn decision_to_guard_verdict(decision: &PolicyDecision) -> GuardVerdict {
    match decision {
        PolicyDecision::Allowed => GuardVerdict::Allowed,
        PolicyDecision::Denied { message, .. } => GuardVerdict::Denied {
            reason: message.clone(),
        },
        PolicyDecision::HumanReview { message, .. } => GuardVerdict::HumanReview {
            reason: message.clone(),
        },
    }
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

    // --- write_pattern_covers_relative: shared lexical semantics ---

    #[test]
    fn covers_directory_glob_children_and_self() {
        assert!(write_pattern_covers_relative("crates/foo/**", "crates/foo/src/x.rs").unwrap());
        assert!(write_pattern_covers_relative("crates/foo/**", "crates/foo").unwrap());
        // Component-wise, not string prefix: a sibling sharing a name prefix
        // must not be covered.
        assert!(!write_pattern_covers_relative("crates/foo/**", "crates/foobar/x.rs").unwrap());
    }

    #[test]
    fn covers_single_component_directory() {
        assert!(write_pattern_covers_relative("src", "src/main.rs").unwrap());
        assert!(write_pattern_covers_relative(".", "anything/at/all.rs").unwrap());
    }

    #[test]
    fn multi_component_no_extension_is_exact_file_not_directory() {
        // Regression: `crates/lattice-agent` (a `/`-containing, extensionless
        // pattern) is classified ExactFile. It must NOT cover child paths —
        // this is exactly where the capability layer and Guard L1 used to
        // disagree, producing declare-pass / use-time-out-of-scope churn.
        assert!(
            write_pattern_covers_relative("crates/lattice-agent", "crates/lattice-agent").unwrap()
        );
        assert!(
            !write_pattern_covers_relative("crates/lattice-agent", "crates/lattice-agent/src/x.rs")
                .unwrap()
        );
    }

    #[test]
    fn covers_rejects_malformed_inputs() {
        assert!(write_pattern_covers_relative("src/**", "../escape").is_err());
        assert!(write_pattern_covers_relative("src/**", "/abs/path").is_err());
        assert!(write_pattern_covers_relative("src/*.rs", "src/a.rs").is_err());
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
        let pattern = normalize_write_path_pattern("crates/lancet-native/").unwrap();
        assert_eq!(
            pattern,
            WritePathPattern::DirectoryGlob(PathBuf::from("crates/lancet-native"))
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

        // No denied patterns → allowed
        let result = check_write_scope("main.rs", root, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn dir_glob_denies_child() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src/lib")).unwrap();
        std::fs::write(dir.path().join("src/lib/mod.rs"), "pub mod foo;").unwrap();

        // src/** is denied → src/lib/mod.rs is denied
        let err = check_write_scope("src/lib/mod.rs", root, &["src/**".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
    }

    #[test]
    fn simple_prefix_denies_child() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // src is denied → src/main.rs is denied
        let err = check_write_scope("src/main.rs", root, &["src".into()]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
    }

    #[test]
    fn target_not_denied_is_allowed() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("README.md"), "# Test").unwrap();

        // README.md is not in the deny list → allowed
        let result = check_write_scope("README.md", root, &["src/**".into()]);
        assert!(result.is_ok());
    }

    #[test]
    fn target_outside_project_root_returns_guard_denied() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();

        let err = check_write_scope("../outside.txt", root, &[]).unwrap_err();
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

        let err = check_write_scope("src/\0file", root, &[]).unwrap_err();
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

        let err = check_write_scope("link.txt", root, &[]).unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn multiple_patterns_first_deny_wins() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        // src/** is denied → src/main.rs is denied even if tests/** is also denied
        let patterns = vec!["tests/**".into(), "src/**".into()];
        let err = check_write_scope("src/main.rs", root, &patterns).unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
    }

    #[test]
    fn no_patterns_allows_all() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::write(dir.path().join("file.txt"), "data").unwrap();

        // No denied patterns → all paths allowed
        let result = check_write_scope("file.txt", root, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn new_file_not_denied_succeeds() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();

        // src/new_file.rs is not denied → allowed
        let result = check_write_scope("src/new_file.rs", root, &[]);
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::policy::default::PolicyConfig;
    use lancet_protocol::policy::{PolicyAuditContext, PolicyAuthorityInput, PolicyBudgetSnapshot};

    #[test]
    fn evaluate_with_default_hook_returns_evidence() {
        let hook = DefaultPolicyHook::new(PolicyConfig::defaults_for_project_root(
            std::path::PathBuf::from("."),
        ));
        let input = PolicyInput {
            method: "observe.workspace".into(),
            params: serde_json::json!({"mode": "read_file", "path": "src/lib.rs"}),
            authority: PolicyAuthorityInput,
            audit_context: PolicyAuditContext {
                turn_id: "t".into(),
                session_id: "s".into(),
                call_id: Some(1),
                budget_snapshot: PolicyBudgetSnapshot {
                    turn_observation_bytes_used: 0,
                    turn_observation_bytes_limit: 65536,
                    todo_ops_used: 0,
                    todo_ops_limit: 10,
                },
            },
        };
        let eval = evaluate_policy(&hook, Some(&hook), &input).unwrap();
        assert!(
            eval.evidence.is_some(),
            "default hook should return evidence"
        );
    }

    #[test]
    fn evaluate_with_external_hook_returns_no_evidence() {
        struct AlwaysAllowed;
        impl PolicyHook for AlwaysAllowed {
            fn decide(&self, _input: &PolicyInput) -> PolicyDecision {
                PolicyDecision::Allowed
            }
        }
        let hook = AlwaysAllowed;
        let input = PolicyInput {
            method: "test".into(),
            params: serde_json::json!({}),
            authority: PolicyAuthorityInput,
            audit_context: PolicyAuditContext {
                turn_id: "t".into(),
                session_id: "s".into(),
                call_id: None,
                budget_snapshot: PolicyBudgetSnapshot {
                    turn_observation_bytes_used: 0,
                    turn_observation_bytes_limit: 0,
                    todo_ops_used: 0,
                    todo_ops_limit: 0,
                },
            },
        };
        let eval = evaluate_policy(&hook, None, &input).unwrap();
        assert!(
            eval.evidence.is_none(),
            "external hook should return no evidence"
        );
        assert_eq!(eval.decision, PolicyDecision::Allowed);
    }

    #[test]
    fn map_allowed_returns_ok() {
        assert!(map_decision_to_cep_error(&PolicyDecision::Allowed, "test").is_ok());
    }

    #[test]
    fn map_missing_capability_to_capability_not_granted() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::MissingCapability,
            message: "no cap".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
        assert_eq!(err.retriability, Retriability::Permanent);
    }

    #[test]
    fn map_replan_required_to_replan_required() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::ReplanRequired,
            message: "replan".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        assert_eq!(err.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn map_budget_exceeded_to_reserved_with_replan() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::BudgetExceeded,
            message: "budget".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);
        assert_eq!(err.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn map_non_determinable_to_guard_denied_unknown() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::NonDeterminable,
            message: "unknown".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert_eq!(err.retriability, Retriability::Unknown);
    }

    #[test]
    fn map_human_review_to_guard_denied_unknown() {
        let decision = PolicyDecision::HumanReview {
            reason_code: PolicyReasonCode::NonDeterminable,
            message: "review".into(),
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        assert_eq!(err.retriability, Retriability::Unknown);
    }

    #[test]
    fn map_network_denied_to_guard_denied() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::NetworkDenied,
            message: "blocked".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "fetch.web").unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[test]
    fn map_path_outside_scope_to_replan_required() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::PathOutsideScope,
            message: "outside".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::ReplanRequired);
        assert_eq!(err.retriability, Retriability::RequiresReplan);
    }

    #[test]
    fn map_capability_source_not_allowed_to_capability_not_granted() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::CapabilitySourceNotAllowed,
            message: "source".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::CapabilityNotGranted);
    }

    #[test]
    fn normalize_lexical_dot_returns_dot_sentinel() {
        let result = normalize_lexical(".").unwrap();
        assert_eq!(
            result.to_string_lossy(),
            ".",
            "workspace root must normalize to \".\""
        );
    }

    #[test]
    fn normalize_lexical_dotslash_prefix_returns_tail() {
        let result = normalize_lexical("./src/main.rs").unwrap();
        assert_eq!(result.to_string_lossy(), "src/main.rs");
    }

    #[test]
    fn path_facts_accepts_workspace_root() {
        use crate::effect::region::AuthorizedEffectRegion;
        let region = AuthorizedEffectRegion::empty();
        let facts = PathFacts {
            normalized_path: ".".into(),
            exists: true,
            kind: Some(PathKind::Directory),
            parent_exists: true,
            parent_kind: Some(PathKind::Directory),
            canonical_under_workspace: true,
        };
        assert!(validate_path_facts(".", &facts, &region).is_ok());
    }

    #[test]
    fn path_facts_rejects_mismatched_path() {
        use crate::effect::region::AuthorizedEffectRegion;
        let region = AuthorizedEffectRegion::empty();
        let facts = PathFacts {
            normalized_path: "src/other.rs".into(),
            exists: true,
            kind: Some(PathKind::File),
            parent_exists: true,
            parent_kind: Some(PathKind::Directory),
            canonical_under_workspace: true,
        };
        assert!(validate_path_facts("src/main.rs", &facts, &region).is_err());
    }

    #[test]
    fn path_facts_rejects_non_canonical_containment() {
        use crate::effect::region::AuthorizedEffectRegion;
        let region = AuthorizedEffectRegion::empty();
        let facts = PathFacts {
            normalized_path: "src/main.rs".into(),
            exists: true,
            kind: Some(PathKind::File),
            parent_exists: true,
            parent_kind: Some(PathKind::Directory),
            canonical_under_workspace: false,
        };
        assert!(validate_path_facts("src/main.rs", &facts, &region).is_err());
    }

    #[test]
    fn path_facts_rejects_denied_pattern() {
        use crate::effect::region::AuthorizedEffectRegion;
        use std::path::PathBuf;
        let mut region = AuthorizedEffectRegion::empty();
        region
            .denied_write_path_prefixes
            .push(PathBuf::from(".git"));
        let facts = PathFacts {
            normalized_path: ".git/config".into(),
            exists: true,
            kind: Some(PathKind::File),
            parent_exists: true,
            parent_kind: Some(PathKind::Directory),
            canonical_under_workspace: true,
        };
        assert!(validate_path_facts(".git/config", &facts, &region).is_err());
    }

    #[test]
    fn path_facts_allows_when_no_denied_patterns() {
        use crate::effect::region::AuthorizedEffectRegion;
        let region = AuthorizedEffectRegion::empty();
        let facts = PathFacts {
            normalized_path: "src/main.rs".into(),
            exists: true,
            kind: Some(PathKind::File),
            parent_exists: true,
            parent_kind: Some(PathKind::Directory),
            canonical_under_workspace: true,
        };
        // No denied patterns → all paths allowed
        assert!(validate_path_facts("src/main.rs", &facts, &region).is_ok());
    }

    #[test]
    fn map_policy_config_invalid_to_reserved() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::PolicyConfigInvalid,
            message: "bad config".into(),
            action_required: None,
        };
        let err = map_decision_to_cep_error(&decision, "test").unwrap_err();
        assert_eq!(err.code, CepErrorCode::Reserved);
        assert_eq!(err.retriability, Retriability::Permanent);
    }

    #[test]
    fn verdict_allowed() {
        assert_eq!(
            decision_to_guard_verdict(&PolicyDecision::Allowed),
            GuardVerdict::Allowed
        );
    }

    #[test]
    fn verdict_denied() {
        let decision = PolicyDecision::Denied {
            reason_code: PolicyReasonCode::MissingCapability,
            message: "no cap".into(),
            action_required: None,
        };
        assert_eq!(
            decision_to_guard_verdict(&decision),
            GuardVerdict::Denied {
                reason: "no cap".into()
            }
        );
    }

    #[test]
    fn verdict_human_review() {
        let decision = PolicyDecision::HumanReview {
            reason_code: PolicyReasonCode::NonDeterminable,
            message: "review needed".into(),
        };
        assert_eq!(
            decision_to_guard_verdict(&decision),
            GuardVerdict::HumanReview {
                reason: "review needed".into()
            }
        );
    }
}
