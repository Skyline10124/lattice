// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Host-side git2 helpers for baseline-commit export.
//!
//! These functions use `git2` only — they never shell out to a `git` subprocess.
//! Used by both `MicrosandboxWorkspace::deliver_baseline` and
//! `DeterministicWorkspace::create_upper`.

use std::path::Path;

use crate::{Result, WorkspaceError};

/// Export the `baseline_oid` tree from `host_repo_path` to a fresh host tempdir.
///
/// The returned [`tempfile::TempDir`] owns the directory and deletes it on drop,
/// preventing temp-directory leaks after the baseline is delivered into the Upper.
///
/// Uses `git2` only — never shells out to a `git archive` subprocess.
pub fn git_archive_to_tempdir(
    host_repo_path: &Path,
    baseline_oid: &str,
) -> Result<tempfile::TempDir> {
    let tmp = tempfile::TempDir::new().map_err(WorkspaceError::Io)?;
    checkout_tree_to_dir(host_repo_path, baseline_oid, tmp.path())?;
    Ok(tmp)
}

/// Checkout the tree of `baseline_oid` from `host_repo_path` into `dest`.
///
/// Does not preserve `.git` metadata; writes only the working-tree files.
/// Maps all `git2` errors to [`WorkspaceError::Git`].
pub fn checkout_tree_to_dir(host_repo_path: &Path, baseline_oid: &str, dest: &Path) -> Result<()> {
    let repo =
        git2::Repository::open(host_repo_path).map_err(|e| WorkspaceError::Git(e.to_string()))?;

    let commit = repo
        .revparse_single(baseline_oid)
        .map_err(|e| WorkspaceError::Git(format!("revparse {baseline_oid}: {e}")))?
        .peel_to_commit()
        .map_err(|e| WorkspaceError::Git(format!("peel to commit: {e}")))?;

    let tree = commit
        .tree()
        .map_err(|e| WorkspaceError::Git(format!("get tree: {e}")))?;

    checkout_tree_walk(&repo, &tree, dest, "")
}

/// Recursively walk a `git2::Tree` and write all blobs to `dest`.
fn checkout_tree_walk(
    repo: &git2::Repository,
    tree: &git2::Tree<'_>,
    dest: &Path,
    prefix: &str,
) -> Result<()> {
    for entry in tree.iter() {
        let name = entry
            .name()
            .ok_or_else(|| WorkspaceError::Git("non-UTF-8 tree entry name".into()))?;

        let entry_path = if prefix.is_empty() {
            dest.join(name)
        } else {
            dest.join(prefix).join(name)
        };

        match entry.kind() {
            Some(git2::ObjectType::Blob) => {
                let obj = entry
                    .to_object(repo)
                    .map_err(|e| WorkspaceError::Git(e.to_string()))?;
                let blob = obj
                    .as_blob()
                    .ok_or_else(|| WorkspaceError::Git("expected blob".into()))?;

                if let Some(parent) = entry_path.parent() {
                    std::fs::create_dir_all(parent).map_err(WorkspaceError::Io)?;
                }

                // Git stores symlinks as blobs with filemode 0o120000; the blob
                // content is the UTF-8 symlink target path.
                if entry.filemode() == 0o120000 {
                    let target = std::str::from_utf8(blob.content())
                        .map_err(|_| WorkspaceError::Git("non-UTF-8 symlink target".into()))?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::symlink;
                        symlink(target, &entry_path).map_err(WorkspaceError::Io)?;
                    }
                    #[cfg(not(unix))]
                    {
                        return Err(WorkspaceError::Git(
                            "symlinks are not supported on this platform".into(),
                        ));
                    }
                } else {
                    std::fs::write(&entry_path, blob.content()).map_err(WorkspaceError::Io)?;

                    // Preserve executable bit from the tree mode.
                    #[cfg(unix)]
                    if entry.filemode() == 0o100755 {
                        use std::os::unix::fs::PermissionsExt;
                        let mut perms = std::fs::metadata(&entry_path)
                            .map_err(WorkspaceError::Io)?
                            .permissions();
                        perms.set_mode(0o755);
                        std::fs::set_permissions(&entry_path, perms).map_err(WorkspaceError::Io)?;
                    }
                }
            }
            Some(git2::ObjectType::Tree) => {
                let sub_prefix = if prefix.is_empty() {
                    name.to_string()
                } else {
                    format!("{prefix}/{name}")
                };
                std::fs::create_dir_all(&entry_path).map_err(WorkspaceError::Io)?;
                let subtree_obj = entry
                    .to_object(repo)
                    .map_err(|e| WorkspaceError::Git(e.to_string()))?;
                let subtree = subtree_obj
                    .as_tree()
                    .ok_or_else(|| WorkspaceError::Git("expected tree".into()))?;
                checkout_tree_walk(repo, subtree, dest, &sub_prefix)?;
            }
            // Submodules are not materialised into the Upper baseline.
            _ => {}
        }
    }
    Ok(())
}
