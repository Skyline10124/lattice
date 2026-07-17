// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Policy-Visible Workspace read visibility (ADR 0003 decision 5).
//!
//! Pure decision layer: gitignore facts are injected by the caller. This
//! module never performs I/O. Precedence is System Path Exclusion, explicit
//! read denial, explicit ignored-path inclusion, then ordinary Git-ignore
//! selection.

use std::path::{Component, Path, PathBuf};

use crate::guard::{WritePathPattern, normalize_write_path_pattern};

/// Runtime-owned path roots that profile policy can never relax.
///
/// Root escape is enforced by secure path resolution and is not listed here.
pub const SYSTEM_PATH_EXCLUSIONS: &[&str] = &[".git", ".lattice"];

/// Git-side file status injected by the host visibility engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileStatus {
    /// Present in the index (tracked). Tracked files are visible even if
    /// they would also match an ignore rule.
    Tracked,
    /// Not tracked and not ignored.
    NotIgnored,
    /// Not tracked and matched by an ignore rule.
    Ignored,
}

/// Why a path is hidden from the Agent-facing observation surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HiddenReason {
    SystemExclusion,
    ReadDenied,
    GitIgnored,
}

impl HiddenReason {
    /// Stable audit label for durable evidence (never returned to the Agent).
    pub fn as_audit_label(self) -> &'static str {
        match self {
            Self::SystemExclusion => "system_exclusion",
            Self::ReadDenied => "read_denied",
            Self::GitIgnored => "ignored_default",
        }
    }
}

/// Agent-facing visibility decision for one project-relative path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadVisibility {
    Visible,
    Hidden(HiddenReason),
}

/// Compiled read-visibility policy for one Turn / Tool call.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ReadVisibilityPolicy {
    included_ignored: Vec<WritePathPattern>,
    denied_read: Vec<WritePathPattern>,
}

impl ReadVisibilityPolicy {
    /// Compile pattern strings using Guard L1 write-pattern rules.
    pub fn from_patterns(
        included_ignored: &[String],
        denied_read: &[String],
    ) -> Result<Self, String> {
        Ok(Self {
            included_ignored: compile_patterns(included_ignored, "included_ignored_read_patterns")?,
            denied_read: compile_patterns(denied_read, "denied_read_patterns")?,
        })
    }

    /// Pure decision order (CONTEXT.md / ADR 0003 decision 5):
    /// System Exclusion > denied_read > included_ignored > gitignore default.
    pub fn decide(&self, rel: &str, git: GitFileStatus) -> ReadVisibility {
        let rel = normalize_rel(rel);
        if under_system_exclusion(rel) {
            return ReadVisibility::Hidden(HiddenReason::SystemExclusion);
        }
        if self.matches_any(&self.denied_read, rel) {
            return ReadVisibility::Hidden(HiddenReason::ReadDenied);
        }
        match git {
            GitFileStatus::Tracked | GitFileStatus::NotIgnored => ReadVisibility::Visible,
            GitFileStatus::Ignored if self.matches_any(&self.included_ignored, rel) => {
                ReadVisibility::Visible
            }
            GitFileStatus::Ignored => ReadVisibility::Hidden(HiddenReason::GitIgnored),
        }
    }

    /// Whether a hidden directory should still be descended into because an
    /// `included_ignored_read_patterns` entry may match a path inside it.
    pub fn may_reach_included(&self, dir_rel: &str) -> bool {
        let dir = normalize_rel(dir_rel);
        // System exclusions never open a navigation hole.
        if under_system_exclusion(dir) {
            return false;
        }
        self.included_ignored
            .iter()
            .any(|pattern| pattern_reachable_from_dir(pattern, dir))
    }

    fn matches_any(&self, patterns: &[WritePathPattern], rel: &str) -> bool {
        patterns.iter().any(|p| pattern_matches(p, rel))
    }
}

/// True when any path component is a System Path Exclusion root.
pub fn under_system_exclusion(rel: &str) -> bool {
    let rel = normalize_rel(rel);
    if rel.is_empty() {
        return false;
    }
    Path::new(rel).components().any(|c| match c {
        Component::Normal(part) => SYSTEM_PATH_EXCLUSIONS
            .iter()
            .any(|ex| part.to_str() == Some(*ex)),
        _ => false,
    })
}

fn compile_patterns(patterns: &[String], field: &str) -> Result<Vec<WritePathPattern>, String> {
    let mut out = Vec::with_capacity(patterns.len());
    for pattern in patterns {
        if pattern.is_empty() {
            return Err(format!("{field} entry must not be empty"));
        }
        if pattern.contains('\0') {
            return Err(format!("{field} entry must not contain NUL bytes"));
        }
        if pattern.contains("..") {
            return Err(format!("{field} entry must not contain '..' traversal"));
        }
        if Path::new(pattern).is_absolute() {
            return Err(format!("{field} entry must be project-root-relative"));
        }
        let compiled = normalize_write_path_pattern(pattern).map_err(|e| e.message)?;
        out.push(compiled);
    }
    Ok(out)
}

fn normalize_rel(rel: &str) -> &str {
    let rel = rel.trim_start_matches("./");
    if rel.is_empty() || rel == "." {
        ""
    } else {
        rel.trim_end_matches('/')
    }
}

fn pattern_matches(pattern: &WritePathPattern, rel: &str) -> bool {
    let rel = normalize_rel(rel);
    if rel.is_empty() {
        return false;
    }
    match pattern {
        WritePathPattern::ExactFile(path) => path_eq(path, rel),
        WritePathPattern::DirectoryGlob(base) => {
            path_eq(base, rel) || rel_has_prefix(rel, base)
        }
    }
}

fn pattern_reachable_from_dir(pattern: &WritePathPattern, dir: &str) -> bool {
    // From project root every pattern is potentially reachable.
    if dir.is_empty() {
        return true;
    }
    let target: PathBuf = match pattern {
        WritePathPattern::ExactFile(path) | WritePathPattern::DirectoryGlob(path) => path.clone(),
    };
    let target = target.to_string_lossy();
    let target = normalize_rel(target.as_ref());
    // dir is an ancestor of the pattern target, the target itself, or the
    // pattern is a directory glob that already covers dir.
    path_eq(Path::new(target), dir)
        || rel_has_prefix(target, Path::new(dir))
        || rel_has_prefix(dir, Path::new(target))
}

fn path_eq(path: &Path, rel: &str) -> bool {
    path.to_string_lossy() == rel
}

fn rel_has_prefix(rel: &str, base: &Path) -> bool {
    let base = base.to_string_lossy();
    let base = normalize_rel(base.as_ref());
    if base.is_empty() {
        return !rel.is_empty();
    }
    rel.starts_with(base) && rel[base.len()..].starts_with('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(included: &[&str], denied: &[&str]) -> ReadVisibilityPolicy {
        ReadVisibilityPolicy::from_patterns(
            &included.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
            &denied.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
        )
        .expect("policy")
    }

    #[test]
    fn tracked_and_not_ignored_are_visible_by_default() {
        let p = policy(&[], &[]);
        assert_eq!(
            p.decide("src/main.rs", GitFileStatus::Tracked),
            ReadVisibility::Visible
        );
        assert_eq!(
            p.decide("scratch.txt", GitFileStatus::NotIgnored),
            ReadVisibility::Visible
        );
    }

    #[test]
    fn ignored_hidden_unless_included() {
        let p = policy(&[], &[]);
        assert_eq!(
            p.decide(".env", GitFileStatus::Ignored),
            ReadVisibility::Hidden(HiddenReason::GitIgnored)
        );

        let p = policy(&[".env"], &[]);
        assert_eq!(
            p.decide(".env", GitFileStatus::Ignored),
            ReadVisibility::Visible
        );
    }

    #[test]
    fn read_denial_beats_include_and_tracked() {
        let p = policy(&[".env", "src/**"], &[".env", "src/secret.rs"]);
        assert_eq!(
            p.decide(".env", GitFileStatus::Ignored),
            ReadVisibility::Hidden(HiddenReason::ReadDenied)
        );
        assert_eq!(
            p.decide("src/secret.rs", GitFileStatus::Tracked),
            ReadVisibility::Hidden(HiddenReason::ReadDenied)
        );
        assert_eq!(
            p.decide("src/main.rs", GitFileStatus::Tracked),
            ReadVisibility::Visible
        );
    }

    #[test]
    fn system_exclusion_beats_include() {
        let p = policy(&[".git/**", ".git/config", ".lattice/**"], &[]);
        for path in [".git", ".git/config", "nested/.git/config", ".lattice/state"] {
            assert_eq!(
                p.decide(path, GitFileStatus::Ignored),
                ReadVisibility::Hidden(HiddenReason::SystemExclusion),
                "{path}"
            );
            assert!(!p.may_reach_included(path), "{path}");
        }
    }

    #[test]
    fn may_reach_included_for_prefix_navigation() {
        let p = policy(&["secret/key", "vendor/**"], &[]);
        assert!(p.may_reach_included(""));
        assert!(p.may_reach_included("secret"));
        assert!(p.may_reach_included("vendor"));
        assert!(p.may_reach_included("vendor/pkg"));
        assert!(!p.may_reach_included("other"));
        assert!(!p.may_reach_included(".git"));
    }

    #[test]
    fn rejects_invalid_patterns() {
        assert!(ReadVisibilityPolicy::from_patterns(&["../x".into()], &[]).is_err());
        assert!(ReadVisibilityPolicy::from_patterns(&[], &["/abs".into()]).is_err());
        assert!(ReadVisibilityPolicy::from_patterns(&["".into()], &[]).is_err());
    }

    #[test]
    fn directory_glob_include_makes_dir_visible() {
        let p = policy(&["secret/**"], &[]);
        assert_eq!(
            p.decide("secret", GitFileStatus::Ignored),
            ReadVisibility::Visible
        );
        assert_eq!(
            p.decide("secret/key", GitFileStatus::Ignored),
            ReadVisibility::Visible
        );
    }
}
