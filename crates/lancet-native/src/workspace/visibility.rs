// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Host-side Policy-Visible Workspace engine (ADR 0003 decision 5 / #39).
//!
//! Built once per Tool call from the current Local state. Git index and
//! exclude stacks are not cached across Tool calls so `.gitignore` and
//! tracking changes take effect immediately. Non-git projects have no
//! ignore rules and treat every path as non-ignored; System Path Exclusions
//! still apply.

use std::path::Path;

use lancet_authority::{GitFileStatus, HiddenReason, ReadVisibility, ReadVisibilityPolicy};

/// Per Tool-call visibility engine: pure policy + current Local Git facts.
pub(crate) struct VisibilityEngine {
    policy: ReadVisibilityPolicy,
    git: Option<GitVisibility>,
}

struct GitVisibility {
    repo: gix::Repository,
    index: gix::worktree::IndexPersistedOrInMemory,
    stack: gix::worktree::Stack,
}

impl VisibilityEngine {
    /// Load policy + Git facts for `root`. Open failure (not a git repo)
    /// yields a non-git engine that reports every path as `NotIgnored`.
    pub(crate) fn load(root: &Path, policy: ReadVisibilityPolicy) -> Self {
        Self {
            policy,
            git: GitVisibility::open(root),
        }
    }

    pub(crate) fn decide(&mut self, rel: &str, is_dir: bool) -> ReadVisibility {
        let git = self.git_status(rel, is_dir);
        self.policy.decide(rel, git)
    }

    pub(crate) fn is_visible_file(&mut self, rel: &str) -> bool {
        matches!(self.decide(rel, false), ReadVisibility::Visible)
    }

    /// List/find/grep descent: visible entries, or hidden dirs that an
    /// include pattern may still reach.
    pub(crate) fn is_listable(&mut self, rel: &str, is_dir: bool) -> bool {
        match self.decide(rel, is_dir) {
            ReadVisibility::Visible => true,
            ReadVisibility::Hidden(HiddenReason::GitIgnored) if is_dir => {
                self.policy.may_reach_included(rel)
            }
            ReadVisibility::Hidden(_) => false,
        }
    }

    pub(crate) fn should_descend(&mut self, dir_rel: &str) -> bool {
        self.is_listable(dir_rel, true)
    }

    fn git_status(&mut self, rel: &str, is_dir: bool) -> GitFileStatus {
        let Some(git) = self.git.as_mut() else {
            return GitFileStatus::NotIgnored;
        };
        let rel = normalize_rel(rel);
        if rel.is_empty() {
            return GitFileStatus::NotIgnored;
        }
        // Tracked ∪ non-ignored is a union: tracked-but-ignored is visible.
        if !is_dir {
            let bpath = gix::bstr::BStr::new(rel.as_bytes());
            if git.index.entry_by_path(bpath).is_some() {
                return GitFileStatus::Tracked;
            }
        }
        let mode = if is_dir {
            Some(gix::index::entry::Mode::DIR)
        } else {
            Some(gix::index::entry::Mode::FILE)
        };
        match git.stack.at_path(Path::new(rel), mode, &git.repo.objects) {
            Ok(platform) if platform.is_excluded() => GitFileStatus::Ignored,
            Ok(_) => GitFileStatus::NotIgnored,
            // Fail open on stack errors would leak secrets; fail closed as ignored
            // only if we cannot evaluate. Prefer NotIgnored for I/O errors on
            // intermediate dirs that simply don't exist yet — callers already
            // handle missing paths. Conservative: treat evaluation failure as
            // NotIgnored so System Exclusion / denied_read still apply.
            Err(_) => GitFileStatus::NotIgnored,
        }
    }
}

impl GitVisibility {
    fn open(root: &Path) -> Option<Self> {
        let repo = gix::open_opts(root, gix::open::Options::isolated()).ok()?;
        let index = repo.index_or_load_from_head_or_empty().ok()?;
        let attr = repo
            .excludes(
                &index,
                None,
                gix::worktree::stack::state::ignore::Source::WorktreeThenIdMappingIfNotSkipped,
            )
            .ok()?;
        let stack = attr.detach();
        Some(Self { repo, index, stack })
    }
}

fn normalize_rel(rel: &str) -> &str {
    let rel = rel.trim_start_matches("./");
    if rel.is_empty() || rel == "." {
        ""
    } else {
        rel.trim_end_matches('/')
    }
}

/// Join a parent WorkspacePath (or ".") with an entry name into a
/// project-relative UTF-8 path string.
pub(crate) fn join_rel(parent: &str, name: &str) -> String {
    let parent = normalize_rel(parent);
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo_with_ignore() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        assert!(
            Command::new("git")
                .args(["init"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(root.join(".gitignore"), ".env\nsecret/\n").unwrap();
        std::fs::write(root.join("visible.txt"), "ok\n").unwrap();
        std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        std::fs::create_dir(root.join("secret")).unwrap();
        std::fs::write(root.join("secret/key"), "k\n").unwrap();
        assert!(
            Command::new("git")
                .args(["add", "visible.txt", ".gitignore"])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "user.name=t",
                    "commit",
                    "-m",
                    "init"
                ])
                .current_dir(root)
                .status()
                .unwrap()
                .success()
        );
        dir
    }

    #[test]
    fn secret_sentinel_hidden_by_default() {
        let dir = init_repo_with_ignore();
        let policy = ReadVisibilityPolicy::from_patterns(&[], &[]).unwrap();
        let mut engine = VisibilityEngine::load(dir.path(), policy);
        assert!(matches!(
            engine.decide("visible.txt", false),
            ReadVisibility::Visible
        ));
        assert!(matches!(
            engine.decide(".env", false),
            ReadVisibility::Hidden(HiddenReason::GitIgnored)
        ));
        assert!(matches!(
            engine.decide("secret/key", false),
            ReadVisibility::Hidden(HiddenReason::GitIgnored)
        ));
        assert!(!engine.is_listable(".env", false));
        assert!(!engine.should_descend("secret"));
    }

    #[test]
    fn include_exposes_ignored_and_allows_descent() {
        let dir = init_repo_with_ignore();
        let policy =
            ReadVisibilityPolicy::from_patterns(&[".env".into(), "secret/**".into()], &[]).unwrap();
        let mut engine = VisibilityEngine::load(dir.path(), policy);
        assert!(matches!(
            engine.decide(".env", false),
            ReadVisibility::Visible
        ));
        assert!(matches!(
            engine.decide("secret/key", false),
            ReadVisibility::Visible
        ));
        assert!(engine.should_descend("secret"));
    }

    #[test]
    fn system_exclusion_blocks_include() {
        let dir = init_repo_with_ignore();
        let policy =
            ReadVisibilityPolicy::from_patterns(&[".git/**".into(), ".git/config".into()], &[])
                .unwrap();
        let mut engine = VisibilityEngine::load(dir.path(), policy);
        assert!(matches!(
            engine.decide(".git/config", false),
            ReadVisibility::Hidden(HiddenReason::SystemExclusion)
        ));
        assert!(!engine.should_descend(".git"));
    }

    #[test]
    fn non_git_project_has_no_ignore() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(".env"), "x\n").unwrap();
        let policy = ReadVisibilityPolicy::from_patterns(&[], &[]).unwrap();
        let mut engine = VisibilityEngine::load(dir.path(), policy);
        assert!(matches!(
            engine.decide(".env", false),
            ReadVisibility::Visible
        ));
    }

    #[test]
    fn gitignore_change_is_visible_on_next_load() {
        let dir = init_repo_with_ignore();
        let policy = ReadVisibilityPolicy::from_patterns(&[], &[]).unwrap();
        let mut engine = VisibilityEngine::load(dir.path(), policy.clone());
        assert!(matches!(
            engine.decide("visible.txt", false),
            ReadVisibility::Visible
        ));
        std::fs::write(
            dir.path().join(".gitignore"),
            ".env\nsecret/\nvisible.txt\n",
        )
        .unwrap();
        let mut engine = VisibilityEngine::load(dir.path(), policy.clone());
        // tracked files remain visible even if newly ignored
        assert!(matches!(
            engine.decide("visible.txt", false),
            ReadVisibility::Visible
        ));
        // untracked new file matching ignore is hidden
        std::fs::write(dir.path().join("visible2.txt"), "x\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "visible2.txt\n").unwrap();
        let mut engine = VisibilityEngine::load(dir.path(), policy);
        assert!(matches!(
            engine.decide("visible2.txt", false),
            ReadVisibility::Hidden(HiddenReason::GitIgnored)
        ));
    }
}
