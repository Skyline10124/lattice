//! Pure capability matcher for CEP policy decisions (D-20 to D-27).
//!
//! The matcher evaluates whether a concrete invocation satisfies a
//! [`Capability`] value, returning an explanatory [`CapabilityMatch`] verdict.
//! It is a pure function: no I/O, no mutable state, no time reads.

use std::path::Path;

use lancet_protocol::envelope::Capability;
use lancet_protocol::method::RequiredCapability;
use lancet_protocol::policy::{CapabilitySource, SourcedCapability};

use super::default::CapabilitySourcePolicy;

// ── Verdict types ───────────────────────────────────────────────────────

/// Verdict from the capability matcher (D-23).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityMatchVerdict {
    /// A matching capability was found and the source is allowed for this method.
    Satisfied,
    /// No capability of the required kind exists in the envelope.
    NoMatchingCapability,
    /// A matching capability exists but its source is not allowed for this method.
    SourceNotAllowed,
    /// A matching capability exists and source is allowed, but the invocation
    /// pattern (path/argv/host) does not match.
    PatternRejected,
    /// Ambiguous match (reserved for future multi-capability disambiguation).
    Ambiguous,
}

/// Typed failure reason (D-24).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityFailureReason {
    /// No capability of the required kind in the envelope.
    MissingCapability,
    /// Capability exists but source is not allowed for the method.
    SourceNotAllowed,
    /// Path target is outside the allowed pattern scope.
    PathOutsidePattern,
    /// Path contains `..` traversal.
    PathTraversalRejected,
    /// Path pattern contains unsupported glob escape.
    GlobEscapeRejected,
    /// URL host does not match the allowed host pattern.
    HostNotAllowed,
    /// RunCommand argv does not match the allowlist.
    ArgvNotAllowed,
}

/// Action the agent should take after a capability failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityActionRequired {
    /// Declare an envelope with the required capability.
    DeclareEnvelope,
    /// Widen the current envelope's path/host scope.
    WidenEnvelope,
    /// Change the command argv to match an allowlist entry.
    ChangeCommand,
    /// Change the URL host to match an allowed pattern.
    ChangeUrl,
}

/// Full capability match result (D-22).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityMatch {
    /// The capability kind that was required.
    pub required: RequiredCapability,
    /// The sourced capability that matched, if any.
    pub matched: Option<SourcedCapability>,
    /// Whether the matched source is allowed for the method.
    pub source_allowed: bool,
    /// Verdict summary.
    pub verdict: CapabilityMatchVerdict,
    /// Typed failure reason, if verdict is not `Satisfied`.
    pub failure_reason: Option<CapabilityFailureReason>,
    /// Suggested corrective action, if verdict is not `Satisfied`.
    pub action_required: Option<CapabilityActionRequired>,
}

// ── Session state operation ─────────────────────────────────────────────

/// Identifies the session-state operation being performed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStateOperation {
    /// `session.todo_write` — writing todo items.
    TodoWrite,
    /// `envelope.declare` — declaring a new envelope.
    EnvelopeDeclare,
}

// ── Invocation facts ────────────────────────────────────────────────────

/// Concrete invocation facts for capability matching.
///
/// All fields are present for coverage and audit evidence even if the
/// matcher does not consume every field for every capability kind.
#[derive(Clone, Debug, Default)]
pub struct InvocationFacts {
    /// Target path (for ReadPath/WritePath) — canonical absolute path.
    pub path: Option<String>,
    /// Normalized relative path (for relative pattern matching).
    pub normalized_path: Option<String>,
    /// Command argv (for RunCommand).
    pub argv: Option<Vec<String>>,
    /// Command working directory.
    pub cwd: Option<std::path::PathBuf>,
    /// Environment variable keys supplied in the invocation.
    pub env_keys: Vec<String>,
    /// URL host (for FetchUrl).
    pub host: Option<String>,
    /// All target file paths extracted from a patch document (for workspace.patch).
    pub all_patch_targets: Vec<String>,
    /// Session-state operation kind (for MutateSessionState).
    pub session_state_operation: Option<SessionStateOperation>,
}

// ── Pure matcher ────────────────────────────────────────────────────────

/// Evaluate whether `sourced_caps` grant `required` for a concrete `invocation`.
///
/// Pure function: no I/O, no mutable state, no time reads.
///
/// Logic per D-20 to D-27:
/// 1. Find first `SourcedCapability` whose variant matches `required`.
/// 2. If none: `NoMatchingCapability` + `DeclareEnvelope`.
/// 3. If found but pattern does not cover invocation: `PatternRejected`.
/// 4. If found and pattern matches: check source allowed for `method`.
/// 5. If source not allowed: `SourceNotAllowed`.
/// 6. Otherwise: `Satisfied`.
pub fn match_capability(
    required: RequiredCapability,
    invocation: &InvocationFacts,
    sourced_caps: &[SourcedCapability],
    method: &str,
    source_policy: &CapabilitySourcePolicy,
) -> CapabilityMatch {
    let matching: Vec<&SourcedCapability> = sourced_caps
        .iter()
        .filter(|sc| capability_matches_required(&sc.capability, required))
        .collect();

    if matching.is_empty() {
        return CapabilityMatch {
            required,
            matched: None,
            source_allowed: false,
            verdict: CapabilityMatchVerdict::NoMatchingCapability,
            failure_reason: Some(CapabilityFailureReason::MissingCapability),
            action_required: Some(CapabilityActionRequired::DeclareEnvelope),
        };
    }

    // Check all candidates — first one with satisfied pattern wins (MEDIUM-1 fix)
    for sc in &matching {
        let pattern_result = check_pattern_coverage(&sc.capability, invocation);
        if pattern_result.is_some() {
            continue;
        }
        let source_allowed = is_source_allowed_for_method(&sc.source, method, source_policy);
        if !source_allowed {
            continue;
        }
        return CapabilityMatch {
            required,
            matched: Some((*sc).clone()),
            source_allowed: true,
            verdict: CapabilityMatchVerdict::Satisfied,
            failure_reason: None,
            action_required: None,
        };
    }

    // None passed both pattern + source checks — report the first candidate's failure
    let sc = matching[0];
    let pattern_result = check_pattern_coverage(&sc.capability, invocation);
    if pattern_result.is_some() {
        return CapabilityMatch {
            required,
            matched: Some(sc.clone()),
            source_allowed: false,
            verdict: CapabilityMatchVerdict::PatternRejected,
            failure_reason: pattern_result,
            action_required: Some(pattern_action_for(required)),
        };
    }

    CapabilityMatch {
        required,
        matched: Some(sc.clone()),
        source_allowed: false,
        verdict: CapabilityMatchVerdict::SourceNotAllowed,
        failure_reason: Some(CapabilityFailureReason::SourceNotAllowed),
        action_required: Some(CapabilityActionRequired::DeclareEnvelope),
    }
}

// ── Source rules (D-13 to D-19) ─────────────────────────────────────────

/// Check whether `source` is allowed for the given `method`.
///
/// Source rules per D-13 to D-19:
/// - Observation methods (workspace.read_file, workspace.grep, workspace.list_directory,
///   workspace.find_files) accept Base, Declared, or Both for their catalog-required capability.
/// - External observation (web.fetch) accepts Base, Declared, or Both.
/// - Session state (session.todo_write, envelope.declare) accepts Base, Declared, or Both.
/// - Workspace mutation (workspace.write_file, workspace.patch) requires Declared or Both.
/// - command.run requires Declared or Both.
fn is_source_allowed_for_method(
    source: &CapabilitySource,
    method: &str,
    source_policy: &CapabilitySourcePolicy,
) -> bool {
    use super::default::CapabilitySourceClass;

    let allowed_classes = match method {
        "workspace.read_file"
        | "workspace.grep"
        | "workspace.list_directory"
        | "workspace.find_files" => &source_policy.observation,
        "web.fetch" => &source_policy.external,
        "session.todo_write" | "envelope.declare" => &source_policy.session_state,
        "workspace.write_file" | "workspace.patch" => &source_policy.mutation,
        "command.run" => &source_policy.command,
        _ => return false,
    };

    match source {
        CapabilitySource::Base => allowed_classes.contains(&CapabilitySourceClass::Base),
        CapabilitySource::Declared { .. } => {
            allowed_classes.contains(&CapabilitySourceClass::Declared)
        }
        CapabilitySource::Both { .. } => {
            allowed_classes.contains(&CapabilitySourceClass::Base)
                || allowed_classes.contains(&CapabilitySourceClass::Declared)
        }
    }
}

// ── Capability kind matching ────────────────────────────────────────────

/// Check if a `Capability` variant matches the `RequiredCapability` kind.
fn capability_matches_required(cap: &Capability, required: RequiredCapability) -> bool {
    matches!(
        (cap, required),
        (Capability::ReadPath { .. }, RequiredCapability::ReadPath)
            | (Capability::WritePath { .. }, RequiredCapability::WritePath)
            | (
                Capability::RunCommand { .. },
                RequiredCapability::RunCommand
            )
            | (Capability::FetchUrl { .. }, RequiredCapability::FetchUrl)
            | (
                Capability::MutateSessionState,
                RequiredCapability::MutateSessionState
            )
            | (Capability::Inspect, RequiredCapability::Inspect)
    )
}

// ── Pattern coverage checks ─────────────────────────────────────────────

/// Check if the invocation facts are covered by the capability pattern.
///
/// Returns `Some(failure_reason)` if NOT covered, `None` if covered.
fn check_pattern_coverage(
    cap: &Capability,
    invocation: &InvocationFacts,
) -> Option<CapabilityFailureReason> {
    match cap {
        Capability::ReadPath { path_pattern } => {
            if let Some(path) = &invocation.path {
                let match_path = if std::path::Path::new(path_pattern).is_absolute() {
                    path.as_str()
                } else {
                    invocation
                        .normalized_path
                        .as_deref()
                        .unwrap_or(path.as_str())
                };
                check_read_path_pattern(path_pattern, match_path)
            } else {
                None
            }
        }
        Capability::WritePath { path_pattern } => {
            if !invocation.all_patch_targets.is_empty() {
                for target in &invocation.all_patch_targets {
                    if check_write_path_pattern(path_pattern, target).is_some() {
                        return Some(CapabilityFailureReason::PathOutsidePattern);
                    }
                }
                None
            } else if let Some(path) = &invocation.path {
                let match_path = if std::path::Path::new(path_pattern).is_absolute() {
                    path.as_str()
                } else {
                    invocation
                        .normalized_path
                        .as_deref()
                        .unwrap_or(path.as_str())
                };
                check_write_path_pattern(path_pattern, match_path)
            } else {
                Some(CapabilityFailureReason::PathOutsidePattern)
            }
        }
        Capability::RunCommand { argv_allowlist } => {
            if let Some(argv) = &invocation.argv {
                if !matches_argv_allowlist(argv_allowlist, argv) {
                    return Some(CapabilityFailureReason::ArgvNotAllowed);
                }
            }
            None
        }
        Capability::FetchUrl { host_pattern } => {
            if let Some(host) = &invocation.host {
                if !matches_host_pattern(host_pattern, host) {
                    return Some(CapabilityFailureReason::HostNotAllowed);
                }
            }
            None
        }
        Capability::MutateSessionState => {
            // MutateSessionState covers all session-state operations
            None
        }
        Capability::Inspect => {
            // Inspect covers all inspection operations
            None
        }
    }
}

/// Check a read path against a ReadPath pattern.
///
/// ReadPath patterns use the path_pattern as a project-root-relative prefix
/// or exact match. Rejects `..` traversal and NUL bytes.
fn check_read_path_pattern(pattern: &str, path: &str) -> Option<CapabilityFailureReason> {
    // Reject NUL in path
    if path.contains('\0') {
        return Some(CapabilityFailureReason::PathTraversalRejected);
    }

    // Reject .. in path
    if path.contains("..") {
        return Some(CapabilityFailureReason::PathTraversalRejected);
    }

    // Reject .. in pattern
    if pattern.contains("..") {
        return Some(CapabilityFailureReason::PathTraversalRejected);
    }

    // Reject unsupported glob escapes in pattern
    if has_unsupported_glob_escape(pattern) {
        return Some(CapabilityFailureReason::GlobEscapeRejected);
    }

    // Check path coverage:
    // - Exact match
    // - Pattern is a prefix directory (path starts with pattern/)
    // - Pattern ends with /** (directory glob)
    if path == pattern {
        return None;
    }

    if pattern == "." {
        return None;
    }

    // Normalize absolute pattern: if both pattern and path are absolute,
    // check if the path starts with the pattern (directory containment).
    if Path::new(pattern).is_absolute() && Path::new(path).is_absolute() {
        if path == pattern {
            return None;
        }
        if let Some(remainder) = path.strip_prefix(pattern) {
            if remainder.is_empty() || remainder.starts_with('/') {
                return None;
            }
        }
        return Some(CapabilityFailureReason::PathOutsidePattern);
    }

    if Path::new(pattern).is_absolute() && !Path::new(path).is_absolute() {
        if let Some(last_component) = Path::new(pattern).file_name() {
            let prefix = last_component.to_string_lossy();
            if path.starts_with(prefix.as_ref()) {
                let remainder = &path[prefix.len()..];
                if remainder.is_empty() || remainder.starts_with('/') {
                    return None;
                }
            }
        }
        return Some(CapabilityFailureReason::PathOutsidePattern);
    }

    if let Some(base) = pattern.strip_suffix("/**") {
        if base.is_empty() {
            return None;
        }
        if path == base {
            return None;
        }
        let prefix = if base.ends_with('/') {
            base.to_string()
        } else {
            format!("{}/", base)
        };
        if path.starts_with(&prefix) {
            return None;
        }
        return Some(CapabilityFailureReason::PathOutsidePattern);
    }

    if let Some(remainder) = path.strip_prefix(pattern) {
        if remainder.is_empty() || remainder.starts_with('/') {
            return None;
        }
    }

    Some(CapabilityFailureReason::PathOutsidePattern)
}

/// Check a write path against a WritePath pattern.
/// as read paths but also validates pattern syntax (no absolute, no ..).
fn check_write_path_pattern(pattern: &str, path: &str) -> Option<CapabilityFailureReason> {
    if path.contains('\0') {
        return Some(CapabilityFailureReason::PathTraversalRejected);
    }

    if path.contains("..") {
        return Some(CapabilityFailureReason::PathTraversalRejected);
    }

    if pattern.contains("..") {
        return Some(CapabilityFailureReason::PathTraversalRejected);
    }

    if Path::new(pattern).is_absolute() {
        return Some(CapabilityFailureReason::PathTraversalRejected);
    }

    if path == pattern {
        return None;
    }

    if pattern == "." {
        return None;
    }

    // Normalize absolute pattern: if both pattern and path are absolute,
    // check if the path starts with the pattern (directory containment).
    if Path::new(pattern).is_absolute() && Path::new(path).is_absolute() {
        if path == pattern {
            return None;
        }
        if let Some(remainder) = path.strip_prefix(pattern) {
            if remainder.is_empty() || remainder.starts_with('/') {
                return None;
            }
        }
        return Some(CapabilityFailureReason::PathOutsidePattern);
    }

    // If pattern is absolute but path is relative, check if path starts with
    // the pattern's last component (handles project-root-relative paths).
    if Path::new(pattern).is_absolute() && !Path::new(path).is_absolute() {
        if let Some(last_component) = Path::new(pattern).file_name() {
            let prefix = last_component.to_string_lossy();
            if path.starts_with(prefix.as_ref()) {
                let remainder = &path[prefix.len()..];
                if remainder.is_empty() || remainder.starts_with('/') {
                    return None;
                }
            }
        }
        return Some(CapabilityFailureReason::PathOutsidePattern);
    }

    if let Some(base) = pattern.strip_suffix("/**") {
        if base.is_empty() {
            return None;
        }
        if path == base {
            return None;
        }
        let prefix = if base.ends_with('/') {
            base.to_string()
        } else {
            format!("{}/", base)
        };
        if path.starts_with(&prefix) {
            return None;
        }
        return Some(CapabilityFailureReason::PathOutsidePattern);
    }

    if let Some(remainder) = path.strip_prefix(pattern) {
        if remainder.is_empty() || remainder.starts_with('/') {
            return None;
        }
    }

    Some(CapabilityFailureReason::PathOutsidePattern)
}

/// Check if argv matches any entry in the allowlist (prefix match).
///
/// Reuses the same logic as `guard::has_run_command_capability`.
fn matches_argv_allowlist(allowlist: &[String], argv: &[String]) -> bool {
    for entry in allowlist {
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
    false
}

/// Check if a host matches the host pattern.
///
/// Supports exact match and wildcard `*`.
fn matches_host_pattern(pattern: &str, host: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    // Exact match (case-insensitive for hosts)
    pattern.eq_ignore_ascii_case(host)
}

/// Check for unsupported glob escape sequences in a pattern.
fn has_unsupported_glob_escape(pattern: &str) -> bool {
    // Reject patterns with unsupported glob characters (but allow /**)
    // We support: exact paths, /** suffix, simple prefix
    // Reject: *, ?, [], {} anywhere except /** suffix
    let stripped = pattern.strip_suffix("/**").unwrap_or(pattern);
    stripped.contains('*')
        || stripped.contains('?')
        || stripped.contains('[')
        || stripped.contains(']')
}

/// Get the appropriate action for a pattern rejection based on capability kind.
fn pattern_action_for(required: RequiredCapability) -> CapabilityActionRequired {
    match required {
        RequiredCapability::ReadPath | RequiredCapability::WritePath => {
            CapabilityActionRequired::WidenEnvelope
        }
        RequiredCapability::RunCommand => CapabilityActionRequired::ChangeCommand,
        RequiredCapability::FetchUrl => CapabilityActionRequired::ChangeUrl,
        RequiredCapability::Inspect | RequiredCapability::MutateSessionState => {
            CapabilityActionRequired::WidenEnvelope
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::envelope::Capability;

    fn base_source() -> CapabilitySource {
        CapabilitySource::Base
    }

    fn declared_source() -> CapabilitySource {
        CapabilitySource::Declared {
            envelope_id: "test-envelope".into(),
        }
    }

    // ── Test 1: ReadPath with Base source satisfies workspace.read_file ──

    #[test]
    fn read_path_base_source_satisfies_workspace_read_file() {
        let caps = vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "/home/user/project".into(),
            },
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            path: Some("/home/user/project/src/main.rs".into()),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::ReadPath,
            &invocation,
            &caps,
            "workspace.read_file",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::Satisfied);
        assert!(result.source_allowed);
        assert!(result.failure_reason.is_none());
        assert!(result.action_required.is_none());
    }

    // ── Test 2: ReadPath with Declared source satisfies workspace.read_file ──

    #[test]
    fn read_path_declared_source_satisfies_workspace_read_file() {
        let caps = vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "/home/user/project".into(),
            },
            source: declared_source(),
        }];
        let invocation = InvocationFacts {
            path: Some("/home/user/project/README.md".into()),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::ReadPath,
            &invocation,
            &caps,
            "workspace.read_file",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::Satisfied);
        assert!(result.source_allowed);
    }

    // ── Test 3: Inspect with Base source satisfies grep, list_directory, find_files ──

    #[test]
    fn inspect_base_source_satisfies_observation_methods() {
        let caps = vec![SourcedCapability {
            capability: Capability::Inspect,
            source: base_source(),
        }];
        let invocation = InvocationFacts::default();

        for method in &[
            "workspace.grep",
            "workspace.list_directory",
            "workspace.find_files",
        ] {
            let result = match_capability(
                RequiredCapability::Inspect,
                &invocation,
                &caps,
                method,
                &CapabilitySourcePolicy::default(),
            );
            assert_eq!(
                result.verdict,
                CapabilityMatchVerdict::Satisfied,
                "Inspect with Base should satisfy {method}"
            );
            assert!(result.source_allowed);
        }
    }

    // ── Test 4: Inspect with Declared source satisfies observation methods ──

    #[test]
    fn inspect_declared_source_satisfies_observation_methods() {
        let caps = vec![SourcedCapability {
            capability: Capability::Inspect,
            source: declared_source(),
        }];
        let invocation = InvocationFacts::default();

        for method in &[
            "workspace.grep",
            "workspace.list_directory",
            "workspace.find_files",
        ] {
            let result = match_capability(
                RequiredCapability::Inspect,
                &invocation,
                &caps,
                method,
                &CapabilitySourcePolicy::default(),
            );
            assert_eq!(
                result.verdict,
                CapabilityMatchVerdict::Satisfied,
                "Inspect with Declared should satisfy {method}"
            );
        }
    }

    // ── Test 5: WritePath with Declared source satisfies mutation methods ──

    #[test]
    fn write_path_declared_source_satisfies_mutation_methods() {
        let caps = vec![SourcedCapability {
            capability: Capability::WritePath {
                path_pattern: "src/**".into(),
            },
            source: declared_source(),
        }];
        let invocation = InvocationFacts {
            path: Some("src/main.rs".into()),
            ..Default::default()
        };

        for method in &["workspace.write_file", "workspace.patch"] {
            let result = match_capability(
                RequiredCapability::WritePath,
                &invocation,
                &caps,
                method,
                &CapabilitySourcePolicy::default(),
            );
            assert_eq!(
                result.verdict,
                CapabilityMatchVerdict::Satisfied,
                "WritePath with Declared should satisfy {method}"
            );
            assert!(result.source_allowed);
        }
    }

    // ── Test 6: WritePath with Base source is SourceNotAllowed for mutation (D-17) ──

    #[test]
    fn write_path_base_source_not_allowed_for_mutation() {
        let caps = vec![SourcedCapability {
            capability: Capability::WritePath {
                path_pattern: "src/**".into(),
            },
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            path: Some("src/main.rs".into()),
            ..Default::default()
        };

        for method in &["workspace.write_file", "workspace.patch"] {
            let result = match_capability(
                RequiredCapability::WritePath,
                &invocation,
                &caps,
                method,
                &CapabilitySourcePolicy::default(),
            );
            assert_eq!(
                result.verdict,
                CapabilityMatchVerdict::SourceNotAllowed,
                "Base-sourced WritePath must be SourceNotAllowed for {method}"
            );
            assert_eq!(
                result.failure_reason,
                Some(CapabilityFailureReason::SourceNotAllowed)
            );
            assert_eq!(
                result.action_required,
                Some(CapabilityActionRequired::DeclareEnvelope)
            );
        }
    }

    // ── Test 7: RunCommand with Declared source and matching argv ──

    #[test]
    fn run_command_declared_source_matching_argv_satisfies() {
        let caps = vec![SourcedCapability {
            capability: Capability::RunCommand {
                argv_allowlist: vec!["cargo test".into(), "cargo check".into()],
            },
            source: declared_source(),
        }];
        let invocation = InvocationFacts {
            argv: Some(vec!["cargo".into(), "test".into(), "--nocapture".into()]),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::RunCommand,
            &invocation,
            &caps,
            "command.run",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::Satisfied);
        assert!(result.source_allowed);
    }

    // ── Test 8: RunCommand with Base source is SourceNotAllowed (D-18) ──

    #[test]
    fn run_command_base_source_not_allowed() {
        let caps = vec![SourcedCapability {
            capability: Capability::RunCommand {
                argv_allowlist: vec!["cargo test".into()],
            },
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            argv: Some(vec!["cargo".into(), "test".into()]),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::RunCommand,
            &invocation,
            &caps,
            "command.run",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::SourceNotAllowed);
        assert_eq!(
            result.failure_reason,
            Some(CapabilityFailureReason::SourceNotAllowed)
        );
    }

    // ── Test 9: RunCommand with non-matching argv is PatternRejected ──

    #[test]
    fn run_command_non_matching_argv_pattern_rejected() {
        let caps = vec![SourcedCapability {
            capability: Capability::RunCommand {
                argv_allowlist: vec!["cargo test".into()],
            },
            source: declared_source(),
        }];
        let invocation = InvocationFacts {
            argv: Some(vec!["cargo".into(), "fmt".into()]),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::RunCommand,
            &invocation,
            &caps,
            "command.run",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::PatternRejected);
        assert_eq!(
            result.failure_reason,
            Some(CapabilityFailureReason::ArgvNotAllowed)
        );
        assert_eq!(
            result.action_required,
            Some(CapabilityActionRequired::ChangeCommand)
        );
    }

    // ── Test 10: RunCommand facts include cwd and env keys ──

    #[test]
    fn run_command_facts_include_cwd_and_env_keys() {
        let caps = vec![SourcedCapability {
            capability: Capability::RunCommand {
                argv_allowlist: vec!["cargo test".into()],
            },
            source: declared_source(),
        }];
        let invocation = InvocationFacts {
            argv: Some(vec!["cargo".into(), "test".into()]),
            cwd: Some(std::path::PathBuf::from("/home/user/project")),
            env_keys: vec!["RUST_LOG".into(), "CARGO_TERM_COLOR".into()],
            ..Default::default()
        };
        // The matcher does not consume cwd/env_keys for capability matching,
        // but they must be present in the facts for audit evidence.
        let result = match_capability(
            RequiredCapability::RunCommand,
            &invocation,
            &caps,
            "command.run",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::Satisfied);
        // Verify facts were constructed with cwd and env
        assert!(invocation.cwd.is_some());
        assert_eq!(invocation.env_keys.len(), 2);
    }

    // ── Test 11: FetchUrl with Base source satisfies web.fetch (D-15) ──

    #[test]
    fn fetch_url_base_source_satisfies_web_fetch() {
        let caps = vec![SourcedCapability {
            capability: Capability::FetchUrl {
                host_pattern: "example.com".into(),
            },
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            host: Some("example.com".into()),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::FetchUrl,
            &invocation,
            &caps,
            "web.fetch",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::Satisfied);
        assert!(result.source_allowed);
    }

    // ── Test 12: FetchUrl with Declared source satisfies web.fetch ──

    #[test]
    fn fetch_url_declared_source_satisfies_web_fetch() {
        let caps = vec![SourcedCapability {
            capability: Capability::FetchUrl {
                host_pattern: "api.github.com".into(),
            },
            source: declared_source(),
        }];
        let invocation = InvocationFacts {
            host: Some("api.github.com".into()),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::FetchUrl,
            &invocation,
            &caps,
            "web.fetch",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::Satisfied);
    }

    // ── Test 13: FetchUrl with non-matching host is PatternRejected ──

    #[test]
    fn fetch_url_non_matching_host_pattern_rejected() {
        let caps = vec![SourcedCapability {
            capability: Capability::FetchUrl {
                host_pattern: "api.github.com".into(),
            },
            source: declared_source(),
        }];
        let invocation = InvocationFacts {
            host: Some("evil.example.com".into()),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::FetchUrl,
            &invocation,
            &caps,
            "web.fetch",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::PatternRejected);
        assert_eq!(
            result.failure_reason,
            Some(CapabilityFailureReason::HostNotAllowed)
        );
        assert_eq!(
            result.action_required,
            Some(CapabilityActionRequired::ChangeUrl)
        );
    }

    // ── Test 14: MutateSessionState with Base source satisfies session.todo_write (D-16) ──

    #[test]
    fn mutate_session_state_base_source_satisfies_todo_write() {
        let caps = vec![SourcedCapability {
            capability: Capability::MutateSessionState,
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            session_state_operation: Some(SessionStateOperation::TodoWrite),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::MutateSessionState,
            &invocation,
            &caps,
            "session.todo_write",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::Satisfied);
        assert!(result.source_allowed);
    }

    // ── Test 15: MutateSessionState with Base source satisfies envelope.declare ──

    #[test]
    fn mutate_session_state_base_source_satisfies_envelope_declare() {
        let caps = vec![SourcedCapability {
            capability: Capability::MutateSessionState,
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            session_state_operation: Some(SessionStateOperation::EnvelopeDeclare),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::MutateSessionState,
            &invocation,
            &caps,
            "envelope.declare",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::Satisfied);
        assert!(result.source_allowed);
    }

    // ── Test 16: Missing capability returns NoMatchingCapability ──

    #[test]
    fn missing_capability_returns_no_matching() {
        let caps = vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: ".".into(),
            },
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            path: Some("src/main.rs".into()),
            ..Default::default()
        };
        // Ask for WritePath when only ReadPath exists
        let result = match_capability(
            RequiredCapability::WritePath,
            &invocation,
            &caps,
            "workspace.write_file",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::NoMatchingCapability);
        assert_eq!(
            result.failure_reason,
            Some(CapabilityFailureReason::MissingCapability)
        );
        assert_eq!(
            result.action_required,
            Some(CapabilityActionRequired::DeclareEnvelope)
        );
    }

    // ── Test 17: Path traversal in request or pattern is rejected ──

    #[test]
    fn path_traversal_rejected() {
        let caps = vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "/home/user/project".into(),
            },
            source: base_source(),
        }];

        // Traversal in invocation path
        let invocation = InvocationFacts {
            path: Some("/home/user/project/../etc/passwd".into()),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::ReadPath,
            &invocation,
            &caps,
            "workspace.read_file",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::PatternRejected);
        assert_eq!(
            result.failure_reason,
            Some(CapabilityFailureReason::PathTraversalRejected)
        );

        // Traversal in pattern
        let caps_traversal = vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "../etc".into(),
            },
            source: base_source(),
        }];
        let invocation_ok = InvocationFacts {
            path: Some("some/file".into()),
            ..Default::default()
        };
        let result2 = match_capability(
            RequiredCapability::ReadPath,
            &invocation_ok,
            &caps_traversal,
            "workspace.read_file",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result2.verdict, CapabilityMatchVerdict::PatternRejected);
        assert_eq!(
            result2.failure_reason,
            Some(CapabilityFailureReason::PathTraversalRejected)
        );
    }

    // ── Test 18: Glob escape in path pattern is rejected ──

    #[test]
    fn glob_escape_in_pattern_rejected() {
        let caps = vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "src/*.rs".into(),
            },
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            path: Some("src/main.rs".into()),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::ReadPath,
            &invocation,
            &caps,
            "workspace.read_file",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::PatternRejected);
        assert_eq!(
            result.failure_reason,
            Some(CapabilityFailureReason::GlobEscapeRejected)
        );
    }

    // ── Test 19: ReadPath outside pattern scope returns PatternRejected ──

    #[test]
    fn read_path_outside_pattern_scope_returns_pattern_rejected() {
        let caps = vec![SourcedCapability {
            capability: Capability::ReadPath {
                path_pattern: "src/**".into(),
            },
            source: base_source(),
        }];
        let invocation = InvocationFacts {
            path: Some("tests/integration_test.rs".into()),
            ..Default::default()
        };
        let result = match_capability(
            RequiredCapability::ReadPath,
            &invocation,
            &caps,
            "workspace.read_file",
            &CapabilitySourcePolicy::default(),
        );
        assert_eq!(result.verdict, CapabilityMatchVerdict::PatternRejected);
        assert_eq!(
            result.failure_reason,
            Some(CapabilityFailureReason::PathOutsidePattern)
        );
        assert_eq!(
            result.action_required,
            Some(CapabilityActionRequired::WidenEnvelope)
        );
    }
}
