// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete Local Workspace implementation.
//!
//! See [`super`] for the module-level contract. The single concrete type
//! [`LocalWorkspace`] owns secure path resolution, bounded reads, content
//! hashing, no-replace create, atomic edit installation, and filesystem
//! error normalization. It is constructed from a canonical project root
//! and never re-canonicalizes after construction.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
#[cfg(any(test, feature = "test-fault-injection"))]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};

use lancet_runtime::{MutationRecord, RecordedMutation, WorkspaceHistory, WorkspaceHistoryError};
use regex::Regex;
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use crate::edit::content_hash;

use super::types::{
    CreatedFile, DirectoryEntry, FindRequest, GrepMatch, GrepRequest, InstalledFile,
    LocalWorkspaceError, PathFacts, PathIntent, PathKind, WorkspacePath,
};
use super::visibility::{VisibilityEngine, VisibilityEvidence};

/// The single concrete Local Workspace (ADR 0003 decision 3).
///
/// Internal deep module: secure path resolution, bounded read, atomic
/// installation, mutation history, and error normalization all live behind
/// this type. It is `pub(crate)` because native handlers and the edit engine
/// are the only allowed callers; there is no workspace-backend seam.
pub(crate) struct LocalWorkspace {
    /// Canonical project root. Captured at construction time; never mutates.
    root: PathBuf,
    /// Private durable history of successfully installed Atomic Local
    /// Mutations (GitHub #63). Lives in its own bare `gix` repository outside
    /// this workspace and outside the project `.git`; recording never touches
    /// workspace content.
    history: Arc<WorkspaceHistory>,
    /// One-shot install-failure flag for the narrow fault-injection seam
    /// (`fail_next_install`). Not present in production builds.
    #[cfg(any(test, feature = "test-fault-injection"))]
    fail_install: Arc<AtomicBool>,
}

impl LocalWorkspace {
    /// Construct from a project root with mutation history recording off.
    ///
    /// Test-only: the production server always attaches the run's private
    /// history through [`LocalWorkspace::with_history`], so an installation
    /// path that records nothing cannot exist in a real turn.
    #[cfg(test)]
    pub(crate) fn new(root: &Path) -> Self {
        Self::with_history(root, Arc::new(WorkspaceHistory::disabled()))
    }

    /// Construct from a project root, canonicalizing it once, with a private
    /// mutation history attached.
    ///
    /// Falls back to the raw path if canonicalize fails (mirrors
    /// `CepPolicyConfig::defaults_for_project_root` tolerance).
    pub(crate) fn with_history(root: &Path, history: Arc<WorkspaceHistory>) -> Self {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Self {
            root: canonical,
            history,
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_install: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Canonical project root this workspace operates on.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Append one successfully installed Atomic Local Mutation to the private
    /// history.
    ///
    /// Callers must only reach this after the atomic install returned success:
    /// a recorded commit asserts the mutation is on disk. Returns `Ok(None)`
    /// when history recording is disabled. Recording writes only into the
    /// private history repository — never the Local Workspace and never the
    /// project `.git`.
    pub(crate) async fn record_mutation(
        &self,
        record: MutationRecord,
    ) -> Result<Option<RecordedMutation>, WorkspaceHistoryError> {
        let history = Arc::clone(&self.history);
        tokio::task::spawn_blocking(move || history.record(&record))
            .await
            .unwrap_or_else(|err| {
                Err(WorkspaceHistoryError::Repo(format!(
                    "history recording task failed: {err}"
                )))
            })
    }

    /// Clone the shareable state for a `spawn_blocking` closure.
    fn blocking_clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            history: Arc::clone(&self.history),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_install: Arc::clone(&self.fail_install),
        }
    }

    /// Per-segment secure path resolution.
    ///
    /// Walks every component of `path` beneath `root`, rejecting any segment
    /// that is a symlink (parent or leaf) and any resolution that escapes
    /// the canonical root. Missing intermediate parents are recorded as
    /// `parent_exists = false`; the remaining segments are checked lexically.
    ///
    /// Resolution is intent-independent: the same secure walk backs read,
    /// create, overwrite, delete, and list flows. The intent is carried as
    /// operation vocabulary for the guard/precondition layer.
    fn resolve_secure(
        &self,
        path: &WorkspacePath,
        _intent: PathIntent,
    ) -> Result<(PathBuf, PathFacts), LocalWorkspaceError> {
        if path.is_project_root() {
            let meta = fs::symlink_metadata(&self.root)?;
            let kind = metadata_kind(&meta);
            let byte_size = if meta.is_file() {
                Some(meta.len())
            } else {
                None
            };
            let facts = PathFacts {
                normalized_path: path.as_str().to_string(),
                exists: true,
                kind: Some(kind),
                parent_exists: true,
                parent_kind: Some(PathKind::Directory),
                byte_size,
                canonical_under_workspace: true,
            };
            return Ok((self.root.clone(), facts));
        }

        let mut cur = self.root.clone();
        let mut last_exists = true;
        let mut last_meta: Option<fs::Metadata> = None;
        for comp in Path::new(path.as_str()).components() {
            match comp {
                Component::Normal(part) => {
                    cur.push(part);
                    match fs::symlink_metadata(&cur) {
                        Ok(meta) if meta.file_type().is_symlink() => {
                            return Err(LocalWorkspaceError::SymlinkRejected {
                                component: cur.clone(),
                            });
                        }
                        Ok(meta) => {
                            last_meta = Some(meta);
                            last_exists = true;
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            last_meta = None;
                            last_exists = false;
                            // Remaining segments can only be lexical — keep
                            // pushing them so the final path is well-formed,
                            // but no further symlink_metadata checks are
                            // meaningful until the target is created.
                            for remaining in Path::new(path.as_str())
                                .components()
                                .skip(cur_strip_count(&self.root, &cur))
                            {
                                if let Component::Normal(p) = remaining {
                                    cur.push(p);
                                }
                            }
                            break;
                        }
                        Err(e) => return Err(LocalWorkspaceError::Io(e)),
                    }
                }
                Component::CurDir => {}
                Component::ParentDir => {
                    // WorkspacePath::parse already rejected `..`, so this is
                    // unreachable in practice; fail closed regardless.
                    return Err(LocalWorkspaceError::RootEscape);
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(LocalWorkspaceError::RootEscape);
                }
            }
        }

        // Containment: lexical guarantee from WorkspacePath + per-segment
        // symlink rejection above means cur must start with root. Confirm
        // and fail closed if anything is inconsistent.
        if !cur.starts_with(&self.root) {
            return Err(LocalWorkspaceError::RootEscape);
        }

        let (kind, byte_size) = match &last_meta {
            Some(meta) if last_exists => {
                let kind = metadata_kind(meta);
                let byte_size = if meta.is_file() {
                    Some(meta.len())
                } else {
                    None
                };
                (Some(kind), byte_size)
            }
            _ => (None, None),
        };

        // Parent facts: walk to the parent of the resolved path.
        let parent = cur.parent().unwrap_or(&self.root).to_path_buf();
        let (parent_exists, parent_kind) = match fs::symlink_metadata(&parent) {
            Ok(m) => (true, Some(metadata_kind(&m))),
            Err(_) => (false, None),
        };

        // Containment is the lexical + per-segment-symlink guarantee that the
        // resolved path stays beneath the canonical root. It must hold for
        // not-yet-existing targets (creates) as well as existing ones: a
        // create into a parent that is itself under the root is contained
        // even though the leaf does not yet exist. `cur.starts_with(root)`
        // was already checked above, so containment is true here regardless
        // of whether the leaf exists.
        let canonical_under_workspace = true;

        let facts = PathFacts {
            normalized_path: path.as_str().to_string(),
            exists: last_exists,
            kind,
            parent_exists,
            parent_kind,
            byte_size,
            canonical_under_workspace,
        };
        Ok((cur, facts))
    }

    /// Open the parent directory of a mutation target through a pinned
    /// per-component dirfd walk. `parent_rel` is the parent directory path
    /// relative to the workspace root; the returned fd is the ONLY way the
    /// write path (create/install/remove) may reach a parent directory.
    ///
    /// Why this exists: `resolve_secure` validates a path and then hands
    /// back a PATH, and anything that re-traverses that path afterward
    /// (open/rename/unlink on the full path) is exposed to a TOCTOU window —
    /// between validation and use, an already-validated directory component
    /// can be swapped for a symlink, redirecting the mutation OUTSIDE the
    /// workspace and bypassing deny rules.
    ///
    /// The walk is portable (macOS has neither `openat2` nor `O_PATH`, so it
    /// uses plain `O_RDONLY` directory opens): the root is opened once with
    /// `O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC`, and every child component is opened
    /// with `openat` relative to the already-pinned parent fd using the same
    /// flags. Once a directory fd is open it is pinned to the inode, so a later
    /// symlink swap of an ancestor PATH component cannot affect operations done
    /// through the fd, and `O_NOFOLLOW|O_DIRECTORY` rejects a symlink (or
    /// non-directory) swapped in at the component itself. Callers then act
    /// through the returned fd with single-component leaf names, so no step
    /// after this walk re-traverses the path.
    ///
    /// With `create_missing`, an absent component is created via `mkdirat`
    /// relative to the pinned fd (`EEXIST` tolerated — losing a race to a
    /// concurrent create is indistinguishable from the directory having
    /// existed) and the `O_NOFOLLOW` open is retried; the retry catches a
    /// symlink swapped in between the `mkdirat` and the `openat`.
    ///
    /// # What this does NOT guarantee (see FINDINGS.md L-39-04)
    ///
    /// This is **not** equivalent to `openat2(RESOLVE_BENEATH)`. `openat2`
    /// resolves parent *and* leaf in one atomic kernel call; here the parent is
    /// resolved, an fd is returned, and the leaf operation happens afterwards.
    /// Pinning defeats symlink substitution, but it does not detect the pinned
    /// directory being **renamed out of the workspace** between the pin and the
    /// leaf operation: the fd follows the inode, so the leaf then lands wherever
    /// that directory now lives. Nothing in the walk is a symlink in that
    /// attack, so `O_NOFOLLOW` never fires, and there is no post-pin
    /// containment re-check. Closing it needs `openat2(RESOLVE_BENEATH)` over
    /// the whole path on Linux (with this walk as the macOS fallback).
    /// The attacker must already hold concurrent write access to the project
    /// directory, which is why the residual is accepted rather than blocking.
    ///
    /// Symlink-shaped failures (`ELOOP`/`ENOTDIR`/`EMLINK`) map to
    /// [`LocalWorkspaceError::SymlinkRejected`]; everything else maps to
    /// `Io`.
    fn open_parent_dir_secure(
        &self,
        parent_rel: &Path,
        create_missing: bool,
    ) -> Result<OwnedFd, LocalWorkspaceError> {
        // Pin the root first. The root was canonicalized at construction,
        // so O_NOFOLLOW failing here means the root itself was tampered
        // with; fail closed as plain I/O.
        let mut fd = rustix::fs::open(&self.root, dir_open_flags(), Mode::empty())
            .map_err(|e| LocalWorkspaceError::Io(e.into()))?;
        // `cur` mirrors the walk for error reporting only — it is never
        // used to touch the filesystem.
        let mut cur = self.root.clone();
        for comp in parent_rel.components() {
            match comp {
                Component::Normal(part) => {
                    cur.push(part);
                    fd = open_dir_component(&fd, part, &cur, create_missing)?;
                }
                Component::CurDir => {}
                // `parent_rel` derives from a resolve_secure result already
                // contained under the root; anything non-normal here is
                // inconsistent — fail closed.
                _ => return Err(LocalWorkspaceError::RootEscape),
            }
        }
        Ok(fd)
    }

    /// Async per-segment secure resolution returning only the observed facts.
    ///
    /// Handlers and the edit engine use this for preflight: the facts feed
    /// both their own flow (existence/kind/byte size) and Guard L1
    /// validation via [`PathFacts::to_guard`]. Resolution runs in the
    /// blocking pool.
    pub(crate) async fn resolve_facts(
        &self,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Result<PathFacts, LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            ws.resolve_secure(&path, intent).map(|(_, facts)| facts)
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Read a file under the workspace, returning its bytes.
    pub(crate) async fn read_file(
        &self,
        path: &WorkspacePath,
    ) -> Result<Vec<u8>, LocalWorkspaceError> {
        path.require_file_path()?;
        let ws = self.blocking_clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let (full, facts) = ws.resolve_secure(&path, PathIntent::Read)?;
            if !facts.exists {
                return Err(LocalWorkspaceError::NotFound {
                    normalized: path.as_str().to_string(),
                });
            }
            if !matches!(facts.kind, Some(PathKind::File)) {
                return Err(LocalWorkspaceError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "not a regular file",
                )));
            }
            let bytes = fs::read(&full)?;
            Ok(bytes)
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// List directory entries directly beneath `path` (non-recursive).
    pub(crate) async fn list_dir(
        &self,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>, LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let (full, facts) = ws.resolve_secure(&path, PathIntent::ListDirectory)?;
            if !facts.exists {
                return Err(LocalWorkspaceError::NotFound {
                    normalized: path.as_str().to_string(),
                });
            }
            if !matches!(facts.kind, Some(PathKind::Directory)) {
                return Err(LocalWorkspaceError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "not a directory",
                )));
            }
            let mut entries = Vec::new();
            for entry in fs::read_dir(&full)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                // `DirEntry::file_type` does NOT follow symlinks (unlike
                // `DirEntry::metadata`), so symlinks — including dangling
                // ones — list as `Symlink` instead of erroring the whole
                // listing or masquerading as their target's kind.
                let ft = entry.file_type()?;
                entries.push(DirectoryEntry {
                    name,
                    kind: file_type_kind(ft),
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Grep a directory subtree, returning up to `max_results` matches.
    ///
    /// Hidden paths (Policy-Visible Workspace) produce no matches and are not
    /// counted toward truncation.
    ///
    /// Byte-budget truncation semantics: files larger than
    /// [`GREP_MAX_FILE_BYTES`] are silently skipped (exactly like unreadable
    /// files), scanning stops once [`GREP_MAX_TOTAL_BYTES`] of content has
    /// been read across the walk, and lines longer than
    /// [`GREP_MAX_LINE_BYTES`] are consumed against the budget but never
    /// matched. Callers may therefore see fewer matches than exist on disk
    /// even when the result is not `max_results`-truncated.
    pub(crate) async fn grep(
        &self,
        req: &GrepRequest,
        max_results: usize,
        mut visibility: VisibilityEngine,
    ) -> Result<(Vec<GrepMatch>, VisibilityEvidence), LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let req = req.clone();
        tokio::task::spawn_blocking(move || {
            let (full, facts) = ws.resolve_secure(&req.path, PathIntent::ListDirectory)?;
            if !facts.exists {
                return Err(LocalWorkspaceError::NotFound {
                    normalized: req.path.as_str().to_string(),
                });
            }
            if !matches!(facts.kind, Some(PathKind::Directory)) {
                return Err(LocalWorkspaceError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "grep root is not a directory",
                )));
            }
            let re = Regex::new(&req.pattern)
                .map_err(std::io::Error::other)
                .map_err(LocalWorkspaceError::Io)?;
            let mut out: Vec<GrepMatch> = Vec::new();
            let mut budget = GrepBudget::default();
            grep_walk(
                &ws.root,
                &full,
                &re,
                req.glob.as_deref(),
                max_results,
                &mut budget,
                &mut visibility,
                &mut out,
            )?;
            Ok((out, visibility.evidence()))
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Find files matching a glob beneath a directory subtree.
    ///
    /// Hidden paths are omitted from results and truncation counts.
    pub(crate) async fn find_files(
        &self,
        req: &FindRequest,
        max_results: usize,
        mut visibility: VisibilityEngine,
    ) -> Result<(Vec<WorkspacePath>, VisibilityEvidence), LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let req = req.clone();
        tokio::task::spawn_blocking(move || {
            let (full, facts) = ws.resolve_secure(&req.path, PathIntent::ListDirectory)?;
            if !facts.exists {
                return Err(LocalWorkspaceError::NotFound {
                    normalized: req.path.as_str().to_string(),
                });
            }
            if !matches!(facts.kind, Some(PathKind::Directory)) {
                return Err(LocalWorkspaceError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "find root is not a directory",
                )));
            }
            let mut out: Vec<WorkspacePath> = Vec::new();
            find_walk(
                &ws.root,
                &full,
                &req.pattern,
                max_results,
                &mut visibility,
                &mut out,
            )?;
            Ok((out, visibility.evidence()))
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// No-replace create (ADR 0003 decision 8).
    ///
    /// Uses `O_CREAT|O_EXCL`. If the target already exists the call fails
    /// with [`LocalWorkspaceError::TargetExists`], which the caller maps to
    /// `ReplanRequired`. Missing parent directories are created when
    /// `create_parents` is true.
    pub(crate) async fn create_excl(
        &self,
        path: &WorkspacePath,
        content: &[u8],
        create_parents: bool,
    ) -> Result<CreatedFile, LocalWorkspaceError> {
        path.require_file_path()?;
        let ws = self.blocking_clone();
        let path = path.clone();
        let content = content.to_vec();
        tokio::task::spawn_blocking(move || {
            let (full, facts) = ws.resolve_secure(&path, PathIntent::Create)?;
            #[cfg(any(test, feature = "test-fault-injection"))]
            if ws.fail_install.swap(false, Ordering::SeqCst) {
                return Err(LocalWorkspaceError::Io(std::io::Error::other(
                    "injected install failure",
                )));
            }
            if facts.exists {
                return Err(LocalWorkspaceError::TargetExists {
                    normalized: path.as_str().to_string(),
                });
            }
            if !create_parents && !facts.parent_exists {
                return Err(LocalWorkspaceError::NotFound {
                    normalized: path.as_str().to_string(),
                });
            }
            let (parent_rel, leaf) = split_parent_leaf(&ws.root, &full)?;
            let parent_fd = ws.open_parent_dir_secure(&parent_rel, create_parents)?;
            // O_CREAT|O_EXCL|O_NOFOLLOW on the single-component leaf,
            // relative to the pinned parent fd: the no-replace guarantee
            // and symlink-leaf rejection are enforced by the kernel at open
            // time with no path re-traversal.
            let flags =
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let fd = rustix::fs::openat(&parent_fd, leaf.as_os_str(), flags, CREATE_FILE_MODE)
                .map_err(|err| {
                    if err == Errno::EXIST {
                        LocalWorkspaceError::TargetExists {
                            normalized: path.as_str().to_string(),
                        }
                    } else {
                        LocalWorkspaceError::Io(err.into())
                    }
                })?;
            let mut file = fs::File::from(fd);
            file.write_all(&content)?;
            file.sync_all()?;
            drop(file);
            Ok(CreatedFile {
                path,
                content_hash: content_hash(&content),
                bytes_written: content.len() as u64,
            })
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Atomic edit install (ADR 0003 decision 8).
    ///
    /// Writes a same-directory tempfile, fsyncs the data, atomically renames
    /// over the target, then fsyncs the parent directory entry. All steps go
    /// through the pinned parent dirfd from [`Self::open_parent_dir_secure`]
    /// with single-component names, so no step re-traverses the validated
    /// path. Renaming OVER a swapped-in symlink leaf replaces the symlink
    /// ENTRY itself (which lives inside the workspace), never its target —
    /// safe by construction. Returns the post-install content hash. The
    /// caller must have already checked the mutation precondition (expected
    /// hash or absence) against current Local state.
    pub(crate) async fn install_atomic(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<InstalledFile, LocalWorkspaceError> {
        path.require_file_path()?;
        let ws = self.blocking_clone();
        let path = path.clone();
        let content = content.to_vec();
        tokio::task::spawn_blocking(move || {
            let (full, _facts) = ws.resolve_secure(&path, PathIntent::Overwrite)?;
            #[cfg(any(test, feature = "test-fault-injection"))]
            if ws.fail_install.swap(false, Ordering::SeqCst) {
                return Err(LocalWorkspaceError::Io(std::io::Error::other(
                    "injected install failure",
                )));
            }
            let (parent_rel, leaf) = split_parent_leaf(&ws.root, &full)?;
            // create_missing=true preserves the historical create_dir_all
            // behavior of the install path, now through the pinned walk.
            let parent_fd = ws.open_parent_dir_secure(&parent_rel, true)?;
            // Unique single-component temp name: pid disambiguates across
            // processes sharing a workspace, the sequence number across
            // concurrent installs into the same directory in this process.
            //
            // Neither is collision-proof on its own — a crashed process can
            // leave `.lattice-tmp-<pid>-<seq>` residue that a later process
            // recycling that pid walks into, and two PID namespaces over one
            // bind-mounted workspace can both start at the same pid. `O_EXCL`
            // detects that; retrying with a fresh sequence number resolves it
            // rather than surfacing a bare `EEXIST`.
            let tmp_flags =
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let (tmp_name, tmp_fd) = {
                let mut last = None;
                let mut acquired = None;
                for _ in 0..INSTALL_TMP_NAME_ATTEMPTS {
                    let candidate = format!(
                        ".lattice-tmp-{}-{}",
                        std::process::id(),
                        INSTALL_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
                    );
                    match rustix::fs::openat(
                        &parent_fd,
                        candidate.as_str(),
                        tmp_flags,
                        CREATE_FILE_MODE,
                    ) {
                        Ok(fd) => {
                            acquired = Some((candidate, fd));
                            break;
                        }
                        Err(e) if e == Errno::EXIST => last = Some(e),
                        Err(e) => return Err(LocalWorkspaceError::Io(e.into())),
                    }
                }
                acquired.ok_or_else(|| {
                    LocalWorkspaceError::Io(std::io::Error::other(format!(
                        "could not claim an install temp name after \
                         {INSTALL_TMP_NAME_ATTEMPTS} attempts: {}",
                        last.map(|e| e.to_string()).unwrap_or_default()
                    )))
                })?
            };
            let mut tmp_file = fs::File::from(tmp_fd);
            let installed = (|| -> Result<(), LocalWorkspaceError> {
                tmp_file.write_all(&content)?;
                tmp_file.sync_all()?;
                // Both names are single components relative to the same
                // pinned fd, so no symlink traversal is possible on either
                // side of the rename.
                rustix::fs::renameat(&parent_fd, tmp_name.as_str(), &parent_fd, leaf.as_os_str())
                    .map_err(|e| {
                    LocalWorkspaceError::Io(std::io::Error::other(format!(
                        "atomic persist failed: {e}"
                    )))
                })?;
                Ok(())
            })();
            if let Err(err) = installed {
                // Best-effort temp hygiene on any failure after creation;
                // the rename consumed the temp name on success.
                let _ = rustix::fs::unlinkat(&parent_fd, tmp_name.as_str(), AtFlags::empty());
                return Err(err);
            }
            drop(tmp_file);
            // Durably record the rename in the directory itself — through
            // the pinned fd, replacing the old path-based directory reopen.
            rustix::fs::fsync(&parent_fd).map_err(|e| LocalWorkspaceError::Io(e.into()))?;
            Ok(InstalledFile {
                path,
                content_hash: content_hash(&content),
                bytes_written: content.len() as u64,
            })
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Remove a file.
    pub(crate) async fn remove_file(
        &self,
        path: &WorkspacePath,
    ) -> Result<(), LocalWorkspaceError> {
        path.require_file_path()?;
        let ws = self.blocking_clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let (full, facts) = ws.resolve_secure(&path, PathIntent::Delete)?;
            if !facts.exists {
                return Err(LocalWorkspaceError::NotFound {
                    normalized: path.as_str().to_string(),
                });
            }
            if !matches!(facts.kind, Some(PathKind::File)) {
                return Err(LocalWorkspaceError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "not a regular file",
                )));
            }
            let (parent_rel, leaf) = split_parent_leaf(&ws.root, &full)?;
            let parent_fd = ws.open_parent_dir_secure(&parent_rel, false)?;
            // Unlink the single-component leaf through the pinned fd. A
            // post-resolve file→dir swap fails (EISDIR/EPERM → Io); a
            // swapped-in symlink leaf has its directory ENTRY removed —
            // which lives inside the workspace — never its target.
            rustix::fs::unlinkat(&parent_fd, leaf.as_os_str(), AtFlags::empty())
                .map_err(|e| LocalWorkspaceError::Io(e.into()))?;
            Ok(())
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Flip a one-shot flag so the next install (`install_atomic` or
    /// `create_excl`) fails before any bytes are written. Narrow test-only
    /// fault injection proving the install failure boundary leaves the
    /// target unchanged — a single boolean, not a composable plugin seam.
    #[cfg(any(test, feature = "test-fault-injection"))]
    pub(crate) fn fail_next_install(&self) {
        self.fail_install.store(true, Ordering::SeqCst);
    }
}

fn metadata_kind(meta: &fs::Metadata) -> PathKind {
    file_type_kind(meta.file_type())
}

/// Map a raw file type to the Local `PathKind` vocabulary.
///
/// Callers must obtain the `FileType` WITHOUT following symlinks
/// (`fs::symlink_metadata` or `DirEntry::file_type`), otherwise the
/// `Symlink` branch is dead and dangling links surface as errors instead of
/// entries.
fn file_type_kind(ft: fs::FileType) -> PathKind {
    if ft.is_symlink() {
        PathKind::Symlink
    } else if ft.is_dir() {
        PathKind::Directory
    } else {
        PathKind::File
    }
}

/// Number of path components between `root` and `cur`, used to skip already
/// walked components when filling in remaining segments lexically.
fn cur_strip_count(root: &Path, cur: &Path) -> usize {
    cur.strip_prefix(root)
        .map(|rel| rel.components().count())
        .unwrap_or(0)
}

fn relative_under(root: &Path, p: &Path) -> Result<String, LocalWorkspaceError> {
    let rel = p
        .strip_prefix(root)
        .map_err(|_| LocalWorkspaceError::RootEscape)?;
    let mut parts = Vec::new();
    for c in rel.components() {
        if let Component::Normal(n) = c {
            parts.push(n.to_string_lossy().into_owned());
        }
    }
    Ok(parts.join("/"))
}

/// Mode for files created by the write path (`0o644`, further narrowed by
/// the process umask — the same policy `OpenOptions` applied before).
const CREATE_FILE_MODE: Mode = Mode::from_raw_mode(0o644);

/// Monotonic per-process counter making install temp names unique when
/// concurrent installs target the same directory.
static INSTALL_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// How many temp names an install may try before giving up. Only reached when
/// `O_EXCL` keeps hitting leftover `.lattice-tmp-*` residue from a crashed
/// process that shared this pid, or a second PID namespace over the same
/// workspace; a handful of fresh sequence numbers clears both.
const INSTALL_TMP_NAME_ATTEMPTS: usize = 8;

/// Directory-open flags for every step of the pinned dirfd walk: read-only
/// (macOS has no `O_PATH`), directories only, and no symlink following.
fn dir_open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Open one child directory component relative to an already-pinned fd.
///
/// `cur` is the walked path for error reporting only. With
/// `create_missing`, a missing component is `mkdirat`-created and the
/// `O_NOFOLLOW` open retried; the retry catches a symlink swapped in
/// between the `mkdirat` and the `openat`.
fn open_dir_component(
    dirfd: &OwnedFd,
    name: &OsStr,
    cur: &Path,
    create_missing: bool,
) -> Result<OwnedFd, LocalWorkspaceError> {
    match rustix::fs::openat(dirfd, name, dir_open_flags(), Mode::empty()) {
        Ok(fd) => Ok(fd),
        Err(err) if err == Errno::NOENT && create_missing => {
            // Tolerate EEXIST: losing a race to a concurrent create is
            // indistinguishable from the directory having existed.
            if let Err(e) = rustix::fs::mkdirat(dirfd, name, Mode::from_raw_mode(0o755))
                && e != Errno::EXIST
            {
                return Err(map_walk_errno(e, cur));
            }
            rustix::fs::openat(dirfd, name, dir_open_flags(), Mode::empty())
                .map_err(|e| map_walk_errno(e, cur))
        }
        Err(err) => Err(map_walk_errno(err, cur)),
    }
}

/// Map a dirfd-walk errno: symlink-shaped failures — `ELOOP` from
/// `O_NOFOLLOW`, `ENOTDIR` from a non-directory swap, `EMLINK` from
/// BSD-style `O_NOFOLLOW` rejection — become `SymlinkRejected`; the rest
/// stay plain I/O.
fn map_walk_errno(e: Errno, cur: &Path) -> LocalWorkspaceError {
    if e == Errno::LOOP || e == Errno::NOTDIR || e == Errno::MLINK {
        LocalWorkspaceError::SymlinkRejected {
            component: cur.to_path_buf(),
        }
    } else {
        LocalWorkspaceError::Io(e.into())
    }
}

/// Split a `resolve_secure` result into (parent path relative to `root`,
/// single-component leaf name) for the pinned-fd write path.
fn split_parent_leaf(root: &Path, full: &Path) -> Result<(PathBuf, OsString), LocalWorkspaceError> {
    let parent = full.parent().unwrap_or(root);
    let parent_rel = parent
        .strip_prefix(root)
        .map_err(|_| LocalWorkspaceError::RootEscape)?
        .to_path_buf();
    let leaf = full
        .file_name()
        .ok_or_else(|| {
            LocalWorkspaceError::Io(std::io::Error::other("mutation target has no file name"))
        })?
        .to_os_string();
    Ok((parent_rel, leaf))
}

/// Per-file grep size cap. Files whose on-disk size exceeds this are skipped
/// outright — before being opened — exactly like unreadable files.
const GREP_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Aggregate byte budget for one grep walk. Once this many content bytes
/// have been read across all scanned files, the walk stops descending and
/// scanning, bounding worst-case RSS/IO independently of `max_results`.
const GREP_MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Longest line eligible for matching. Longer lines are consumed against the
/// budget but never matched or returned, so a single pathological line
/// cannot balloon a `GrepMatch` payload.
const GREP_MAX_LINE_BYTES: usize = 64 * 1024;

/// Byte budget threaded through one grep walk.
///
/// `per_file_bytes` is the fixed per-file cap; `remaining_total_bytes`
/// drains as file content is read. Both bound bytes *read*, not bytes
/// matched, so memory stays proportional to the budget regardless of file
/// or match sizes.
struct GrepBudget {
    /// Fixed cap on bytes read from any single file.
    per_file_bytes: u64,
    /// Aggregate bytes still available across the rest of the walk.
    remaining_total_bytes: u64,
}

impl GrepBudget {
    fn new(per_file_bytes: u64, total_bytes: u64) -> Self {
        Self {
            per_file_bytes,
            remaining_total_bytes: total_bytes,
        }
    }

    /// True once the aggregate budget has been fully consumed.
    fn exhausted(&self) -> bool {
        self.remaining_total_bytes == 0
    }

    /// Bytes the next file may read: the per-file cap clamped by whatever
    /// remains of the aggregate budget.
    fn file_allowance(&self) -> u64 {
        self.per_file_bytes.min(self.remaining_total_bytes)
    }

    /// Consume `bytes` from the aggregate budget (saturating).
    fn charge(&mut self, bytes: u64) {
        self.remaining_total_bytes = self.remaining_total_bytes.saturating_sub(bytes);
    }
}

impl Default for GrepBudget {
    fn default() -> Self {
        Self::new(GREP_MAX_FILE_BYTES, GREP_MAX_TOTAL_BYTES)
    }
}

// Recursive walk plumbing: every parameter is threaded unchanged through the
// recursion, so a bundling struct would only rename the argument list.
#[allow(clippy::too_many_arguments)]
fn grep_walk(
    workspace_root: &Path,
    current: &Path,
    re: &Regex,
    glob: Option<&str>,
    max_results: usize,
    budget: &mut GrepBudget,
    visibility: &mut VisibilityEngine,
    out: &mut Vec<GrepMatch>,
) -> Result<(), LocalWorkspaceError> {
    if out.len() >= max_results || budget.exhausted() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        if out.len() >= max_results || budget.exhausted() {
            break;
        }
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let rel = relative_under(workspace_root, &path)?;
        if meta.is_dir() {
            if visibility.should_descend(&rel) {
                grep_walk(
                    workspace_root,
                    &path,
                    re,
                    glob,
                    max_results,
                    budget,
                    visibility,
                    out,
                )?;
            }
        } else if meta.is_file() {
            if !visibility.is_visible_file(&rel) {
                continue;
            }
            if let Some(g) = glob {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !glob_match(g, name) {
                    continue;
                }
            }
            // Over-cap files are skipped before being opened, exactly like
            // unreadable files below.
            if meta.len() > budget.per_file_bytes {
                continue;
            }
            grep_file(&rel, &path, re, max_results, budget, out)?;
        }
    }
    Ok(())
}

/// Scan one file line-by-line under the walk budget.
///
/// The reader is hard-capped at the budget's current file allowance via
/// [`Read::take`], so a file can never cost more than `per_file_bytes` (or
/// the remaining aggregate budget, if smaller) regardless of its size on
/// disk. Every byte read is charged to the budget. Lines longer than
/// [`GREP_MAX_LINE_BYTES`] are consumed but never matched. An IO or
/// invalid-UTF-8 error mid-file stops the scan of that file while keeping
/// the matches already found in it — a deliberate improvement over the old
/// whole-file `read_to_string`, where a single invalid byte dropped every
/// match in the file.
fn grep_file(
    rel: &str,
    path: &Path,
    re: &Regex,
    max_results: usize,
    budget: &mut GrepBudget,
    out: &mut Vec<GrepMatch>,
) -> Result<(), LocalWorkspaceError> {
    // Unreadable files are silently skipped, preserving the old semantics.
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };
    let allowance = budget.file_allowance();
    let mut reader = BufReader::new(file.take(allowance));
    let mut buf = String::new();
    let mut consumed: u64 = 0;
    let mut line_number: u64 = 0;
    loop {
        if out.len() >= max_results {
            break;
        }
        buf.clear();
        let read = match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            // IO / invalid-UTF-8 mid-file: keep matches already found in
            // this file, stop scanning it.
            Err(_) => break,
        };
        consumed += read as u64;
        budget.charge(read as u64);
        line_number += 1;
        // A line without a trailing newline that ends exactly at the
        // allowance was truncated by the cap; matching half a line would
        // fabricate results, so stop instead.
        if !buf.ends_with('\n') && consumed >= allowance {
            break;
        }
        let mut line = buf.as_str();
        if let Some(stripped) = line.strip_suffix('\n') {
            line = stripped;
        }
        if let Some(stripped) = line.strip_suffix('\r') {
            line = stripped;
        }
        if line.len() > GREP_MAX_LINE_BYTES {
            continue;
        }
        if re.is_match(line) {
            let wp = WorkspacePath::parse(rel).map_err(LocalWorkspaceError::Path)?;
            out.push(GrepMatch {
                path: wp,
                line_number,
                line: line.to_string(),
            });
        }
    }
    Ok(())
}

fn find_walk(
    workspace_root: &Path,
    current: &Path,
    pattern: &str,
    max_results: usize,
    visibility: &mut VisibilityEngine,
    out: &mut Vec<WorkspacePath>,
) -> Result<(), LocalWorkspaceError> {
    if out.len() >= max_results {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        if out.len() >= max_results {
            break;
        }
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = relative_under(workspace_root, &path)?;
        let meta = fs::symlink_metadata(&path)?;
        let is_dir = meta.is_dir();
        let assessment = visibility.assess(&rel, is_dir);
        if glob_match(pattern, &name) && assessment.is_visible() {
            out.push(WorkspacePath::parse(&rel).map_err(LocalWorkspaceError::Path)?);
        }
        if is_dir && assessment.should_descend() {
            find_walk(workspace_root, &path, pattern, max_results, visibility, out)?;
        }
    }
    Ok(())
}

/// Fnmatch-style glob matcher backing `find_files` patterns and grep's glob
/// file filter.
///
/// Supports `*` (any sequence, including empty), `?` (exactly one
/// character), and character classes `[abc]`, `[a-z]`, `[!...]`. An
/// unclosed `[` is treated as a literal `[`. Matches over Unicode scalar
/// values using the standard iterative two-pointer backtracking algorithm —
/// no recursion, no dependency, linear-space, worst-case O(pattern × name).
fn glob_match(pattern: &str, name: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = name.chars().collect();
    let mut p = 0usize;
    let mut t = 0usize;
    // Most recent `*` in the pattern and the text position just after the
    // span it currently absorbs; on mismatch we backtrack here and widen
    // the span by one character.
    let mut star: Option<usize> = None;
    let mut star_t = 0usize;
    while t < txt.len() {
        if p < pat.len() {
            match pat[p] {
                '*' => {
                    star = Some(p);
                    star_t = t;
                    p += 1;
                    continue;
                }
                '?' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, next_p)) = match_char_class(&pat, p, txt[t]) {
                        if matched {
                            p = next_p;
                            t += 1;
                            continue;
                        }
                        // Well-formed class that does not match: fall
                        // through to backtracking.
                    } else if txt[t] == '[' {
                        // Unclosed class: literal '['.
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
                c => {
                    if c == txt[t] {
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
            }
        }
        // Mismatch (or pattern exhausted with text left): let the last `*`
        // absorb one more character, or fail if there is none.
        match star {
            Some(sp) => {
                star_t += 1;
                p = sp + 1;
                t = star_t;
            }
            None => return false,
        }
    }
    // Text consumed: the rest of the pattern must be all `*`.
    while p < pat.len() && pat[p] == '*' {
        p += 1;
    }
    p == pat.len()
}

/// Match `ch` against the character class opening at `pat[start]` (which
/// must be `[`).
///
/// Returns `None` when the class is never closed (the caller then treats
/// the `[` as a literal), otherwise `Some((matched, index past the closing
/// `]`))`. A leading `!` negates; a `]` immediately after `[` or `[!` is a
/// literal member (fnmatch convention); `a-z` ranges are inclusive.
fn match_char_class(pat: &[char], start: usize, ch: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negated = if i < pat.len() && pat[i] == '!' {
        i += 1;
        true
    } else {
        false
    };
    let mut matched = false;
    let mut first = true;
    while i < pat.len() {
        if pat[i] == ']' && !first {
            return Some((matched != negated, i + 1));
        }
        first = false;
        if i + 2 < pat.len() && pat[i + 1] == '-' && pat[i + 2] != ']' {
            if pat[i] <= ch && ch <= pat[i + 2] {
                matched = true;
            }
            i += 3;
        } else {
            if pat[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn ws_at(dir: &TempDir) -> LocalWorkspace {
        LocalWorkspace::new(dir.path())
    }

    // ── ADR step 1: eight test classes ───────────────────────────────────

    // 1. Root escape: WorkspacePath::parse already rejects `..` and absolute
    //    paths, so the only escape vector is a symlink chain. This test pins
    //    the leaf-symlink rejection that closes that vector.
    #[tokio::test]
    async fn root_escape_via_symlink_rejected() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret"), b"private").unwrap();
        let link = dir.path().join("escape");
        symlink(outside.path(), &link).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("escape/secret").unwrap();
        let err = ws.read_file(&path).await.unwrap_err();
        assert!(
            matches!(err, LocalWorkspaceError::SymlinkRejected { .. }),
            "got {err:?}"
        );
    }

    // 2. Symlink parent rejection: a directory symlink in the parent chain
    //    must be rejected even when the leaf is a regular file.
    #[tokio::test]
    async fn symlink_parent_rejected_even_when_leaf_is_regular() {
        let dir = TempDir::new().unwrap();
        let real = TempDir::new().unwrap();
        std::fs::write(real.path().join("file"), b"ok").unwrap();
        let link = dir.path().join("linkdir");
        symlink(real.path(), &link).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("linkdir/file").unwrap();
        let err = ws.read_file(&path).await.unwrap_err();
        assert!(matches!(err, LocalWorkspaceError::SymlinkRejected { .. }));
    }

    // 3. Symlink leaf rejection: a symlink at the target itself is rejected.
    #[tokio::test]
    async fn symlink_leaf_rejected() {
        let dir = TempDir::new().unwrap();
        let target = TempDir::new().unwrap();
        std::fs::write(target.path().join("file"), b"x").unwrap();
        let link = dir.path().join("leaf");
        symlink(target.path().join("file"), &link).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("leaf").unwrap();
        let err = ws.read_file(&path).await.unwrap_err();
        assert!(matches!(err, LocalWorkspaceError::SymlinkRejected { .. }));
    }

    // 4. Limits: content too large is surfaced by the caller (handler), not
    //    by the workspace itself, so this test pins the bounded-read contract
    //    indirectly by confirming the read returns the full bytes and the
    //    handler-side budget check is what truncates. We assert read_file
    //    returns exactly the file contents for a small file.
    #[tokio::test]
    async fn read_returns_full_bytes_for_small_file() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("a.txt").unwrap();
        let bytes = ws.read_file(&path).await.unwrap();
        assert_eq!(bytes, b"hello");
    }

    // 5. Expected-hash conflict: the caller checks the precondition; this
    //    test pins that the installed content hash matches a known blake3
    //    literal computed independently, so the precondition comparison is
    //    sound.
    #[tokio::test]
    async fn installed_content_hash_matches_independent_blake3() {
        let dir = TempDir::new().unwrap();
        let body = b"lattice content hash";
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("h.txt").unwrap();
        let created = ws.create_excl(&path, body.as_ref(), false).await.unwrap();
        let want = format!("blake3:{}", blake3::hash(body).to_hex());
        assert_eq!(created.content_hash, want);
    }

    // 6. No-replace create: a second create on the same path returns
    //    TargetExists, leaving the original file untouched.
    #[tokio::test]
    async fn create_excl_rejects_existing_target_and_preserves_original() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("c.txt"), b"original").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("c.txt").unwrap();
        let err = ws
            .create_excl(&path, b"attacker".as_ref(), false)
            .await
            .unwrap_err();
        assert!(matches!(err, LocalWorkspaceError::TargetExists { .. }));
        let on_disk = std::fs::read(dir.path().join("c.txt")).unwrap();
        assert_eq!(on_disk, b"original");
    }

    // 7. Atomic replacement: install_atomic overwrites the target and the
    //    content is observable immediately afterward, atomically.
    #[tokio::test]
    async fn install_atomic_replaces_target_atomically() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("r.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("r.txt").unwrap();
        let installed = ws.install_atomic(&path, b"v2").await.unwrap();
        assert_eq!(installed.bytes_written, 2);
        let on_disk = std::fs::read(dir.path().join("r.txt")).unwrap();
        assert_eq!(on_disk, b"v2");
        let want = format!("blake3:{}", blake3::hash(b"v2").to_hex());
        assert_eq!(installed.content_hash, want);
    }

    // 8. Partial-write failure: writing into a read-only directory fails and
    //    leaves no partial target on disk (the O_EXCL temp creation itself
    //    fails, and any later failure unlinks the temp name through the
    //    pinned parent fd). This is the real-fault approximation ADR step 1
    //    prescribes; the narrow fault-injection seam below covers the
    //    deterministic boundary.
    #[tokio::test]
    async fn install_atomic_failure_leaves_no_partial_target() {
        let dir = TempDir::new().unwrap();
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        let mut perms = fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&ro, perms).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("ro/new.txt").unwrap();
        let err = ws.install_atomic(&path, b"x").await;
        // Restore permissions so TempDir can clean up. Test-only restore
        // path: the TempDir parent is owner-only, so widening the child back
        // to writable here carries no production meaning.
        let mut perms = fs::metadata(&ro).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(&ro, perms);
        assert!(err.is_err(), "expected install to fail in read-only dir");
        assert!(
            !dir.path().join("ro/new.txt").exists(),
            "no partial target may be left on disk"
        );
    }

    // 8b. Narrow fault injection (#38): a primed install failure leaves the
    //     existing target byte-identical, and the flag is one-shot — the
    //     next install succeeds. This proves the atomic-install failure
    //     boundary without a composable plugin seam.
    #[tokio::test]
    async fn fail_next_install_leaves_target_unchanged_and_resets() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("f.txt").unwrap();

        ws.fail_next_install();
        let err = ws.install_atomic(&path, b"v2").await.unwrap_err();
        assert!(matches!(err, LocalWorkspaceError::Io(_)), "got {err:?}");
        let on_disk = std::fs::read(dir.path().join("f.txt")).unwrap();
        assert_eq!(on_disk, b"v1", "failed install must not touch the target");

        // One-shot: the flag was consumed by the failed install.
        ws.install_atomic(&path, b"v2").await.unwrap();
        let on_disk = std::fs::read(dir.path().join("f.txt")).unwrap();
        assert_eq!(on_disk, b"v2");
    }

    // ── Supporting tests for observation operations ──────────────────────

    #[tokio::test]
    async fn list_dir_returns_sorted_entries() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("b"), b"").unwrap();
        std::fs::write(dir.path().join("a"), b"").unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::project_root();
        let entries = ws.list_dir(&path).await.unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["a", "b", "d"]);
    }

    #[tokio::test]
    async fn grep_finds_matches_with_line_numbers() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("g.txt"), "alpha\nbeta\nalpha\n").unwrap();
        let ws = ws_at(&dir);
        let req = GrepRequest {
            pattern: "alpha".into(),
            path: WorkspacePath::project_root(),
            glob: None,
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let (matches, _evidence) = ws.grep(&req, 100, visibility).await.unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].line_number, 1);
        assert_eq!(matches[1].line_number, 3);
    }

    #[tokio::test]
    async fn find_files_returns_matching_paths() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.rs"), b"").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/c.rs"), b"").unwrap();
        let ws = ws_at(&dir);
        let req = FindRequest {
            pattern: "*.rs".into(),
            path: WorkspacePath::project_root(),
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let (paths, _evidence) = ws.find_files(&req, 100, visibility).await.unwrap();
        let strs: Vec<_> = paths.iter().map(|p| p.as_str().to_string()).collect();
        assert!(strs.contains(&"a.rs".to_string()));
        assert!(strs.contains(&"sub/c.rs".to_string()));
        assert!(!strs.contains(&"b.txt".to_string()));
    }

    #[tokio::test]
    async fn read_file_missing_returns_not_found() {
        let dir = TempDir::new().unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("nope.txt").unwrap();
        let err = ws.read_file(&path).await.unwrap_err();
        assert!(matches!(err, LocalWorkspaceError::NotFound { .. }));
    }

    #[tokio::test]
    async fn create_excl_with_create_parents_creates_missing_dirs() {
        let dir = TempDir::new().unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("a/b/c.txt").unwrap();
        ws.create_excl(&path, b"hi".as_ref(), true).await.unwrap();
        assert!(dir.path().join("a/b/c.txt").exists());
    }

    #[tokio::test]
    async fn remove_file_deletes_target() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("rm.txt"), b"x").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("rm.txt").unwrap();
        ws.remove_file(&path).await.unwrap();
        assert!(!dir.path().join("rm.txt").exists());
    }

    // ── TOCTOU-safe write path (pinned dirfd walk) ───────────────────────

    /// Names of `.lattice-tmp-*` residue directly beneath `dir`.
    fn tmp_residue(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".lattice-tmp-"))
            .collect()
    }

    // The validate→use race itself cannot be scheduled deterministically,
    // so these tests pin the property that makes it harmless: the dirfd
    // walk re-checks every component with O_NOFOLLOW at open time, so a
    // directory swapped for a symlink AFTER resolve_secure passed is still
    // rejected and nothing reaches the outside target.
    #[test]
    fn open_parent_dir_secure_rejects_post_resolve_symlink_swap() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let ws = ws_at(&dir);
        // At this point resolve_secure("a/…") would pass: `a` is a real
        // directory. Simulate the post-resolve swap.
        std::fs::remove_dir(dir.path().join("a")).unwrap();
        symlink(outside.path(), dir.path().join("a")).unwrap();
        let err = ws
            .open_parent_dir_secure(Path::new("a"), false)
            .unwrap_err();
        assert!(
            matches!(err, LocalWorkspaceError::SymlinkRejected { .. }),
            "got {err:?}"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "nothing may reach the outside directory"
        );
    }

    // create_missing must not mkdir THROUGH a symlink component either: the
    // O_NOFOLLOW open of the symlink fails before any mkdirat can run.
    #[test]
    fn open_parent_dir_secure_create_missing_rejects_symlink_component() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let ws = ws_at(&dir);
        symlink(outside.path(), dir.path().join("a")).unwrap();
        let err = ws
            .open_parent_dir_secure(Path::new("a/b"), true)
            .unwrap_err();
        assert!(
            matches!(err, LocalWorkspaceError::SymlinkRejected { .. }),
            "got {err:?}"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "no directory may be created through the symlink"
        );
    }

    // End-to-end guard: every mutation entry point already rejects a
    // symlinked parent at resolve_secure; pin that the outside target stays
    // byte-identical afterward for all three write operations.
    #[tokio::test]
    async fn write_paths_reject_symlink_parent_and_leave_outside_untouched() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("x"), b"keep").unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("link/x").unwrap();

        let err = ws.create_excl(&path, b"evil".as_ref(), true).await;
        assert!(
            matches!(err, Err(LocalWorkspaceError::SymlinkRejected { .. })),
            "got {err:?}"
        );
        let err = ws.install_atomic(&path, b"evil").await;
        assert!(
            matches!(err, Err(LocalWorkspaceError::SymlinkRejected { .. })),
            "got {err:?}"
        );
        let err = ws.remove_file(&path).await;
        assert!(
            matches!(err, Err(LocalWorkspaceError::SymlinkRejected { .. })),
            "got {err:?}"
        );

        assert_eq!(
            std::fs::read(outside.path().join("x")).unwrap(),
            b"keep",
            "outside file must survive all three rejected mutations"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            1,
            "outside directory must gain no entries"
        );
    }

    // Temp hygiene: neither a forced install failure nor a successful
    // install may leave `.lattice-tmp-*` residue in the parent directory.
    #[tokio::test]
    async fn install_atomic_leaves_no_temp_residue() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("t.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("t.txt").unwrap();

        ws.fail_next_install();
        ws.install_atomic(&path, b"v2").await.unwrap_err();
        assert_eq!(tmp_residue(dir.path()), Vec::<String>::new());

        ws.install_atomic(&path, b"v2").await.unwrap();
        assert_eq!(tmp_residue(dir.path()), Vec::<String>::new());
        assert_eq!(std::fs::read(dir.path().join("t.txt")).unwrap(), b"v2");
    }

    // Unique temp names: concurrent installs into the same directory must
    // not collide on the O_EXCL temp file.
    #[tokio::test]
    async fn concurrent_installs_in_same_dir_succeed() {
        let dir = TempDir::new().unwrap();
        let ws = ws_at(&dir);
        let p1 = WorkspacePath::parse("one.txt").unwrap();
        let p2 = WorkspacePath::parse("two.txt").unwrap();
        let (r1, r2) = tokio::join!(
            ws.install_atomic(&p1, b"first"),
            ws.install_atomic(&p2, b"second")
        );
        r1.unwrap();
        r2.unwrap();
        assert_eq!(std::fs::read(dir.path().join("one.txt")).unwrap(), b"first");
        assert_eq!(
            std::fs::read(dir.path().join("two.txt")).unwrap(),
            b"second"
        );
        assert_eq!(tmp_residue(dir.path()), Vec::<String>::new());
    }

    // ── Grep byte-budget bounds ──────────────────────────────────────────

    fn needle_re() -> Regex {
        Regex::new("needle").unwrap()
    }

    // (a) A file above the per-file cap is skipped outright while a small
    //     file in the same walk still matches.
    #[test]
    fn grep_walk_skips_over_cap_file_but_scans_small_file() {
        let dir = TempDir::new().unwrap();
        // 100 bytes > per-file cap of 64: skipped before opening.
        let mut big = String::from("needle\n");
        big.push_str(&"x".repeat(93));
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();
        std::fs::write(dir.path().join("small.txt"), "needle\n").unwrap();
        let mut budget = GrepBudget::new(64, 1024);
        let mut visibility = VisibilityEngine::load(dir.path(), Default::default());
        let mut out = Vec::new();
        grep_walk(
            dir.path(),
            dir.path(),
            &needle_re(),
            None,
            100,
            &mut budget,
            &mut visibility,
            &mut out,
        )
        .unwrap();
        assert_eq!(out.len(), 1, "got {out:?}");
        assert_eq!(out[0].path.as_str(), "small.txt");
    }

    // (b) Total-budget exhaustion stops scanning subsequent files: an
    //     exhausted budget makes grep_walk a no-op, and a shared budget
    //     drained by one grep_file call leaves the next file only a
    //     truncated allowance that cannot produce a match.
    #[test]
    fn grep_budget_total_exhaustion_stops_scanning() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("m.txt"), "needle\n").unwrap();
        let mut budget = GrepBudget::new(64, 128);
        budget.charge(128);
        assert!(budget.exhausted());
        let mut visibility = VisibilityEngine::load(dir.path(), Default::default());
        let mut out = Vec::new();
        grep_walk(
            dir.path(),
            dir.path(),
            &needle_re(),
            None,
            100,
            &mut budget,
            &mut visibility,
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty(), "exhausted budget must scan nothing");

        // Deterministic two-file sequence: file one drains most of the
        // total, so file two's allowance is clamped below its first line.
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        std::fs::write(&a, format!("needle {}\n", "a".repeat(53))).unwrap(); // 61 bytes
        std::fs::write(&b, "needle b\n").unwrap();
        let mut budget = GrepBudget::new(64, 64);
        let mut out = Vec::new();
        grep_file("a.txt", &a, &needle_re(), 100, &mut budget, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(budget.file_allowance(), 3, "61 of 64 total consumed");
        grep_file("b.txt", &b, &needle_re(), 100, &mut budget, &mut out).unwrap();
        assert_eq!(out.len(), 1, "truncated allowance must not match: {out:?}");
    }

    // (c) Matches beyond the per-file allowance are not returned while
    //     earlier ones in the same file are.
    #[test]
    fn grep_file_drops_matches_beyond_per_file_allowance() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("c.txt");
        // Line 1 (11 bytes) fits the 32-byte allowance; the filler line
        // crosses the cap, so the later matching line is never reached.
        let content = format!("needle one\n{}\nneedle two\n", "x".repeat(40));
        std::fs::write(&path, content).unwrap();
        let mut budget = GrepBudget::new(32, 1024);
        let mut out = Vec::new();
        grep_file("c.txt", &path, &needle_re(), 100, &mut budget, &mut out).unwrap();
        assert_eq!(out.len(), 1, "got {out:?}");
        assert_eq!(out[0].line_number, 1);
        assert_eq!(out[0].line, "needle one");
    }

    // Long-line guard through the public grep() path: a >64KiB line that
    // contains the pattern is consumed but never matched; the normal line
    // after it still matches with the correct line number.
    #[tokio::test]
    async fn grep_skips_over_long_lines_but_matches_normal_ones() {
        let dir = TempDir::new().unwrap();
        let long = format!("needle {}\nneedle short\n", "y".repeat(70 * 1024));
        std::fs::write(dir.path().join("l.txt"), long).unwrap();
        let ws = ws_at(&dir);
        let req = GrepRequest {
            pattern: "needle".into(),
            path: WorkspacePath::project_root(),
            glob: None,
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let (matches, _evidence) = ws.grep(&req, 100, visibility).await.unwrap();
        assert_eq!(matches.len(), 1, "got {matches:?}");
        assert_eq!(matches[0].line_number, 2);
        assert_eq!(matches[0].line, "needle short");
    }

    // Invalid UTF-8 mid-file: the match found before the bad bytes
    // survives and no error is surfaced (old whole-file read dropped the
    // entire file).
    #[tokio::test]
    async fn grep_keeps_matches_before_invalid_utf8() {
        let dir = TempDir::new().unwrap();
        let mut bytes = b"needle ok\n".to_vec();
        bytes.extend_from_slice(b"\xff\xfe broken\n");
        bytes.extend_from_slice(b"needle later\n");
        std::fs::write(dir.path().join("u.txt"), &bytes).unwrap();
        let ws = ws_at(&dir);
        let req = GrepRequest {
            pattern: "needle".into(),
            path: WorkspacePath::project_root(),
            glob: None,
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let (matches, _evidence) = ws.grep(&req, 100, visibility).await.unwrap();
        assert_eq!(matches.len(), 1, "got {matches:?}");
        assert_eq!(matches[0].line_number, 1);
        assert_eq!(matches[0].line, "needle ok");
    }

    // ── list_dir symlink reporting ───────────────────────────────────────

    #[tokio::test]
    async fn list_dir_reports_symlinks_including_dangling() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("target.txt"), b"x").unwrap();
        std::fs::create_dir(dir.path().join("targetdir")).unwrap();
        symlink(dir.path().join("target.txt"), dir.path().join("file_link")).unwrap();
        symlink(dir.path().join("targetdir"), dir.path().join("dir_link")).unwrap();
        symlink(dir.path().join("missing"), dir.path().join("dangling")).unwrap();
        let ws = ws_at(&dir);
        let entries = ws.list_dir(&WorkspacePath::project_root()).await.unwrap();
        let kind_of = |name: &str| entries.iter().find(|e| e.name == name).map(|e| e.kind);
        assert_eq!(kind_of("file_link"), Some(PathKind::Symlink));
        assert_eq!(kind_of("dir_link"), Some(PathKind::Symlink));
        assert_eq!(kind_of("dangling"), Some(PathKind::Symlink));
        assert_eq!(kind_of("target.txt"), Some(PathKind::File));
        assert_eq!(kind_of("targetdir"), Some(PathKind::Directory));
    }

    // ── glob_match semantics ─────────────────────────────────────────────

    #[test]
    fn glob_match_table() {
        let cases: &[(&str, &str, bool)] = &[
            // Old behavior preserved.
            ("*.rs", "a.rs", true),
            ("*.rs", "a.txt", false),
            ("a.rs", "a.rs", true),
            ("a.rs", "b.rs", false),
            ("pre*post", "prepost", true),
            ("pre*post", "preXYZpost", true),
            ("pre*post", "preX", false),
            // '?' — exactly one character.
            ("test?.rs", "test1.rs", true),
            ("test?.rs", "test10.rs", false),
            ("test?.rs", "test.rs", false),
            // Multiple '*'.
            ("a*b*c", "abc", true),
            ("a*b*c", "aXbYc", true),
            ("a*b*c", "aXcYb", false),
            ("src.*.rs", "src.foo.rs", true),
            ("src.*.rs", "src.rs", false),
            // Character classes.
            ("[ab]c", "ac", true),
            ("[ab]c", "bc", true),
            ("[ab]c", "cc", false),
            ("[a-z]x", "bx", true),
            ("[a-z]x", "Bx", false),
            ("[!a]b", "cb", true),
            ("[!a]b", "ab", false),
            // Unclosed '[' is a literal.
            ("[", "[", true),
            ("a[b", "a[b", true),
            ("a[b", "ab", false),
        ];
        for (pattern, name, want) in cases {
            assert_eq!(
                glob_match(pattern, name),
                *want,
                "glob_match({pattern:?}, {name:?})"
            );
        }
    }

    #[tokio::test]
    async fn find_files_supports_question_mark_glob() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test1.rs"), b"").unwrap();
        std::fs::write(dir.path().join("test10.rs"), b"").unwrap();
        let ws = ws_at(&dir);
        let req = FindRequest {
            pattern: "test?.rs".into(),
            path: WorkspacePath::project_root(),
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let (paths, _evidence) = ws.find_files(&req, 100, visibility).await.unwrap();
        let strs: Vec<_> = paths.iter().map(|p| p.as_str().to_string()).collect();
        assert_eq!(strs, vec!["test1.rs".to_string()]);
    }
}
