// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! File, ref, and directory durability barriers (ADR 0005 §4).
//!
//! Every barrier failure is a typed [`StoreError::Durability`] — durability is
//! never best-effort on the write path. gix writes loose objects and loose
//! refs; these helpers fsync exactly the files the kernel knows were written
//! plus their parent directories, which is what makes a publication survive a
//! crash in the order the calls happened.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use crate::StoreError;

/// fsync one file's data and metadata.
pub fn fsync_file(path: &Path) -> Result<(), StoreError> {
    let file = File::open(path).map_err(|source| StoreError::Io {
        operation: "open for fsync",
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| StoreError::Durability {
        operation: "fsync file",
        path: path.to_path_buf(),
        source,
    })
}

/// fsync one directory entry point (Linux allows opening directories
/// read-only for exactly this purpose).
pub fn fsync_dir(path: &Path) -> Result<(), StoreError> {
    let dir = File::open(path).map_err(|source| StoreError::Io {
        operation: "open directory for fsync",
        path: path.to_path_buf(),
        source,
    })?;
    dir.sync_all().map_err(|source| StoreError::Durability {
        operation: "fsync directory",
        path: path.to_path_buf(),
        source,
    })
}

/// The on-disk path of a loose object inside a git directory.
pub fn loose_object_path(git_dir: &Path, oid: gix::ObjectId) -> PathBuf {
    let hex = oid.to_string();
    git_dir.join("objects").join(&hex[..2]).join(&hex[2..])
}

/// The on-disk path of a loose ref inside a git directory.
pub fn loose_ref_path(git_dir: &Path, ref_name: &str) -> PathBuf {
    git_dir.join(ref_name)
}

/// Durability barrier for a loose-object + loose-ref publication: fsync each
/// written object file, each written ref file, and every directory on the
/// path from the deepest fanout up to the git directory itself.
///
/// Missing object/ref files are tolerated (an object that already existed is
/// not rewritten); directories are always synced.
pub fn fsync_publication(
    git_dir: &Path,
    written_oids: &[gix::ObjectId],
    written_refs: &[&str],
) -> Result<(), StoreError> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push_dir = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    for oid in written_oids {
        let path = loose_object_path(git_dir, *oid);
        if path.exists() {
            fsync_file(&path)?;
        }
        if let Some(fanout) = path.parent() {
            push_dir(fanout.to_path_buf());
        }
    }
    push_dir(git_dir.join("objects"));

    for ref_name in written_refs {
        let path = loose_ref_path(git_dir, ref_name);
        if path.exists() {
            fsync_file(&path)?;
        }
        // Every directory between the ref file and the git dir can hold the
        // new directory entries (`refs/`, `refs/lattice/`, ...).
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d == git_dir {
                break;
            }
            push_dir(d.to_path_buf());
            dir = d.parent();
        }
    }
    push_dir(git_dir.to_path_buf());

    // Deepest-first ordering is irrelevant to correctness (each fsync is
    // independent) but syncing children before parents matches the usual
    // durability write-down order. A directory that no longer exists is
    // skipped rather than an error: publishing a ref *deletion* can prune the
    // now-empty directory that held it, and its surviving parents are synced
    // in the same pass.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in dirs {
        if dir.is_dir() {
            fsync_dir(&dir)?;
        }
    }
    Ok(())
}

/// Recursively fsync every file and directory under `root`, then `root`
/// itself. Used once per first-create publication, where the whole fresh
/// repository must be durable before the rename publishes it.
pub fn fsync_tree(root: &Path) -> Result<(), StoreError> {
    if root.is_dir() {
        for entry in fs::read_dir(root).map_err(|source| StoreError::Io {
            operation: "reading directory for fsync",
            path: root.to_path_buf(),
            source,
        })? {
            let entry = entry.map_err(|source| StoreError::Io {
                operation: "reading directory entry for fsync",
                path: root.to_path_buf(),
                source,
            })?;
            fsync_tree(&entry.path())?;
        }
        fsync_dir(root)
    } else {
        fsync_file(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loose_paths_match_git_layout() {
        let git = Path::new("/repo.git");
        let oid = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        assert_eq!(
            loose_object_path(git, oid),
            PathBuf::from("/repo.git/objects/01/23456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(
            loose_ref_path(git, "refs/lattice/meta"),
            PathBuf::from("/repo.git/refs/lattice/meta")
        );
    }

    #[test]
    fn fsync_publication_on_empty_is_just_dir_syncs() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("r.git");
        fs::create_dir_all(git.join("objects")).unwrap();
        fsync_publication(&git, &[], &[]).unwrap();
    }
}
