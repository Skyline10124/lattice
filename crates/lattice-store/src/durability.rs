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
    crate::fault::on_fsync(path)?;
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
    crate::fault::on_fsync(path)?;
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

/// Two-digit lowercase hex prefix for a loose-object fanout directory.
pub fn loose_object_fanout_hex(prefix: u8) -> String {
    format!("{prefix:02x}")
}

fn objects_dir(git_dir: &Path) -> PathBuf {
    git_dir.join("objects")
}

fn require_real_directory(path: &Path, operation: &'static str) -> Result<(), StoreError> {
    let meta = fs::symlink_metadata(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            StoreError::Corruption {
                detail: format!("'{path}' is missing", path = path.display()),
            }
        } else {
            StoreError::Io {
                operation,
                path: path.to_path_buf(),
                source,
            }
        }
    })?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(StoreError::Corruption {
            detail: format!("'{path}' is not a directory", path = path.display()),
        });
    }
    Ok(())
}

/// Create `objects/00`..=`objects/ff` in a not-yet-published repository.
///
/// First-create calls this before writing objects so steady-state publication
/// never adds a fanout directory entry and therefore never needs to fsync
/// top-level `objects/`.
pub fn precreate_loose_object_fanout(git_dir: &Path) -> Result<(), StoreError> {
    let objects = objects_dir(git_dir);
    require_real_directory(&objects, "inspecting objects directory")?;
    for prefix in 0u8..=255 {
        let dir = objects.join(loose_object_fanout_hex(prefix));
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                require_real_directory(&dir, "inspecting existing loose-object fanout")?;
            }
            Err(source) => {
                return Err(StoreError::Io {
                    operation: "creating loose-object fanout",
                    path: dir,
                    source,
                });
            }
        }
    }
    Ok(())
}

/// Strict-open check: every fanout directory exists and is a real directory.
/// Missing or malformed topology is [`StoreError::Corruption`]. This never
/// creates or repairs a directory.
pub fn validate_loose_object_fanout(git_dir: &Path) -> Result<(), StoreError> {
    let objects = objects_dir(git_dir);
    require_real_directory(&objects, "inspecting objects directory")?;
    for prefix in 0u8..=255 {
        let dir = objects.join(loose_object_fanout_hex(prefix));
        require_real_directory(&dir, "inspecting loose-object fanout")?;
    }
    Ok(())
}

/// The on-disk path of a loose ref inside a git directory.
pub fn loose_ref_path(git_dir: &Path, ref_name: &str) -> PathBuf {
    git_dir.join(ref_name)
}

/// Durability barrier for a loose-object + loose-ref publication.
///
/// First-create publishes a complete `objects/00`..=`objects/ff` fanout.
/// Steady-state object barriers therefore fsync only each newly installed
/// object file and that object's exact fanout directory. They do not fsync
/// top-level `objects/` or the git directory: no fanout directory entry is
/// being added or removed. A ref-only barrier fsyncs the ref file and every
/// ancestor directory whose entries may have changed, including the git
/// directory, and does not touch the object namespace.
///
/// Missing object/ref files are tolerated (an object that already existed is
/// not rewritten).
pub fn fsync_publication(
    git_dir: &Path,
    written_oids: &[gix::ObjectId],
    written_refs: &[&str],
) -> Result<(), StoreError> {
    if !written_oids.is_empty() {
        crate::fault::object_fsync(git_dir)?;
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push_dir = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    let objects = objects_dir(git_dir);
    for oid in written_oids {
        let path = loose_object_path(git_dir, *oid);
        if path.exists() {
            fsync_file(&path)?;
        }
        if let Some(fanout) = path.parent()
            && fanout != git_dir
            && fanout != objects.as_path()
        {
            push_dir(fanout.to_path_buf());
        }
    }

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
            if d != objects.as_path() && !d.starts_with(&objects) {
                push_dir(d.to_path_buf());
            }
            dir = d.parent();
        }
    }
    if !written_refs.is_empty() {
        push_dir(git_dir.to_path_buf());
        // The ref edit has already been accepted by the caller at this point;
        // a failure here is therefore a published-but-not-durable result.
        crate::fault::ref_fsync(git_dir)?;
    }

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
    use crate::fault;

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
    fn object_publication_syncs_file_and_fanout_not_objects_dir() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("r.git");
        let oid = gix::ObjectId::from_hex(b"0123456789abcdef0123456789abcdef01234567").unwrap();
        let object = loose_object_path(&git, oid);
        let fanout = object.parent().unwrap().to_path_buf();
        fs::create_dir_all(&fanout).unwrap();
        fs::write(&object, b"blob").unwrap();
        fault::reset_fsync_trace();
        fsync_publication(&git, &[oid], &[]).unwrap();
        let synced = fault::synced_paths();
        assert!(
            synced.iter().any(|p| p == &object),
            "object file must be synced: {synced:?}"
        );
        assert!(
            synced.iter().any(|p| p == &fanout),
            "exact fanout directory must be synced: {synced:?}"
        );
        assert!(
            !synced.iter().any(|p| p == &git.join("objects")),
            "steady-state object publication must not fsync objects/: {synced:?}"
        );
        assert!(
            !synced.iter().any(|p| p == &git),
            "object-only publication must not fsync the git directory: {synced:?}"
        );
    }

    #[test]
    fn ref_only_publication_does_not_sync_object_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join("r.git");
        fs::create_dir_all(git.join("objects/01")).unwrap();
        fs::create_dir_all(git.join("refs/lattice")).unwrap();
        let ref_path = git.join("refs/lattice/meta");
        fs::write(&ref_path, b"0123456789abcdef0123456789abcdef01234567\n").unwrap();
        fault::reset_fsync_trace();
        fsync_publication(&git, &[], &["refs/lattice/meta"]).unwrap();
        let synced = fault::synced_paths();
        assert!(
            synced.iter().any(|p| p == &ref_path),
            "ref file must be synced: {synced:?}"
        );
        let objects = git.join("objects");
        assert!(
            !synced
                .iter()
                .any(|p| p == &objects || p.starts_with(&objects)),
            "ref-only barrier must not touch the object namespace: {synced:?}"
        );
    }
}
