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
#[cfg(any(test, feature = "test-fault-injection"))]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::history::{WorkspaceHistory, WorkspaceHistoryError};
use regex::Regex;
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use crate::workspace::content_hash;

use super::types::{
    CreatedFile, DirectoryEntry, FindRequest, GrepMatch, GrepRequest, InstalledFile,
    LocalWorkspaceError, PathFacts, PathIntent, PathKind, WorkspacePath,
};
use super::visibility::{VisibilityEngine, VisibilityEvidence};

#[cfg(any(test, feature = "test-fault-injection"))]
type ReplaceBeforeCheck = Arc<Mutex<Option<(WorkspacePath, Vec<u8>)>>>;

/// The single concrete Local Workspace (ADR 0003 decision 3).
///
/// Internal deep module: secure path resolution, bounded read, atomic
/// installation, mutation history, and error normalization all live behind
/// this type. All Tool Host observation/mutation paths use it directly; there
/// is no workspace-backend trait or caller-supplied execution seam.
pub struct LocalWorkspace {
    /// Canonical project root. Captured at construction time; never mutates.
    root: PathBuf,
    /// Private durable history of successfully installed Atomic Local
    /// Mutations (GitHub #63). Lives in its own bare `gix` repository outside
    /// this workspace and outside the project `.git`; recording never touches
    /// workspace content.
    history: Arc<WorkspaceHistory>,
    /// Serializes LATTICE-owned replacement checks and installs. It does not
    /// claim to lock non-cooperating external writers (ADR 0003 decision 9),
    /// but keeps the final revision check and rename in one host operation.
    mutation_lock: Arc<Mutex<()>>,
    /// Serializes one complete LATTICE mutation transaction — installation,
    /// private-history append, and evidence publication — across every Host
    /// instance constructed over this workspace. This is separate from
    /// `mutation_lock` because conditional replacement acquires that narrower
    /// lock while the outer transaction gate is already held.
    mutation_sequence_gate: Arc<Mutex<()>>,
    /// One-shot install-failure flag for the narrow fault-injection seam
    /// (`fail_next_install`). Not present in production builds.
    #[cfg(any(test, feature = "test-fault-injection"))]
    fail_install: Arc<AtomicBool>,
    /// One-shot post-install parent-fsync failure for tests. This fires only
    /// after the target name has been atomically installed.
    #[cfg(any(test, feature = "test-fault-injection"))]
    fail_parent_fsync: Arc<AtomicBool>,
    /// One-shot external-writer simulation consumed immediately before a
    /// conditional replace performs its final Local Revision check.
    #[cfg(any(test, feature = "test-fault-injection"))]
    replace_before_check: ReplaceBeforeCheck,
}

/// Result of a read whose allocation is capped by the caller.
pub enum BoundedFileRead {
    Complete(Vec<u8>),
    LimitExceeded { observed: usize, allowed: usize },
}

/// Successful conditional replacement: exact pre-install bytes plus the
/// atomic install receipt used for history and evidence.
#[derive(Debug)]
pub(crate) struct RevisionMatchedInstall {
    pub before: Vec<u8>,
    pub installed: InstalledFile,
}

/// An atomic replacement receipt plus a possible failure to confirm the
/// installed directory entry's durability. The target is already changed
/// whenever `durability_error` is present.
struct AtomicInstallReceipt {
    installed: InstalledFile,
    durability_error: Option<LocalWorkspaceError>,
}

/// Failure vocabulary for the host-only conditional replacement primitive.
#[derive(Debug)]
pub(crate) enum ConditionalReplaceError {
    Workspace(LocalWorkspaceError),
    StaleRevision {
        observed: String,
    },
    Cancelled,
    InstalledButNotDurable {
        replacement: RevisionMatchedInstall,
        error: LocalWorkspaceError,
    },
}

#[derive(Debug)]
pub(crate) enum ConditionalCreateError {
    Workspace(LocalWorkspaceError),
    Cancelled,
    InstalledButNotDurable {
        created: CreatedFile,
        error: LocalWorkspaceError,
    },
}

impl From<LocalWorkspaceError> for ConditionalCreateError {
    fn from(error: LocalWorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<std::io::Error> for ConditionalCreateError {
    fn from(error: std::io::Error) -> Self {
        Self::Workspace(LocalWorkspaceError::Io(error))
    }
}

impl LocalWorkspace {
    /// Construct from a project root with mutation history recording off.
    ///
    /// Test-only: the production runtime always attaches the Turn's private
    /// history through [`LocalWorkspace::with_history`], so an installation
    /// path that records nothing cannot exist in a real turn. Gated on the
    /// The feature keeps the constructor absent from production binaries.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn new(root: &Path) -> Self {
        Self::with_history(root, Arc::new(WorkspaceHistory::disabled()))
    }

    /// Construct from a project root, canonicalizing it once, with a private
    /// mutation history attached.
    ///
    /// Falls back to the raw path if canonicalize fails (mirrors
    /// `CepPolicyConfig::defaults_for_project_root` tolerance).
    pub fn with_history(root: &Path, history: Arc<WorkspaceHistory>) -> Self {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Self {
            root: canonical,
            history,
            mutation_lock: Arc::new(Mutex::new(())),
            mutation_sequence_gate: Arc::new(Mutex::new(())),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_install: Arc::new(AtomicBool::new(false)),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_parent_fsync: Arc::new(AtomicBool::new(false)),
            #[cfg(any(test, feature = "test-fault-injection"))]
            replace_before_check: Arc::new(Mutex::new(None)),
        }
    }

    /// Canonical project root this workspace operates on.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Acquire the per-project `WorkspaceOperationLock` for this workspace
    /// (ADR 0005 §5).
    ///
    /// The lock belongs to the project binding the private history carries,
    /// so it is reached through the history handle. Observation takes it
    /// shared; a mutation takes it exclusively and holds it from the
    /// precondition read through install and history publication, which is
    /// strictly wider than the repository write lock the history append takes
    /// underneath. `Ok(None)` means recording is disabled and there is no
    /// project binding to serialize against.
    pub fn lock_operation(
        &self,
        mode: lattice_store::LockMode,
        blocking: lattice_store::Blocking,
    ) -> Result<Option<lattice_store::FlockGuard>, WorkspaceHistoryError> {
        self.history.lock_workspace_operation(mode, blocking)
    }

    /// Clone the shareable state for a `spawn_blocking` closure.
    fn blocking_clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            history: Arc::clone(&self.history),
            mutation_lock: Arc::clone(&self.mutation_lock),
            mutation_sequence_gate: Arc::clone(&self.mutation_sequence_gate),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_install: Arc::clone(&self.fail_install),
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_parent_fsync: Arc::clone(&self.fail_parent_fsync),
            #[cfg(any(test, feature = "test-fault-injection"))]
            replace_before_check: Arc::clone(&self.replace_before_check),
        }
    }

    /// Shared gate for one full Host mutation transaction.
    pub(crate) fn mutation_sequence_gate(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.mutation_sequence_gate)
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
    pub fn resolve_secure(
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
                                component: walk_component_for_error(&self.root, &cur),
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
    /// The Linux walk uses `O_RDONLY` directory opens: the root is opened once with
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
    /// # What this does NOT guarantee (see docs/archive/records/FINDINGS.md L-39-04)
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
    /// the whole path.
    /// The attacker must already hold concurrent write access to the project
    /// directory, which is why the residual is accepted rather than blocking.
    ///
    /// Symlink-shaped failures (`ELOOP`/`ENOTDIR`) map to
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
        // used to touch the filesystem, and it stays root-relative so a
        // reported component cannot carry the host path (#98).
        let mut cur = PathBuf::new();
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

    /// Per-segment secure resolution returning only the observed facts.
    ///
    /// Every observation entry point and synchronous Tool Host import funnels
    /// through this function, so path safety has one implementation.
    pub fn resolve_facts_blocking(
        &self,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Result<PathFacts, LocalWorkspaceError> {
        self.resolve_secure(path, intent).map(|(_, facts)| facts)
    }

    /// Read a file under the workspace, returning its bytes. Synchronous form.
    pub fn read_file_blocking(&self, path: &WorkspacePath) -> Result<Vec<u8>, LocalWorkspaceError> {
        path.require_file_path()?;
        let (full, facts) = self.resolve_secure(path, PathIntent::Read)?;
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
        Ok(fs::read(&full)?)
    }

    /// Read a regular file without ever allocating more than `allowed + 1`
    /// bytes. Metadata rejects an already-oversized file before opening it;
    /// the capped read is the post-check for a file that grows after resolution.
    pub fn read_file_bounded_blocking(
        &self,
        path: &WorkspacePath,
        allowed: usize,
    ) -> Result<BoundedFileRead, LocalWorkspaceError> {
        path.require_file_path()?;
        let (full, facts) = self.resolve_secure(path, PathIntent::Read)?;
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
        if let Some(size) = facts.byte_size
            && size as usize > allowed
        {
            return Ok(BoundedFileRead::LimitExceeded {
                observed: size as usize,
                allowed,
            });
        }

        let file = fs::File::open(full)?;
        let probe = allowed.saturating_add(1) as u64;
        let mut bytes = Vec::with_capacity(allowed.min(64 * 1024));
        file.take(probe).read_to_end(&mut bytes)?;
        if bytes.len() > allowed {
            Ok(BoundedFileRead::LimitExceeded {
                observed: bytes.len(),
                allowed,
            })
        } else {
            Ok(BoundedFileRead::Complete(bytes))
        }
    }

    /// Read a file under the workspace, returning its bytes.
    pub async fn read_file(&self, path: &WorkspacePath) -> Result<Vec<u8>, LocalWorkspaceError> {
        path.require_file_path()?;
        let ws = self.blocking_clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || ws.read_file_blocking(&path))
            .await
            .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// List directory entries directly beneath `path` (non-recursive).
    /// Synchronous form.
    pub fn list_dir_blocking(
        &self,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>, LocalWorkspaceError> {
        let (full, facts) = self.resolve_secure(path, PathIntent::ListDirectory)?;
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
    }

    /// List directory entries directly beneath `path` (non-recursive).
    pub async fn list_dir(
        &self,
        path: &WorkspacePath,
    ) -> Result<Vec<DirectoryEntry>, LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let path = path.clone();
        tokio::task::spawn_blocking(move || ws.list_dir_blocking(&path))
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
    pub fn grep_blocking(
        &self,
        req: &GrepRequest,
        max_results: usize,
        visibility: VisibilityEngine,
    ) -> Result<(Vec<GrepMatch>, VisibilityEvidence), LocalWorkspaceError> {
        self.grep_blocking_cancellable(req, max_results, visibility, || false)
    }

    pub(crate) fn grep_blocking_cancellable(
        &self,
        req: &GrepRequest,
        max_results: usize,
        mut visibility: VisibilityEngine,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<(Vec<GrepMatch>, VisibilityEvidence), LocalWorkspaceError> {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
        let (full, facts) = self.resolve_secure(&req.path, PathIntent::ListDirectory)?;
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
        let mut out = Vec::new();
        let mut budget = GrepBudget::default();
        grep_walk(
            &self.root,
            &full,
            &re,
            req.glob.as_deref(),
            max_results,
            &mut budget,
            &mut visibility,
            &mut out,
            &is_cancelled,
        )?;
        Ok((out, visibility.evidence()))
    }

    /// Grep a directory subtree, returning up to `max_results` matches.
    pub async fn grep(
        &self,
        req: &GrepRequest,
        max_results: usize,
        visibility: VisibilityEngine,
    ) -> Result<(Vec<GrepMatch>, VisibilityEvidence), LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let req = req.clone();
        tokio::task::spawn_blocking(move || ws.grep_blocking(&req, max_results, visibility))
            .await
            .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Find files matching a glob beneath a directory subtree. Synchronous form.
    ///
    /// Hidden paths are omitted from results and truncation counts.
    pub fn find_files_blocking(
        &self,
        req: &FindRequest,
        max_results: usize,
        visibility: VisibilityEngine,
    ) -> Result<(Vec<WorkspacePath>, VisibilityEvidence), LocalWorkspaceError> {
        self.find_files_blocking_cancellable(req, max_results, visibility, || false)
    }

    pub(crate) fn find_files_blocking_cancellable(
        &self,
        req: &FindRequest,
        max_results: usize,
        mut visibility: VisibilityEngine,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<(Vec<WorkspacePath>, VisibilityEvidence), LocalWorkspaceError> {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
        let (full, facts) = self.resolve_secure(&req.path, PathIntent::ListDirectory)?;
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
        let mut out = Vec::new();
        find_walk(
            &self.root,
            &full,
            &req.pattern,
            max_results,
            &mut visibility,
            &mut out,
            &is_cancelled,
        )?;
        Ok((out, visibility.evidence()))
    }

    /// Find files matching a glob beneath a directory subtree.
    ///
    /// Hidden paths are omitted from results and truncation counts.
    pub async fn find_files(
        &self,
        req: &FindRequest,
        max_results: usize,
        visibility: VisibilityEngine,
    ) -> Result<(Vec<WorkspacePath>, VisibilityEvidence), LocalWorkspaceError> {
        let ws = self.blocking_clone();
        let req = req.clone();
        tokio::task::spawn_blocking(move || ws.find_files_blocking(&req, max_results, visibility))
            .await
            .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Non-cancellable no-replace create; maps installation errors to workspace errors.
    pub fn create_excl_sync(
        &self,
        path: &WorkspacePath,
        content: &[u8],
        create_parents: bool,
    ) -> Result<CreatedFile, LocalWorkspaceError> {
        match self.create_excl_sync_cancellable(path, content, create_parents, || false) {
            Ok(created) => Ok(created),
            Err(ConditionalCreateError::Workspace(err)) => Err(err),
            Err(ConditionalCreateError::Cancelled) => Err(LocalWorkspaceError::Io(
                std::io::Error::other("unexpected cancellation in non-cancellable create"),
            )),
            Err(ConditionalCreateError::InstalledButNotDurable { error, .. }) => Err(error),
        }
    }

    /// Atomic no-replace create with a call-scoped cancellation probe checked
    /// before parent creation, before preparing the temporary inode, and
    /// immediately before the no-replace link installs the final name.
    pub(crate) fn create_excl_sync_cancellable(
        &self,
        path: &WorkspacePath,
        content: &[u8],
        create_parents: bool,
        is_cancelled: impl Fn() -> bool,
    ) -> Result<CreatedFile, ConditionalCreateError> {
        path.require_file_path()
            .map_err(LocalWorkspaceError::from)?;
        let (full, facts) = self.resolve_secure(path, PathIntent::Create)?;
        #[cfg(any(test, feature = "test-fault-injection"))]
        if self.fail_install.swap(false, Ordering::SeqCst) {
            return Err(
                LocalWorkspaceError::Io(std::io::Error::other("injected install failure")).into(),
            );
        }
        if facts.exists {
            return Err(LocalWorkspaceError::TargetExists {
                normalized: path.as_str().to_string(),
            }
            .into());
        }
        if !create_parents && !facts.parent_exists {
            return Err(LocalWorkspaceError::NotFound {
                normalized: path.as_str().to_string(),
            }
            .into());
        }
        let (parent_rel, leaf) = split_parent_leaf(&self.root, &full)?;
        if is_cancelled() {
            return Err(ConditionalCreateError::Cancelled);
        }
        let parent_fd = self.open_parent_dir_secure(&parent_rel, create_parents)?;
        if is_cancelled() {
            return Err(ConditionalCreateError::Cancelled);
        }
        let (tmp_name, tmp_fd) = claim_install_temp(&parent_fd)?;
        let mut tmp_file = fs::File::from(tmp_fd);
        let prepared = (|| -> Result<(), ConditionalCreateError> {
            tmp_file.write_all(content)?;
            tmp_file.sync_all()?;
            if is_cancelled() {
                return Err(ConditionalCreateError::Cancelled);
            }
            // POSIX linkat is an atomic no-replace name installation: a
            // concurrent target (including a symlink) yields EEXIST, while
            // the final name can never expose the partially written temp.
            rustix::fs::linkat(
                &parent_fd,
                tmp_name.as_str(),
                &parent_fd,
                leaf.as_os_str(),
                AtFlags::empty(),
            )
            .map_err(|err| {
                if err == Errno::EXIST {
                    ConditionalCreateError::Workspace(LocalWorkspaceError::TargetExists {
                        normalized: path.as_str().to_string(),
                    })
                } else {
                    ConditionalCreateError::Workspace(LocalWorkspaceError::Io(
                        std::io::Error::other(format!("atomic no-replace persist failed: {err}")),
                    ))
                }
            })?;
            Ok(())
        })();
        if let Err(err) = prepared {
            let _ = rustix::fs::unlinkat(&parent_fd, tmp_name.as_str(), AtFlags::empty());
            return Err(err);
        }
        drop(tmp_file);
        // The final target is already complete. Temp-name cleanup is
        // best-effort because reporting a failure here would falsely claim
        // that the installed mutation did not happen.
        let _ = rustix::fs::unlinkat(&parent_fd, tmp_name.as_str(), AtFlags::empty());
        let created = CreatedFile {
            path: path.clone(),
            content_hash: content_hash(content),
            bytes_written: content.len() as u64,
        };
        if let Err(error) = self.sync_parent_dir(&parent_fd) {
            return Err(ConditionalCreateError::InstalledButNotDurable { created, error });
        }
        Ok(created)
    }

    /// Non-cancellable atomic install through the pinned parent dirfd.
    /// The caller must already have checked the mutation precondition.
    pub fn install_atomic_sync(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<InstalledFile, LocalWorkspaceError> {
        let _guard = self.mutation_lock.lock().unwrap_or_else(|e| e.into_inner());
        let receipt = self.install_atomic_sync_locked(path, content)?;
        match receipt.durability_error {
            Some(error) => Err(error),
            None => Ok(receipt.installed),
        }
    }

    /// Host-only conditional replacement. The caller supplies the call-scoped
    /// cancellation probe; the concrete workspace owns the final revision
    /// check and atomic install under one LATTICE mutation lock.
    pub(crate) fn replace_if_revision_sync(
        &self,
        path: &WorkspacePath,
        expected_revision: &str,
        content: &[u8],
        is_cancelled: impl Fn() -> bool,
    ) -> Result<RevisionMatchedInstall, ConditionalReplaceError> {
        let _guard = self.mutation_lock.lock().unwrap_or_else(|e| e.into_inner());
        if is_cancelled() {
            return Err(ConditionalReplaceError::Cancelled);
        }

        #[cfg(any(test, feature = "test-fault-injection"))]
        if let Some((race_path, race_content)) = self.replace_before_check.lock().unwrap().take() {
            debug_assert_eq!(&race_path, path);
            fs::write(self.root.join(race_path.as_str()), race_content)
                .map_err(LocalWorkspaceError::from)
                .map_err(ConditionalReplaceError::Workspace)?;
        }

        let before = self
            .read_file_blocking(path)
            .map_err(ConditionalReplaceError::Workspace)?;
        let observed = content_hash(&before);
        if observed != expected_revision {
            return Err(ConditionalReplaceError::StaleRevision { observed });
        }
        if is_cancelled() {
            return Err(ConditionalReplaceError::Cancelled);
        }
        let receipt = self
            .install_atomic_sync_locked(path, content)
            .map_err(ConditionalReplaceError::Workspace)?;
        let replacement = RevisionMatchedInstall {
            before,
            installed: receipt.installed,
        };
        if let Some(error) = receipt.durability_error {
            return Err(ConditionalReplaceError::InstalledButNotDurable { replacement, error });
        }
        Ok(replacement)
    }

    /// Atomic install implementation. The caller holds `mutation_lock`.
    fn install_atomic_sync_locked(
        &self,
        path: &WorkspacePath,
        content: &[u8],
    ) -> Result<AtomicInstallReceipt, LocalWorkspaceError> {
        path.require_file_path()?;
        let ws = self;
        let (full, _facts) = ws.resolve_secure(path, PathIntent::Overwrite)?;
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
        let (tmp_name, tmp_fd) = claim_install_temp(&parent_fd)?;
        let mut tmp_file = fs::File::from(tmp_fd);
        let installed = (|| -> Result<(), LocalWorkspaceError> {
            tmp_file.write_all(content)?;
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
        let installed = InstalledFile {
            path: path.clone(),
            content_hash: content_hash(content),
            bytes_written: content.len() as u64,
        };
        let durability_error = self.sync_parent_dir(&parent_fd).err();
        Ok(AtomicInstallReceipt {
            installed,
            durability_error,
        })
    }

    /// Flip a one-shot flag so the next create or replacement fails before
    /// any bytes are written. Narrow test-only
    /// fault injection proving the install failure boundary leaves the
    /// target unchanged — a single boolean, not a composable plugin seam.
    #[cfg(any(test, feature = "test-fault-injection"))]
    pub fn fail_next_install(&self) {
        self.fail_install.store(true, Ordering::SeqCst);
    }

    /// Fail the next parent-directory fsync after the target was installed.
    #[cfg(any(test, feature = "test-fault-injection"))]
    pub fn fail_next_parent_fsync(&self) {
        self.fail_parent_fsync.store(true, Ordering::SeqCst);
    }

    /// Simulate a non-cooperating writer changing `path` immediately before
    /// the next conditional replacement's final revision check.
    #[cfg(any(test, feature = "test-fault-injection"))]
    pub fn replace_target_before_next_revision_check(&self, path: WorkspacePath, content: Vec<u8>) {
        *self.replace_before_check.lock().unwrap() = Some((path, content));
    }

    fn sync_parent_dir(&self, parent_fd: &OwnedFd) -> Result<(), LocalWorkspaceError> {
        #[cfg(any(test, feature = "test-fault-injection"))]
        if self.fail_parent_fsync.swap(false, Ordering::SeqCst) {
            return Err(LocalWorkspaceError::Io(std::io::Error::other(
                "injected parent fsync failure after install",
            )));
        }
        rustix::fs::fsync(parent_fd).map_err(|err| LocalWorkspaceError::Io(err.into()))
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

/// Claim a unique same-directory temporary file for atomic installation.
fn claim_install_temp(parent_fd: &OwnedFd) -> Result<(String, OwnedFd), LocalWorkspaceError> {
    let tmp_flags =
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut last = None;
    for _ in 0..INSTALL_TMP_NAME_ATTEMPTS {
        let candidate = format!(
            ".lattice-tmp-{}-{}",
            std::process::id(),
            INSTALL_TMP_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        match rustix::fs::openat(parent_fd, candidate.as_str(), tmp_flags, CREATE_FILE_MODE) {
            Ok(fd) => return Ok((candidate, fd)),
            Err(err) if err == Errno::EXIST => last = Some(err),
            Err(err) => return Err(LocalWorkspaceError::Io(err.into())),
        }
    }
    Err(LocalWorkspaceError::Io(std::io::Error::other(format!(
        "could not claim an install temp name after {INSTALL_TMP_NAME_ATTEMPTS} attempts: {}",
        last.map(|err| err.to_string()).unwrap_or_default()
    ))))
}

/// Directory-open flags for every step of the pinned dirfd walk: read-only,
/// directories only, and no symlink following.
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

/// Render a walk position for an error message with the workspace root
/// removed.
///
/// `SymlinkRejected` is not purely internal: `host_mutate` forwards its
/// `Display` text into the guest-visible `MutateError::PathRejected` reason,
/// so an absolute component would hand a Tool the project's real location on
/// disk through an error message (#98).
fn walk_component_for_error(root: &Path, cur: &Path) -> PathBuf {
    match cur.strip_prefix(root) {
        Ok(relative) => relative.to_path_buf(),
        // Unreachable — every walk starts at the root. Fall back to the leaf
        // name rather than `cur`, because the whole point is that the host
        // path must not reach the message.
        Err(_) => PathBuf::from(cur.file_name().unwrap_or_default()),
    }
}

/// Map a dirfd-walk errno: symlink-shaped failures — `ELOOP` from
/// `O_NOFOLLOW` and `ENOTDIR` from a non-directory swap — become
/// `SymlinkRejected`; the rest stay plain I/O. `cur` is already root-relative.
fn map_walk_errno(e: Errno, cur: &Path) -> LocalWorkspaceError {
    if e == Errno::LOOP || e == Errno::NOTDIR {
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
    is_cancelled: &impl Fn() -> bool,
) -> Result<(), LocalWorkspaceError> {
    if is_cancelled() {
        return Err(LocalWorkspaceError::Cancelled);
    }
    if out.len() >= max_results || budget.exhausted() {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
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
                    is_cancelled,
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
            grep_file(&rel, &path, re, max_results, budget, out, is_cancelled)?;
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
    is_cancelled: &impl Fn() -> bool,
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
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
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
    is_cancelled: &impl Fn() -> bool,
) -> Result<(), LocalWorkspaceError> {
    if is_cancelled() {
        return Err(LocalWorkspaceError::Cancelled);
    }
    if out.len() >= max_results {
        return Ok(());
    }
    for entry in fs::read_dir(current)? {
        if is_cancelled() {
            return Err(LocalWorkspaceError::Cancelled);
        }
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
            find_walk(
                workspace_root,
                &path,
                pattern,
                max_results,
                visibility,
                out,
                is_cancelled,
            )?;
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

    // 5. Expected-hash conflict: the caller checks the precondition; this
    //    test pins that the installed content hash matches a known blake3
    //    literal computed independently, so the precondition comparison is
    //    sound.
    #[test]
    fn installed_content_hash_matches_independent_blake3() {
        let dir = TempDir::new().unwrap();
        let body = b"lattice content hash";
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("h.txt").unwrap();
        let created = ws
            .create_excl_sync_cancellable(&path, body.as_ref(), false, || false)
            .unwrap();
        let want = format!("blake3:{}", blake3::hash(body).to_hex());
        assert_eq!(created.content_hash, want);
    }

    // Conditional replacement: the complete content is observable after installation.
    #[test]
    fn conditional_replace_installs_complete_target() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("r.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("r.txt").unwrap();
        let installed = ws
            .replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap()
            .installed;
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
    #[test]
    fn install_atomic_failure_leaves_no_partial_target() {
        let dir = TempDir::new().unwrap();
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        let mut perms = fs::metadata(&ro).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&ro, perms).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("ro/new.txt").unwrap();
        let err = {
            let _guard = ws.mutation_lock.lock().unwrap();
            ws.install_atomic_sync_locked(&path, b"x")
        };
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
    #[test]
    fn fail_next_install_leaves_target_unchanged_and_resets() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("f.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("f.txt").unwrap();

        ws.fail_next_install();
        let err = ws
            .replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap_err();
        assert!(
            matches!(
                err,
                ConditionalReplaceError::Workspace(LocalWorkspaceError::Io(_))
            ),
            "got {err:?}"
        );
        let on_disk = std::fs::read(dir.path().join("f.txt")).unwrap();
        assert_eq!(on_disk, b"v1", "failed install must not touch the target");

        // One-shot: the flag was consumed by the failed install.
        ws.replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap();
        let on_disk = std::fs::read(dir.path().join("f.txt")).unwrap();
        assert_eq!(on_disk, b"v2");
    }

    #[test]
    fn conditional_replace_rechecks_cancellation_at_install_point() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("target.txt"), b"before\n").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("target.txt").unwrap();
        let probes = std::sync::atomic::AtomicUsize::new(0);

        let err = ws
            .replace_if_revision_sync(&path, &content_hash(b"before\n"), b"after\n", || {
                probes.fetch_add(1, Ordering::SeqCst) > 0
            })
            .expect_err("cancellation arriving after the first probe must abort install");
        assert!(matches!(err, ConditionalReplaceError::Cancelled));
        assert_eq!(
            std::fs::read(dir.path().join("target.txt")).unwrap(),
            b"before\n"
        );
        assert_eq!(probes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn no_replace_create_cancelled_after_temp_sync_leaves_no_target_or_temp() {
        let dir = TempDir::new().unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("cancelled.txt").unwrap();
        let probes = std::sync::atomic::AtomicUsize::new(0);

        let err = ws
            .create_excl_sync_cancellable(&path, b"content\n", false, || {
                probes.fetch_add(1, Ordering::SeqCst) > 1
            })
            .expect_err("cancellation after temp sync must abort before no-replace install");
        assert!(matches!(err, ConditionalCreateError::Cancelled));
        assert!(!dir.path().join("cancelled.txt").exists());
        assert_eq!(probes.load(Ordering::SeqCst), 3);
        assert!(
            fs::read_dir(dir.path()).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lattice-tmp-")),
            "cancelled create must clean up its prepared temp file"
        );
    }

    // ── Supporting tests for observation operations ──────────────────────

    #[test]
    fn find_walk_observes_mid_traversal_cancellation() {
        let dir = TempDir::new().unwrap();
        for index in 0..32 {
            let child = dir.path().join(format!("d{index}"));
            std::fs::create_dir(&child).unwrap();
            std::fs::write(child.join("file.txt"), b"").unwrap();
        }
        let ws = ws_at(&dir);
        let request = FindRequest {
            pattern: "*".into(),
            path: WorkspacePath::project_root(),
        };
        let visibility = VisibilityEngine::load(dir.path(), Default::default());
        let probes = std::sync::atomic::AtomicUsize::new(0);
        let error = ws
            .find_files_blocking_cancellable(&request, 100, visibility, || {
                probes.fetch_add(1, Ordering::Relaxed) >= 3
            })
            .unwrap_err();
        assert!(matches!(error, LocalWorkspaceError::Cancelled));
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

    /// #98: `host_mutate` forwards this error's `Display` into the
    /// guest-visible `PathRejected` reason, so the reported component must be
    /// root-relative. Covers both construction sites — the `resolve_secure`
    /// walk and the pinned-dirfd walk in `open_parent_dir_secure`.
    #[test]
    fn symlink_rejection_reports_a_relative_component_not_the_host_path() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        symlink(outside.path(), dir.path().join("nested/link")).unwrap();
        let ws = ws_at(&dir);
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let root_str = root.to_str().expect("utf-8 temp path");

        let resolve_err = ws
            .resolve_secure(
                &WorkspacePath::parse("nested/link/x").unwrap(),
                PathIntent::Read,
            )
            .unwrap_err();
        let LocalWorkspaceError::SymlinkRejected { component } = &resolve_err else {
            panic!("expected SymlinkRejected, got {resolve_err:?}");
        };
        assert_eq!(component, Path::new("nested/link"));

        std::fs::create_dir(dir.path().join("swap")).unwrap();
        let swapped = ws_at(&dir);
        std::fs::remove_dir(dir.path().join("swap")).unwrap();
        symlink(outside.path(), dir.path().join("swap")).unwrap();
        let walk_err = swapped
            .open_parent_dir_secure(Path::new("swap"), false)
            .unwrap_err();
        let LocalWorkspaceError::SymlinkRejected { component } = &walk_err else {
            panic!("expected SymlinkRejected, got {walk_err:?}");
        };
        assert_eq!(component, Path::new("swap"));

        for err in [&resolve_err, &walk_err] {
            let message = err.to_string();
            assert!(
                !message.contains(root_str),
                "refusal leaked the workspace root: {message}"
            );
            assert!(
                !message.contains("/tmp"),
                "refusal leaked a host path: {message}"
            );
        }
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
    // byte-identical afterward for both write operations.
    #[test]
    fn write_paths_reject_symlink_parent_and_leave_outside_untouched() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("x"), b"keep").unwrap();
        symlink(outside.path(), dir.path().join("link")).unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("link/x").unwrap();

        let err = ws.create_excl_sync_cancellable(&path, b"evil".as_ref(), true, || false);
        assert!(
            matches!(
                err,
                Err(ConditionalCreateError::Workspace(
                    LocalWorkspaceError::SymlinkRejected { .. }
                ))
            ),
            "got {err:?}"
        );
        let err = ws.replace_if_revision_sync(&path, &content_hash(b"keep"), b"evil", || false);
        assert!(
            matches!(
                err,
                Err(ConditionalReplaceError::Workspace(
                    LocalWorkspaceError::SymlinkRejected { .. }
                ))
            ),
            "got {err:?}"
        );
        assert_eq!(
            std::fs::read(outside.path().join("x")).unwrap(),
            b"keep",
            "outside file must survive both rejected mutations"
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            1,
            "outside directory must gain no entries"
        );
    }

    // Temp hygiene: neither a forced install failure nor a successful
    // install may leave `.lattice-tmp-*` residue in the parent directory.
    #[test]
    fn conditional_replace_leaves_no_temp_residue() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("t.txt"), b"v1").unwrap();
        let ws = ws_at(&dir);
        let path = WorkspacePath::parse("t.txt").unwrap();

        ws.fail_next_install();
        ws.replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap_err();
        assert_eq!(tmp_residue(dir.path()), Vec::<String>::new());

        ws.replace_if_revision_sync(&path, &content_hash(b"v1"), b"v2", || false)
            .unwrap();
        assert_eq!(tmp_residue(dir.path()), Vec::<String>::new());
        assert_eq!(std::fs::read(dir.path().join("t.txt")).unwrap(), b"v2");
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
            &|| false,
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
            &|| false,
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
        grep_file(
            "a.txt",
            &a,
            &needle_re(),
            100,
            &mut budget,
            &mut out,
            &|| false,
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(budget.file_allowance(), 3, "61 of 64 total consumed");
        grep_file(
            "b.txt",
            &b,
            &needle_re(),
            100,
            &mut budget,
            &mut out,
            &|| false,
        )
        .unwrap();
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
        grep_file(
            "c.txt",
            &path,
            &needle_re(),
            100,
            &mut budget,
            &mut out,
            &|| false,
        )
        .unwrap();
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

    // ── glob_match semantics ─────────────────────────────────────────────
}
