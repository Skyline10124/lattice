// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Explicit Rewind restore executor (GitHub #64).
//!
//! Applies a [`lancet_runtime::RewindPlan`]'s per-path restores to the Local
//! Workspace. This is a host-side operator surface, not a CEP method: the
//! catalog gains no `workspace.rewind`, and the executor records nothing into
//! the private history — reversing a mutation must never append compensation
//! commits.
//!
//! Two strict phases:
//!
//! 1. **Preflight** — every affected path is parsed, checked against System
//!    Path Exclusion, resolved through the same symlink-rejecting secure walk
//!    the mutation path uses, and its on-disk bytes compared against the
//!    recorded post-state. Any divergence — changed content, deletion, a
//!    directory or symlink where a file was recorded — makes the whole
//!    Rewind report a structured conflict. Nothing is written.
//! 2. **Restore** — only after every path verified: recorded parent-tree
//!    bytes are installed atomically (same-directory tempfile + fsync +
//!    rename), and a reversed `mutate.create` deletes its file through the
//!    same secure resolution.
//!
//! Paths never touched by LATTICE are never read or written here — the
//! restore set is exactly the affected paths of the reversed commits.
//!
//! Directories a `mutate.create` created on the way to its target are
//! deliberately *not* removed. The history records file blobs, so it cannot
//! know which directories were LATTICE's, and a directory that looks empty of
//! LATTICE content may hold files the user put there since — removing it
//! would destroy exactly the external changes a Rewind must preserve. An
//! empty directory left behind is the safe residue.

use std::path::Path;
use std::sync::Arc;

use lancet_authority::under_system_exclusion;
use lancet_runtime::{RewindPathRestore, WorkspaceHistory};

use crate::edit::content_hash;

use super::local::LocalWorkspace;
use super::types::{LocalWorkspaceError, PathIntent, PathKind, WorkspacePath};

/// What preflight observed for one affected path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindPathPreflight {
    /// Workspace-relative path.
    pub path: String,
    /// Recorded post-state Local Revision (`None` = recorded as absent).
    pub expected_revision: Option<String>,
    /// Observed on-disk Local Revision (`None` = absent or not a file).
    pub observed_revision: Option<String>,
    /// Whether the observation diverged from the recorded post-state.
    pub diverged: bool,
}

/// One path whose current Local Revision no longer matches the recorded
/// post-mutation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewindPathConflict {
    /// Workspace-relative path.
    pub path: String,
    /// Recorded post-state Local Revision (`None` = recorded as absent).
    pub expected_revision: Option<String>,
    /// Observed Local Revision (`None` = absent or not a regular file).
    pub observed_revision: Option<String>,
    /// Human-readable divergence description. Never file content.
    pub detail: String,
}

/// One path the restore phase wrote or removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoredPath {
    /// Workspace-relative path.
    pub path: String,
    /// Local Revision after the restore, or `None` when the file was removed.
    pub restored_revision: Option<String>,
}

/// Result of one preflight-and-restore pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewindRestoreOutcome {
    /// Every path matched its recorded post-state and was restored.
    Applied {
        /// Preflight observations, one per affected path.
        preflight: Vec<RewindPathPreflight>,
        /// Applied restores, one per affected path.
        restored: Vec<RestoredPath>,
    },
    /// At least one path diverged; nothing was written.
    Conflict {
        /// Preflight observations, one per affected path.
        preflight: Vec<RewindPathPreflight>,
        /// The divergent paths.
        conflicts: Vec<RewindPathConflict>,
    },
}

/// Hard failure of the restore executor (not a same-path conflict).
#[derive(Debug, thiserror::Error)]
pub enum RewindRestoreError {
    /// A recorded path failed workspace path validation.
    #[error("rewind path '{path}' rejected: {reason}")]
    Path {
        /// The rejected path as recorded.
        path: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A recorded path fell under System Path Exclusion.
    #[error("rewind path '{path}' is under a System Path Exclusion")]
    Excluded {
        /// The excluded path.
        path: String,
    },
    /// Reading or writing the Local Workspace failed.
    #[error("rewind restore io on '{path}': {detail}")]
    Io {
        /// The path the failure occurred on.
        path: String,
        /// Normalized failure description.
        detail: String,
    },
}

/// Preflight every affected path, then restore all of them.
///
/// `project_root` is the canonical Local Workspace root. See the module doc
/// for the two-phase contract. The executor never touches paths outside
/// `restores`, never records history, and never follows symlinks.
pub fn preflight_and_restore(
    project_root: &Path,
    restores: &[RewindPathRestore],
) -> Result<RewindRestoreOutcome, RewindRestoreError> {
    let workspace =
        LocalWorkspace::with_history(project_root, Arc::new(WorkspaceHistory::disabled()));

    // Phase 1: parse + exclusion + secure resolve + byte compare, all paths.
    let mut parsed: Vec<(WorkspacePath, &RewindPathRestore)> = Vec::with_capacity(restores.len());
    for restore in restores {
        let path = WorkspacePath::parse(&restore.path).map_err(|err| RewindRestoreError::Path {
            path: restore.path.clone(),
            reason: err.to_string(),
        })?;
        if under_system_exclusion(path.as_str()) {
            return Err(RewindRestoreError::Excluded {
                path: restore.path.clone(),
            });
        }
        parsed.push((path, restore));
    }

    let mut preflight = Vec::with_capacity(parsed.len());
    let mut conflicts = Vec::new();
    for (path, restore) in &parsed {
        let expected_revision = restore.expected.as_deref().map(content_hash);
        let observed = observe(&workspace, path)?;
        let (observed_revision, detail) = match &observed {
            Observed::File(bytes) => (
                Some(content_hash(bytes)),
                if restore.expected.as_deref() == Some(bytes.as_slice()) {
                    None
                } else if restore.expected.is_none() {
                    Some("a file exists where the recorded state is absence".to_string())
                } else {
                    Some("content diverged from the recorded post-state".to_string())
                },
            ),
            Observed::Absent => (
                None,
                restore
                    .expected
                    .is_some()
                    .then(|| "the file is gone; the recorded post-state is content".to_string()),
            ),
            Observed::NotAFile(kind) => (
                None,
                Some(format!(
                    "a {} sits where a regular file was recorded",
                    kind_label(*kind)
                )),
            ),
        };
        let diverged = detail.is_some();
        preflight.push(RewindPathPreflight {
            path: path.as_str().to_string(),
            expected_revision: expected_revision.clone(),
            observed_revision: observed_revision.clone(),
            diverged,
        });
        if let Some(detail) = detail {
            conflicts.push(RewindPathConflict {
                path: path.as_str().to_string(),
                expected_revision,
                observed_revision,
                detail,
            });
        }
    }
    if !conflicts.is_empty() {
        return Ok(RewindRestoreOutcome::Conflict {
            preflight,
            conflicts,
        });
    }

    // Phase 2: every path verified — restore.
    let mut restored = Vec::with_capacity(parsed.len());
    for (path, restore) in &parsed {
        match restore.restore.as_deref() {
            Some(bytes) => {
                let installed = workspace.install_atomic_sync(path, bytes).map_err(|err| {
                    RewindRestoreError::Io {
                        path: path.as_str().to_string(),
                        detail: err.to_string(),
                    }
                })?;
                restored.push(RestoredPath {
                    path: path.as_str().to_string(),
                    restored_revision: Some(installed.content_hash),
                });
            }
            None => {
                workspace
                    .remove_file_sync(path)
                    .map_err(|err| RewindRestoreError::Io {
                        path: path.as_str().to_string(),
                        detail: err.to_string(),
                    })?;
                restored.push(RestoredPath {
                    path: path.as_str().to_string(),
                    restored_revision: None,
                });
            }
        }
    }
    Ok(RewindRestoreOutcome::Applied {
        preflight,
        restored,
    })
}

/// What preflight sees at a path.
enum Observed {
    File(Vec<u8>),
    Absent,
    NotAFile(PathKind),
}

fn observe(
    workspace: &LocalWorkspace,
    path: &WorkspacePath,
) -> Result<Observed, RewindRestoreError> {
    let (full, facts) = match workspace.resolve_secure(path, PathIntent::Read) {
        Ok(resolved) => resolved,
        // A symlink anywhere in the recorded path is a divergence from the
        // recorded regular-file state, not an executor failure.
        Err(LocalWorkspaceError::SymlinkRejected { .. }) => {
            return Ok(Observed::NotAFile(PathKind::Symlink));
        }
        Err(err) => {
            return Err(RewindRestoreError::Io {
                path: path.as_str().to_string(),
                detail: err.to_string(),
            });
        }
    };
    if !facts.exists {
        return Ok(Observed::Absent);
    }
    match facts.kind {
        Some(PathKind::File) => {
            let bytes = std::fs::read(&full).map_err(|err| RewindRestoreError::Io {
                path: path.as_str().to_string(),
                detail: err.to_string(),
            })?;
            Ok(Observed::File(bytes))
        }
        Some(other) => Ok(Observed::NotAFile(other)),
        None => Ok(Observed::Absent),
    }
}

fn kind_label(kind: PathKind) -> &'static str {
    match kind {
        PathKind::File => "file",
        PathKind::Directory => "directory",
        PathKind::Symlink => "symlink",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn restore(path: &str, expected: Option<&str>, restore: Option<&str>) -> RewindPathRestore {
        RewindPathRestore {
            path: path.to_string(),
            expected: expected.map(|s| s.as_bytes().to_vec()),
            restore: restore.map(|s| s.as_bytes().to_vec()),
        }
    }

    #[test]
    fn matching_paths_are_restored_atomically() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("edited.txt"), "v2").unwrap();
        std::fs::write(dir.path().join("created.txt"), "new").unwrap();
        std::fs::write(dir.path().join("unrelated.txt"), "external").unwrap();

        let outcome = preflight_and_restore(
            dir.path(),
            &[
                restore("edited.txt", Some("v2"), Some("v1")),
                restore("created.txt", Some("new"), None),
            ],
        )
        .unwrap();

        let RewindRestoreOutcome::Applied {
            preflight,
            restored,
        } = outcome
        else {
            panic!("expected Applied, got {outcome:?}");
        };
        assert_eq!(preflight.len(), 2);
        assert!(preflight.iter().all(|p| !p.diverged));
        assert_eq!(restored.len(), 2);
        assert_eq!(std::fs::read(dir.path().join("edited.txt")).unwrap(), b"v1");
        assert!(
            !dir.path().join("created.txt").exists(),
            "a reversed create is deleted"
        );
        assert_eq!(
            std::fs::read(dir.path().join("unrelated.txt")).unwrap(),
            b"external",
            "paths outside the plan are untouched"
        );
    }

    /// A reversed create removes its file but leaves the directory it needed,
    /// because the user may have put their own files there since. Removing
    /// the directory would destroy exactly the external changes a Rewind
    /// must preserve.
    #[test]
    fn reversing_a_create_never_removes_a_directory_holding_external_files() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("made/by/lattice")).unwrap();
        std::fs::write(dir.path().join("made/by/lattice/new.txt"), "new").unwrap();
        // The user drops their own file next to LATTICE's.
        std::fs::write(dir.path().join("made/by/lattice/mine.txt"), "user").unwrap();

        let outcome = preflight_and_restore(
            dir.path(),
            &[restore("made/by/lattice/new.txt", Some("new"), None)],
        )
        .unwrap();
        assert!(matches!(outcome, RewindRestoreOutcome::Applied { .. }));
        assert!(!dir.path().join("made/by/lattice/new.txt").exists());
        assert_eq!(
            std::fs::read(dir.path().join("made/by/lattice/mine.txt")).unwrap(),
            b"user",
            "the user's file in a LATTICE-created directory must survive"
        );
        assert!(dir.path().join("made/by/lattice").is_dir());
    }

    #[test]
    fn any_divergence_conflicts_and_writes_nothing() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("ok.txt"), "v2").unwrap();
        std::fs::write(dir.path().join("drifted.txt"), "user-edit").unwrap();

        let outcome = preflight_and_restore(
            dir.path(),
            &[
                restore("ok.txt", Some("v2"), Some("v1")),
                restore("drifted.txt", Some("recorded"), Some("old")),
                restore("gone.txt", Some("recorded"), Some("old")),
            ],
        )
        .unwrap();

        let RewindRestoreOutcome::Conflict {
            preflight,
            conflicts,
        } = outcome
        else {
            panic!("expected Conflict, got {outcome:?}");
        };
        assert_eq!(preflight.len(), 3, "every path is preflighted");
        let paths: Vec<&str> = conflicts.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["drifted.txt", "gone.txt"],
            "every divergent path is reported, not only the first"
        );
        assert!(
            conflicts.iter().all(|c| !c.detail.contains("user-edit")),
            "conflict details never leak file content"
        );
        assert_eq!(
            std::fs::read(dir.path().join("ok.txt")).unwrap(),
            b"v2",
            "a conflict leaves even matching paths unwritten"
        );
        assert_eq!(
            std::fs::read(dir.path().join("drifted.txt")).unwrap(),
            b"user-edit"
        );
    }

    #[test]
    fn a_directory_or_symlink_where_a_file_was_recorded_conflicts() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("was-file.txt")).unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("target"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path().join("target"), dir.path().join("linked.txt"))
            .unwrap();

        let outcome = preflight_and_restore(
            dir.path(),
            &[
                restore("was-file.txt", Some("v"), Some("u")),
                restore("linked.txt", Some("v"), Some("u")),
            ],
        )
        .unwrap();
        let RewindRestoreOutcome::Conflict { conflicts, .. } = outcome else {
            panic!("expected Conflict, got {outcome:?}");
        };
        assert_eq!(conflicts.len(), 2);
        assert!(conflicts[0].detail.contains("directory"));
        assert!(conflicts[1].detail.contains("symlink"));
        assert!(
            dir.path().join("was-file.txt").is_dir(),
            "nothing was written"
        );
    }

    #[test]
    fn expected_absence_with_a_present_file_conflicts() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("ghost.txt"), "resurrected").unwrap();
        let outcome =
            preflight_and_restore(dir.path(), &[restore("ghost.txt", None, Some("old"))]).unwrap();
        assert!(
            matches!(outcome, RewindRestoreOutcome::Conflict { .. }),
            "got {outcome:?}"
        );
    }

    #[test]
    fn system_exclusions_and_invalid_paths_are_hard_errors() {
        let dir = TempDir::new().unwrap();
        let err =
            preflight_and_restore(dir.path(), &[restore(".git/config", Some("v"), Some("u"))])
                .unwrap_err();
        assert!(matches!(err, RewindRestoreError::Excluded { .. }));

        let err = preflight_and_restore(
            dir.path(),
            &[restore(".lattice/state.toml", Some("v"), Some("u"))],
        )
        .unwrap_err();
        assert!(matches!(err, RewindRestoreError::Excluded { .. }));

        let err = preflight_and_restore(
            dir.path(),
            &[restore("../escape.txt", Some("v"), Some("u"))],
        )
        .unwrap_err();
        assert!(matches!(err, RewindRestoreError::Path { .. }));
    }

    #[test]
    fn restore_failure_surfaces_as_io_error() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), "v2").unwrap();
        // Deleting the file between preflight and restore is an executor
        // failure (the in-process window has no other writers), not a
        // conflict — prove it surfaces rather than reporting success.
        // Simulate by expecting absence for a path we then try to delete.
        let outcome = preflight_and_restore(dir.path(), &[restore("missing.txt", None, None)]);
        let err = outcome.unwrap_err();
        assert!(matches!(err, RewindRestoreError::Io { .. }), "got {err:?}");
    }
}
