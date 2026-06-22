// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Host-side git helpers for baseline commit export and exact object provisioning.
//!
//! Tree checkout helpers use `git2` only — they never shell out to a `git` subprocess.
//! Pack construction and exact-baseline ingestion use typed `git` CLI argv (no shell).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::{Result, WorkspaceError};

/// Maximum host/guest pack transfer chunk before base64 encoding (64 KiB).
///
/// The base64-encoded chunk is passed as a single argv element to `sh` inside
/// the Upper VM.  Linux `MAX_ARG_STRLEN` is 128 KiB per argument; 64 KiB raw
/// produces ~87 KiB base64, safely within that limit.
pub const BASELINE_TRANSFER_CHUNK_BYTES: usize = 64 * 1024;

/// Maximum host/guest candidate bundle transfer chunk before base64 encoding (64 KiB).
///
/// Same constraint as [`BASELINE_TRANSFER_CHUNK_BYTES`] — the base64 string is
/// passed through `exec_with("sh", ...)` and must stay under `MAX_ARG_STRLEN`.
pub const CANDIDATE_EXPORT_CHUNK_BYTES: usize = 64 * 1024;

/// Temporary export ref prefix removed on every exit path.
const CANDIDATE_EXPORT_REF_PREFIX: &str = "refs/lattice/export/candidate-";

/// Branch ref attached to Upper `HEAD` after exact baseline provisioning.
pub const LATTICE_UPPER_REF: &str = "refs/heads/lattice-upper";

/// Build a pack containing `baseline_oid` and all reachable objects from `host_repo_path`.
///
/// Spawns `git pack-objects --stdout --revs` with the OID on stdin; streams stdout
/// into a synced host tempfile. Never invokes a shell.
pub fn build_baseline_pack(
    host_repo_path: &Path,
    baseline_oid: &str,
) -> Result<tempfile::NamedTempFile> {
    crate::validate_baseline_oid(baseline_oid)?;

    let repo = host_repo_path
        .to_str()
        .ok_or_else(|| WorkspaceError::Git("non-UTF-8 host repo path".into()))?;

    let mut child = Command::new("git")
        .args(["-C", repo, "pack-objects", "--stdout", "--revs"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(WorkspaceError::Io)?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(baseline_oid.as_bytes())
            .map_err(WorkspaceError::Io)?;
        stdin.write_all(b"\n").map_err(WorkspaceError::Io)?;
    }

    let output = child.wait_with_output().map_err(WorkspaceError::Io)?;
    if !output.status.success() {
        return Err(WorkspaceError::Git(format!(
            "pack-objects failed for {baseline_oid}: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let mut pack_file = tempfile::NamedTempFile::new().map_err(WorkspaceError::Io)?;
    pack_file
        .write_all(&output.stdout)
        .map_err(WorkspaceError::Io)?;
    pack_file.flush().map_err(WorkspaceError::Io)?;
    pack_file.as_file().sync_all().map_err(WorkspaceError::Io)?;
    Ok(pack_file)
}

/// Initialize `repo_path`, ingest `pack_path`, attach `HEAD` to [`LATTICE_UPPER_REF`],
/// check out the exact `baseline_oid`, and verify resulting `HEAD`.
pub fn provision_exact_baseline(
    repo_path: &Path,
    pack_path: &Path,
    baseline_oid: &str,
) -> Result<()> {
    crate::validate_baseline_oid(baseline_oid)?;

    run_git(repo_path, ["init"], "git init")?;

    let pack_bytes = std::fs::read(pack_path).map_err(WorkspaceError::Io)?;
    let mut index_pack = Command::new("git")
        .args(["index-pack", "--stdin"])
        .current_dir(repo_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(WorkspaceError::Io)?;

    if let Some(mut stdin) = index_pack.stdin.take() {
        stdin.write_all(&pack_bytes).map_err(WorkspaceError::Io)?;
    }

    let index_out = index_pack.wait_with_output().map_err(WorkspaceError::Io)?;
    if !index_out.status.success() {
        return Err(WorkspaceError::Git(format!(
            "index-pack failed: {}",
            String::from_utf8_lossy(&index_out.stderr)
        )));
    }

    run_git(
        repo_path,
        ["update-ref", LATTICE_UPPER_REF, baseline_oid],
        "update-ref lattice-upper",
    )?;
    run_git(
        repo_path,
        ["symbolic-ref", "HEAD", LATTICE_UPPER_REF],
        "symbolic-ref HEAD",
    )?;
    run_git(
        repo_path,
        ["checkout", "-f", baseline_oid],
        "checkout baseline",
    )?;

    let head_out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .map_err(WorkspaceError::Io)?;
    if !head_out.status.success() {
        return Err(WorkspaceError::Git(format!(
            "rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&head_out.stderr)
        )));
    }

    let actual = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
    if actual != baseline_oid {
        return Err(WorkspaceError::BaselineIdentityMismatch {
            expected: baseline_oid.to_string(),
            actual,
        });
    }

    Ok(())
}

/// Export a clean incremental candidate bundle from an on-disk Git repository.
///
/// Uses typed `git` argv only (no shell). Creates and removes a temporary export ref
/// on every exit path. Never writes bundle bytes to process stdout.
pub fn export_candidate_from_repo(
    repo_path: &Path,
    baseline_oid: &str,
    max_artifact_bytes: u64,
) -> Result<crate::CandidateExport> {
    crate::validate_baseline_oid(baseline_oid)?;

    let repo = repo_path
        .to_str()
        .ok_or_else(|| WorkspaceError::Git("non-UTF-8 repo path".into()))?;

    let status = Command::new("git")
        .args([
            "-C",
            repo,
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ])
        .output()
        .map_err(WorkspaceError::Io)?;
    if !status.status.success() {
        return Err(WorkspaceError::Git(format!(
            "status failed: {}",
            String::from_utf8_lossy(&status.stderr)
        )));
    }
    if !status.stdout.is_empty() {
        // Parse through strict NUL-delimited parser (fail-closed on non-UTF-8).
        let git_status = crate::parse_git_status_z(&status.stdout)
            .map_err(|e| WorkspaceError::Git(format!("status parse failed: {e}")))?;
        let summary = crate::git_status::GitStatusEntry::workspace_dirty_fact(&git_status.entries);
        return Err(WorkspaceError::UpperDirty { summary });
    }

    let candidate_oid = git_rev_parse(repo_path, &["rev-parse", "HEAD"], "rev-parse HEAD")?;
    crate::validate_baseline_oid(&candidate_oid)?;

    if !git_is_ancestor(repo_path, baseline_oid, &candidate_oid)? {
        return Err(WorkspaceError::NotDescendant {
            baseline: baseline_oid.to_owned(),
            candidate: candidate_oid.clone(),
        });
    }

    let export_ref = format!(
        "{CANDIDATE_EXPORT_REF_PREFIX}{}",
        uuid::Uuid::new_v4().simple()
    );
    let _ref_guard = ExportRefGuard {
        repo_path: repo_path.to_path_buf(),
        export_ref: export_ref.clone(),
    };

    run_git(
        repo_path,
        ["update-ref", &export_ref, &candidate_oid],
        "update-ref export ref",
    )?;

    let scratch = tempfile::TempDir::new().map_err(WorkspaceError::Io)?;
    let bundle_path = scratch.path().join("candidate.bundle");

    let range = format!("{baseline_oid}..{export_ref}");
    run_git(
        repo_path,
        [
            "bundle",
            "create",
            bundle_path.to_str().expect("utf8 bundle path"),
            &range,
        ],
        "bundle create",
    )?;

    let metadata = std::fs::metadata(&bundle_path).map_err(WorkspaceError::Io)?;
    let artifact_len = metadata.len();
    if artifact_len > max_artifact_bytes {
        return Err(WorkspaceError::ArtifactTooLarge {
            actual: artifact_len,
            max: max_artifact_bytes,
        });
    }

    let mut bundle_file = tempfile::NamedTempFile::new().map_err(WorkspaceError::Io)?;
    std::fs::copy(&bundle_path, bundle_file.path()).map_err(WorkspaceError::Io)?;
    bundle_file.flush().map_err(WorkspaceError::Io)?;
    bundle_file
        .as_file()
        .sync_all()
        .map_err(WorkspaceError::Io)?;

    Ok(crate::CandidateExport::from_parts(
        bundle_file,
        baseline_oid.to_owned(),
        candidate_oid,
        artifact_len,
    ))
}

struct ExportRefGuard {
    repo_path: PathBuf,
    export_ref: String,
}

impl Drop for ExportRefGuard {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .arg("-C")
            .arg(&self.repo_path)
            .args(["update-ref", "-d", &self.export_ref])
            .output();
    }
}

fn git_rev_parse(repo_path: &Path, args: &[&str], ctx: &str) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(args)
        .output()
        .map_err(WorkspaceError::Io)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(WorkspaceError::Git(format!(
            "{ctx} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn git_is_ancestor(repo_path: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map_err(WorkspaceError::Io)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(WorkspaceError::Git(format!(
            "merge-base --is-ancestor failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ))),
    }
}

fn run_git(
    repo_path: &Path,
    args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
    ctx: &str,
) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_path)
        .output()
        .map_err(WorkspaceError::Io)?;
    if !output.status.success() {
        return Err(WorkspaceError::Git(format!(
            "{ctx} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

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
