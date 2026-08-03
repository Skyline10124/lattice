// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Shared gix-backed fixture helpers for tests.
//!
//! Replaces shell-git and legacy-git-bindings fixture bootstraps across the
//! workspace. Enabled by the `test-utils` feature (and always available to
//! this crate's own tests).

use std::fs;
use std::path::Path;

use gix::objs::tree::EntryKind;
use gix::refs::transaction::PreviousValue;

const FIXTURE_AUTHOR_NAME: &str = "lattice-test";
const FIXTURE_AUTHOR_EMAIL: &str = "test@lattice.local";

/// Initialize an empty non-bare git repository at `dir` (creates the directory
/// if needed). No commits are created.
pub fn init_empty_repo(dir: &Path) -> gix::Repository {
    fs::create_dir_all(dir).expect("create fixture dir");
    // gix queries the process CWD even for an absolute target. Tests in other
    // crates `set_current_dir` into a TempDir (see lattice-api's
    // consistency/release_e2e/usage_turn fixtures); once that dir is dropped the
    // process has no valid CWD and `gix::init` fails with
    // `Init(CurrentDir(NotFound))`.
    //
    // Checking `current_dir()` first is a TOCTOU race — the directory can be
    // removed between the check and gix's own read — so retry once from a
    // known-good CWD instead of pre-validating.
    let mut repo = match gix::init(dir) {
        Ok(repo) => repo,
        // The variant is `gix::init::Error::Init(gix::create::Error::CurrentDir(_))`;
        // match on the rendered form so a gix version bump that reshapes the
        // nesting still recovers instead of hard-failing.
        Err(err) if format!("{err:?}").contains("CurrentDir") => {
            std::env::set_current_dir(std::env::temp_dir()).expect("restore fixture cwd");
            // The failed attempt may already have created `.git` before it
            // needed the CWD, so accept an existing repo on retry.
            match gix::init(dir) {
                Ok(repo) => repo,
                Err(gix::init::Error::Init(gix::create::Error::DirectoryExists { .. })) => {
                    gix::open(dir).expect("open fixture repo after cwd recovery")
                }
                Err(err) => panic!("gix init empty repo after cwd recovery: {err:?}"),
            }
        }
        Err(err) => panic!("gix init empty repo: {err:?}"),
    };
    // A fresh repo inherits no identity. Ref writes (e.g. `refs/heads/main` in
    // `commit_fixture`) also write a reflog entry, which requires a committer;
    // on a host without a global git config that fails with
    // `MissingCommitter`. Every fixture repo in this crate is born here, so
    // injecting the identity once covers all of them.
    {
        let mut cfg = repo.config_snapshot_mut();
        cfg.set_raw_value("user.name", "lattice-test")
            .expect("set fixture user.name");
        cfg.set_raw_value("user.email", "test@lattice.local")
            .expect("set fixture user.email");
        let _ = cfg.commit();
    }
    repo
}

/// Initialize a non-bare repository, write `files` into the worktree, and
/// create an initial commit on `refs/heads/main`. Returns the open repository.
///
/// If `files` is empty, creates an empty-tree commit so HEAD is born.
pub fn init_fixture_repo(dir: &Path, files: &[(&str, &str)]) -> gix::Repository {
    let repo = init_empty_repo(dir);
    for (rel, content) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs for fixture file");
        }
        fs::write(&path, content).expect("write fixture file");
    }
    commit_paths(&repo, dir, files, "init");
    repo
}

/// Stage the listed relative worktree paths (already written under `root`) and
/// create a new commit on `refs/heads/main`.
pub fn commit_paths(
    repo: &gix::Repository,
    root: &Path,
    files: &[(&str, &str)],
    message: &str,
) -> gix::ObjectId {
    let base_tree = match repo.head_id() {
        Ok(id) => repo
            .find_commit(id)
            .ok()
            .and_then(|c| c.tree_id().ok())
            .map(|t| t.detach())
            .unwrap_or_else(|| gix::ObjectId::empty_tree(repo.object_hash())),
        Err(_) => gix::ObjectId::empty_tree(repo.object_hash()),
    };

    let mut editor = repo
        .edit_tree(base_tree)
        .expect("edit tree for fixture commit");
    for (rel, _) in files {
        let bytes = fs::read(root.join(rel)).expect("read worktree file for commit");
        let blob_id = repo
            .write_blob(&bytes)
            .expect("write fixture blob")
            .detach();
        editor
            .upsert(*rel, EntryKind::Blob, blob_id)
            .expect("upsert fixture path into tree");
    }
    let tree_id = editor.write().expect("write fixture tree").detach();

    let parents: Vec<gix::ObjectId> = match repo.head_id() {
        Ok(id) => vec![id.detach()],
        Err(_) => Vec::new(),
    };

    let commit_id = write_commit(repo, tree_id, &parents, message);
    let constraint = match parents.first() {
        Some(parent) => PreviousValue::MustExistAndMatch(gix::refs::Target::Object(*parent)),
        None => PreviousValue::MustNotExist,
    };
    // gix::init leaves HEAD as a symbolic ref to refs/heads/main. Creating
    // the first commit via the main ref is enough to birth HEAD.
    repo.reference("refs/heads/main", commit_id, constraint, "fixture commit")
        .expect("update refs/heads/main");
    commit_id
}

/// Create a commit that captures every regular file under `root` (skipping
/// `.git`) and advances `refs/heads/main`.
pub fn commit_all(repo: &gix::Repository, root: &Path, message: &str) -> gix::ObjectId {
    let mut paths = Vec::new();
    collect_file_paths(root, root, &mut paths);
    let files: Vec<(&str, &str)> = paths.iter().map(|path| (path.as_str(), "")).collect();
    commit_paths(repo, root, &files, message)
}

fn collect_file_paths(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == ".git" {
            continue;
        }
        if path.is_dir() {
            collect_file_paths(root, &path, out);
        } else if path.is_file() {
            let rel = path
                .strip_prefix(root)
                .expect("path under root")
                .to_string_lossy()
                .into_owned();
            out.push(rel);
        }
    }
}

fn write_commit(
    repo: &gix::Repository,
    tree: gix::ObjectId,
    parents: &[gix::ObjectId],
    message: &str,
) -> gix::ObjectId {
    let sig = gix::actor::Signature {
        name: FIXTURE_AUTHOR_NAME.into(),
        email: FIXTURE_AUTHOR_EMAIL.into(),
        time: gix::date::Time::now_utc(),
    };
    let mut time_buf = gix::date::parse::TimeBuf::default();
    let sig_ref = sig.to_ref(&mut time_buf);
    repo.new_commit_as(sig_ref, sig_ref, message, tree, parents.iter().copied())
        .expect("write fixture commit")
        .id
}

#[cfg(test)]
mod cwd_recovery_tests {
    use super::*;

    /// `gix::init` reads the process CWD even for an absolute target. Other
    /// crates' tests chdir into a TempDir that later gets dropped, leaving the
    /// process without a valid CWD; `init_empty_repo` must recover instead of
    /// panicking.
    ///
    /// Run serially and marked `#[ignore]`: the CWD is process-global, so
    /// invalidating it races every fixture running in parallel in this binary.
    /// Verify with
    /// `cargo test -p lattice-session -- --ignored --test-threads=1`.
    #[test]
    #[ignore = "mutates the process-global CWD; run with --test-threads=1"]
    fn init_empty_repo_recovers_from_deleted_cwd() {
        let original = std::env::current_dir().expect("cwd before test");
        let target = tempfile::TempDir::new().expect("target");

        let doomed = tempfile::TempDir::new().expect("doomed cwd");
        std::env::set_current_dir(doomed.path()).expect("chdir into doomed");
        drop(doomed);
        let cwd_is_invalid = std::env::current_dir().is_err();

        let outcome = std::panic::catch_unwind(|| {
            let repo = init_empty_repo(&target.path().join("repo"));
            repo.git_dir().exists()
        });

        // Restore before asserting so a failure cannot poison sibling tests.
        std::env::set_current_dir(&original).expect("restore cwd");

        assert!(cwd_is_invalid, "precondition: process CWD must be invalid");
        assert!(
            matches!(outcome, Ok(true)),
            "init_empty_repo must recover from a deleted CWD: {outcome:?}"
        );
    }

    /// The production counterpart of the test above. `GitSessionGraphStore`
    /// auto-initializes its repository on first access, and that init hits the
    /// same `gix` CWD read — a user whose checkout was deleted or whose volume
    /// was unmounted would otherwise get `Could not obtain the current
    /// directory` from a store whose own directory is perfectly fine.
    ///
    /// Covers both init paths in `git_graph::GitSessionGraphStore::repo`:
    /// the bare init taken for an empty directory, and the worktree init taken
    /// when the directory already holds non-git content.
    ///
    /// Same `#[ignore]` rationale as above: invalidating the process-global CWD
    /// races every fixture running in parallel in this binary.
    #[test]
    #[ignore = "mutates the process-global CWD; run with --test-threads=1"]
    fn session_store_auto_init_recovers_from_deleted_cwd() {
        use crate::git_graph::GitSessionGraphStore;
        use crate::graph::SessionGraphStore;

        for (label, seed_content) in [("empty_dir", false), ("non_empty_dir", true)] {
            let original = std::env::current_dir().expect("cwd before test");
            let target = tempfile::TempDir::new().expect("target");
            let store_dir = target.path().join(label);
            fs::create_dir_all(&store_dir).expect("create store dir");
            if seed_content {
                // Forces the worktree-init branch rather than the bare one.
                fs::write(store_dir.join("stray.txt"), b"not a git dir")
                    .expect("seed non-git content");
            }

            let doomed = tempfile::TempDir::new().expect("doomed cwd");
            std::env::set_current_dir(doomed.path()).expect("chdir into doomed");
            drop(doomed);
            let cwd_is_invalid = std::env::current_dir().is_err();

            let outcome = std::panic::catch_unwind(|| {
                GitSessionGraphStore::new(&store_dir).create_session(
                    "sess-cwd-recovery",
                    None,
                    "coder",
                    "main",
                )
            });

            // Restore before asserting so a failure cannot poison siblings.
            std::env::set_current_dir(&original).expect("restore cwd");

            assert!(
                cwd_is_invalid,
                "{label}: precondition: process CWD must be invalid"
            );
            match outcome {
                Ok(Ok(session)) => assert_eq!(session.session_id, "sess-cwd-recovery"),
                other => {
                    panic!("{label}: session store must recover from a deleted CWD, got {other:?}")
                }
            }
        }
    }
}
