// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! API 工作区验收使用的非 bare Git 夹具；仅在 integration tests 编译。

use std::fs;
use std::path::Path;

use gix::objs::tree::EntryKind;
use gix::refs::transaction::PreviousValue;

const FIXTURE_AUTHOR_NAME: &str = "lattice-test";
const FIXTURE_AUTHOR_EMAIL: &str = "test@lattice.local";

/// Initialize an empty non-bare git repository at `dir` (creates the directory
/// if needed). No commits are created.
fn init_empty_repo(dir: &Path) -> gix::Repository {
    fs::create_dir_all(dir).expect("create fixture dir");
    // Fixture setup is deliberately side-effect free with respect to the
    // process CWD. A deleted CWD is an environment failure, not a reason for
    // a helper to change another test's process-global directory.
    let mut repo = gix::init(dir).expect("gix init empty repo");
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
fn commit_paths(
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
