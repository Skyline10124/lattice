// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Host-side Policy-Visible Workspace engine (ADR 0003 decision 5 / #61,
//! Codeberg #39).
//!
//! Built once per Tool call from the current Local state. Git index and
//! exclude stacks are not cached across Tool calls so `.gitignore` and
//! tracking changes take effect immediately. Non-git projects have no
//! ignore rules and treat every path as non-ignored; System Path Exclusions
//! still apply.

use std::path::Path;

use lancet_authority::{GitFileStatus, HiddenReason, ReadVisibility, ReadVisibilityPolicy};
use lancet_protocol::evidence::WorkspaceVisibilityEvidence;

/// Content-free category evidence accumulated while evaluating one Tool call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VisibilityEvidence {
    included_ignored: bool,
    filtered_system_exclusion: bool,
    filtered_read_denied: bool,
    filtered_ignored_default: bool,
}

impl VisibilityEvidence {
    pub fn into_protocol(self) -> WorkspaceVisibilityEvidence {
        WorkspaceVisibilityEvidence {
            included_ignored: self.included_ignored,
            filtered_system_exclusion: self.filtered_system_exclusion,
            filtered_read_denied: self.filtered_read_denied,
            filtered_ignored_default: self.filtered_ignored_default,
        }
    }

    fn record_hidden(&mut self, reason: HiddenReason) {
        match reason {
            HiddenReason::SystemExclusion => self.filtered_system_exclusion = true,
            HiddenReason::ReadDenied => self.filtered_read_denied = true,
            HiddenReason::GitIgnored => self.filtered_ignored_default = true,
        }
    }
}

/// Visibility and traversal assessment for one project-relative entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibilityAssessment {
    visibility: ReadVisibility,
    descend: bool,
}

impl VisibilityAssessment {
    pub fn is_visible(self) -> bool {
        matches!(self.visibility, ReadVisibility::Visible)
    }

    pub fn should_descend(self) -> bool {
        self.descend
    }

    pub fn hidden_reason(self) -> Option<HiddenReason> {
        match self.visibility {
            ReadVisibility::Visible => None,
            ReadVisibility::Hidden(reason) => Some(reason),
        }
    }
}

/// Per Tool-call visibility engine: pure policy + current Local Git facts.
pub struct VisibilityEngine {
    policy: ReadVisibilityPolicy,
    git: Option<GitVisibility>,
    evidence: VisibilityEvidence,
}

struct GitVisibility {
    repo: gix::Repository,
    index: gix::worktree::IndexPersistedOrInMemory,
    stack: gix::worktree::Stack,
}

impl VisibilityEngine {
    /// Load policy + Git facts for `root`. Open failure (not a git repo)
    /// yields a non-git engine that reports every path as `NotIgnored`.
    pub fn load(root: &Path, policy: ReadVisibilityPolicy) -> Self {
        Self {
            policy,
            git: GitVisibility::open(root),
            evidence: VisibilityEvidence::default(),
        }
    }

    pub fn assess(&mut self, rel: &str, is_dir: bool) -> VisibilityAssessment {
        let git = self.git_status(rel, is_dir);
        let visibility = self.policy.decide(rel, git);
        match visibility {
            ReadVisibility::Visible => {
                if git == GitFileStatus::Ignored {
                    self.evidence.included_ignored = true;
                }
            }
            ReadVisibility::Hidden(reason) => self.evidence.record_hidden(reason),
        }
        let descend = is_dir
            && (matches!(visibility, ReadVisibility::Visible)
                || matches!(visibility, ReadVisibility::Hidden(HiddenReason::GitIgnored))
                    && self.policy.may_reach_included(rel));
        VisibilityAssessment {
            visibility,
            descend,
        }
    }

    pub fn decide(&mut self, rel: &str, is_dir: bool) -> ReadVisibility {
        self.assess(rel, is_dir).visibility
    }

    pub fn is_visible_entry(&mut self, rel: &str, is_dir: bool) -> bool {
        self.assess(rel, is_dir).is_visible()
    }

    pub fn is_visible_file(&mut self, rel: &str) -> bool {
        self.is_visible_entry(rel, false)
    }

    /// Recursive navigation may pass through an ignored directory only when
    /// an explicit include can still be reached beneath it.
    pub fn should_descend(&mut self, dir_rel: &str) -> bool {
        self.assess(dir_rel, true).should_descend()
    }

    pub fn evidence(&self) -> VisibilityEvidence {
        self.evidence
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
            // Missing ignore data is treated as not ignored. System exclusions
            // and explicit read denials still apply, while callers handle paths
            // that disappeared during observation.
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
pub fn join_rel(parent: &str, name: &str) -> String {
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
    use tempfile::TempDir;

    fn init_repo_with_ignore() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let mut repo = gix::init(root).expect("gix init");
        // A fresh repo inherits no identity, and on a host without a global git
        // config (CI runners) writing the `refs/heads/main` reflog below fails
        // with `MissingCommitter`. Inject a fixture committer, matching the
        // pattern used by the lattice-api test fixtures and the system harness.
        {
            let mut cfg = repo.config_snapshot_mut();
            cfg.set_raw_value("user.name", "lattice-test")
                .expect("set name");
            cfg.set_raw_value("user.email", "test@lattice.local")
                .expect("set email");
            let _ = cfg.commit();
        }
        std::fs::write(root.join(".gitignore"), ".env\nsecret/\n").unwrap();
        std::fs::write(root.join("visible.txt"), "ok\n").unwrap();

        std::fs::write(root.join(".env"), "SECRET=1\n").unwrap();
        std::fs::create_dir_all(root.join("secret")).unwrap();
        std::fs::write(root.join("secret/key"), "hidden\n").unwrap();
        let empty = gix::ObjectId::empty_tree(repo.object_hash());
        let mut editor = repo.edit_tree(empty).expect("edit tree");
        for (rel, content) in [(".gitignore", ".env\nsecret/\n"), ("visible.txt", "ok\n")] {
            let blob = repo.write_blob(content.as_bytes()).expect("blob").detach();
            editor
                .upsert(rel, gix::objs::tree::EntryKind::Blob, blob)
                .expect("upsert");
        }
        let tree_id = editor.write().expect("write tree").detach();
        let sig = gix::actor::Signature {
            name: "t".into(),
            email: "t@t".into(),
            time: gix::date::Time::now_utc(),
        };
        let mut time_buf = gix::date::parse::TimeBuf::default();
        let sig_ref = sig.to_ref(&mut time_buf);
        let commit = repo
            .new_commit_as(sig_ref, sig_ref, "init", tree_id, None::<gix::ObjectId>)
            .expect("commit");
        repo.reference(
            "refs/heads/main",
            commit.id,
            gix::refs::transaction::PreviousValue::MustNotExist,
            "init",
        )
        .expect("update main");
        dir
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
        assert!(engine.is_visible_entry("secret", true));
        assert!(engine.should_descend("secret"));
        assert!(engine.evidence().into_protocol().included_ignored);
    }

    #[test]
    fn exact_descendant_include_separates_visibility_from_descent() {
        let dir = init_repo_with_ignore();
        let policy = ReadVisibilityPolicy::from_patterns(&["secret/key".into()], &[]).unwrap();
        let mut engine = VisibilityEngine::load(dir.path(), policy);

        let ancestor = engine.assess("secret", true);
        assert!(!ancestor.is_visible());
        assert!(ancestor.should_descend());
        assert!(engine.is_visible_entry("secret/key", false));
        assert_eq!(
            engine.evidence().into_protocol(),
            WorkspaceVisibilityEvidence {
                included_ignored: true,
                filtered_system_exclusion: false,
                filtered_read_denied: false,
                filtered_ignored_default: true,
            }
        );
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
