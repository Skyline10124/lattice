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
//!    directory or symlink where a file was recorded, a path component the
//!    user replaced with a file, or a path that cannot be read at all —
//!    makes the whole Rewind report a structured conflict. A path that
//!    cannot be verified is never written: that is the same rule, not an
//!    exception to it. Nothing is written.
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

use crate::history::{RewindPathRestore, WorkspaceHistory};
use lancet_authority::under_system_exclusion;

use crate::workspace::content_hash;

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
    ///
    /// Phase 2 writes one path at a time, so a failure part-way leaves the
    /// earlier paths restored. `restored` carries exactly those, because a
    /// half-restored workspace is the state an operator most needs recorded.
    #[error("rewind restore io on '{path}': {detail}")]
    Io {
        /// The path the failure occurred on.
        path: String,
        /// Normalized failure description.
        detail: String,
        /// Paths already restored when the failure hit.
        restored: Vec<RestoredPath>,
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
        // The parse must not rename the path. `WorkspacePath::parse` trims
        // its input, so a recorded path whose first or last character is
        // whitespace would otherwise be verified and written under a
        // *different* name — touching a file LATTICE may never have mutated
        // while leaving the real one unverified. Refuse instead.
        if path.as_str() != restore.path {
            return Err(RewindRestoreError::Path {
                path: restore.path.clone(),
                reason: format!(
                    "the recorded path does not round-trip through workspace path resolution \
                     (resolves to '{}')",
                    path.as_str()
                ),
            });
        }
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
        let observed = observe(&workspace, path);
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
            Observed::Unverifiable(reason) => (None, Some(reason.clone())),
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
                let installed = match workspace.install_atomic_sync(path, bytes) {
                    Ok(installed) => installed,
                    Err(err) => {
                        return Err(RewindRestoreError::Io {
                            path: path.as_str().to_string(),
                            detail: err.to_string(),
                            restored,
                        });
                    }
                };
                restored.push(RestoredPath {
                    path: path.as_str().to_string(),
                    restored_revision: Some(installed.content_hash),
                });
            }
            None => {
                // Preflight proved the target state; a path that is already
                // absent is already restored, so deletion is idempotent.
                match workspace.remove_file_sync(path) {
                    Ok(()) | Err(LocalWorkspaceError::NotFound { .. }) => {}
                    Err(err) => {
                        return Err(RewindRestoreError::Io {
                            path: path.as_str().to_string(),
                            detail: err.to_string(),
                            restored,
                        });
                    }
                }
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
    /// The recorded regular file could not be observed at all — a path
    /// component is no longer a directory, the path is unreadable, or
    /// resolution refused it.
    Unverifiable(String),
}

/// Observe one affected path.
///
/// Every outcome is a *state*, never an executor failure: a path that cannot
/// be read is a path that cannot be verified, and the verify-before-write
/// rule makes that a conflict. Reporting it as an error instead would hide it
/// among the other affected paths and give the user nothing to act on.
fn observe(workspace: &LocalWorkspace, path: &WorkspacePath) -> Observed {
    let (full, facts) = match workspace.resolve_secure(path, PathIntent::Read) {
        Ok(resolved) => resolved,
        // A symlink anywhere in the recorded path is a divergence from the
        // recorded regular-file state.
        Err(LocalWorkspaceError::SymlinkRejected { .. }) => {
            return Observed::NotAFile(PathKind::Symlink);
        }
        Err(LocalWorkspaceError::Io(err)) if err.kind() == std::io::ErrorKind::NotADirectory => {
            return Observed::Unverifiable("a path component is no longer a directory".to_string());
        }
        Err(err) => {
            return Observed::Unverifiable(format!("the path could not be resolved: {err}"));
        }
    };
    if !facts.exists {
        return Observed::Absent;
    }
    match facts.kind {
        Some(PathKind::File) => {
            // `PathKind::File` means "neither directory nor symlink", which
            // also covers FIFOs, sockets, and device nodes. Reading a FIFO
            // blocks until a writer appears — forever, while this thread
            // holds the history's recording lock — so confirm a regular file
            // before opening it.
            match std::fs::symlink_metadata(&full) {
                Ok(meta) if meta.file_type().is_file() => match std::fs::read(&full) {
                    Ok(bytes) => Observed::File(bytes),
                    Err(err) => {
                        Observed::Unverifiable(format!("the file could not be read: {err}"))
                    }
                },
                Ok(_) => Observed::Unverifiable(
                    "a special file sits where a regular file was recorded".to_string(),
                ),
                Err(err) => {
                    Observed::Unverifiable(format!("the path could not be inspected: {err}"))
                }
            }
        }
        Some(other) => Observed::NotAFile(other),
        None => Observed::Absent,
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

    /// A recorded path that workspace path resolution would rename must be
    /// refused outright: verifying and writing the renamed path would touch a
    /// file LATTICE may never have mutated and leave the real one unverified.
    #[test]
    fn a_recorded_path_that_does_not_round_trip_is_refused() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "user content").unwrap();

        let err = preflight_and_restore(dir.path(), &[restore(" a.txt", Some("v2"), Some("v1"))])
            .expect_err("a path that resolution would rename must be refused");
        assert!(
            matches!(err, RewindRestoreError::Path { .. }),
            "got {err:?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).unwrap(),
            b"user content",
            "the similarly-named user file must be untouched"
        );
    }

    /// When phase 2 fails part-way, the error must carry the paths it had
    /// already written — a half-restored workspace is the state an operator
    /// most needs recorded.
    #[test]
    fn a_partial_restore_reports_the_paths_it_already_wrote() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a2").unwrap();
        // A read-only directory: preflight sees an absent path (no conflict),
        // and phase 2 cannot write into it.
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&ro, perms).unwrap();

        let err = preflight_and_restore(
            dir.path(),
            &[
                restore("a.txt", Some("a2"), Some("a1")),
                restore("ro/blocked.txt", None, Some("nope")),
            ],
        );

        // Restore permissions so the TempDir can clean up.
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(&ro, perms);

        let Err(RewindRestoreError::Io { path, restored, .. }) = err else {
            panic!("expected a phase-2 io failure, got {err:?}");
        };
        assert_eq!(path, "ro/blocked.txt");
        assert_eq!(
            restored,
            vec![RestoredPath {
                path: "a.txt".to_string(),
                restored_revision: Some(content_hash(b"a1")),
            }],
            "the error names exactly the paths already written"
        );
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).unwrap(),
            b"a1",
            "the first path really was written"
        );
    }

    /// A FIFO at an affected path must conflict, not block the Rewind (and
    /// with it the recording lock) inside `open(2)` waiting for a writer.
    #[test]
    fn a_fifo_at_an_affected_path_conflicts_instead_of_blocking() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("piped.txt");
        let c_path = std::ffi::CString::new(target.to_str().unwrap()).unwrap();
        // SAFETY: `c_path` is a valid NUL-terminated path for the duration of
        // the call; mkfifo only creates a filesystem entry.
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let outcome =
            preflight_and_restore(dir.path(), &[restore("piped.txt", Some("v2"), Some("v1"))])
                .expect("a FIFO is a conflict, not an executor error");
        let RewindRestoreOutcome::Conflict { conflicts, .. } = outcome else {
            panic!("expected Conflict, got {outcome:?}");
        };
        assert_eq!(conflicts.len(), 1);
        assert!(
            conflicts[0].detail.contains("special file"),
            "detail: {}",
            conflicts[0].detail
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

    /// Restoring "absent" onto a path that is already absent is the goal
    /// state, not a failure.
    #[test]
    fn deleting_an_already_absent_path_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let outcome = preflight_and_restore(dir.path(), &[restore("missing.txt", None, None)])
            .expect("an already-absent path is not an error");
        let RewindRestoreOutcome::Applied { restored, .. } = outcome else {
            panic!("expected Applied, got {outcome:?}");
        };
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].restored_revision, None);
    }

    /// A path component the user replaced with a regular file, or a file that
    /// cannot be read, is a same-path divergence: it must be reported as a
    /// structured conflict alongside the other affected paths, not as an
    /// opaque executor error that hides which path is at fault.
    #[test]
    fn an_unverifiable_path_conflicts_rather_than_erroring() {
        let dir = TempDir::new().unwrap();
        // LATTICE recorded "held/deep.txt"; the user replaced the directory
        // `held` with a regular file.
        std::fs::write(dir.path().join("held"), "user replaced the directory").unwrap();
        std::fs::write(dir.path().join("fine.txt"), "v2").unwrap();

        let outcome = preflight_and_restore(
            dir.path(),
            &[
                restore("fine.txt", Some("v2"), Some("v1")),
                restore("held/deep.txt", Some("d2"), Some("d1")),
            ],
        )
        .expect("an unverifiable path is a conflict, not an executor error");
        let RewindRestoreOutcome::Conflict {
            preflight,
            conflicts,
        } = outcome
        else {
            panic!("expected Conflict, got {outcome:?}");
        };
        assert_eq!(preflight.len(), 2, "every path is still preflighted");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "held/deep.txt");
        assert!(
            conflicts[0].detail.contains("directory"),
            "detail names the divergence: {}",
            conflicts[0].detail
        );
        assert_eq!(
            std::fs::read(dir.path().join("fine.txt")).unwrap(),
            b"v2",
            "the matching path is left alone"
        );
    }
}
