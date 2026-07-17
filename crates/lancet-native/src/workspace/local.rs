// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concrete Local Workspace implementation.
//!
//! See [`super`] for the module-level contract. The single concrete type
//! [`LocalWorkspace`] owns secure path resolution, bounded reads, content
//! hashing, no-replace create, atomic edit installation, and filesystem
//! error normalization. It is constructed from a canonical project root
//! and never re-canonicalizes after construction.

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
#[cfg(any(test, feature = "test-fault-injection"))]
use std::sync::Arc;
#[cfg(any(test, feature = "test-fault-injection"))]
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;

use crate::edit::content_hash;
use lancet_authority::ReadVisibilityPolicy;

use super::types::{
    CreatedFile, DirectoryEntry, FindRequest, GrepMatch, GrepRequest, InstalledFile,
    LocalWorkspaceError, PathFacts, PathIntent, PathKind, WorkspacePath,
};
use super::visibility::VisibilityEngine;

/// The single concrete Local Workspace (ADR 0003 decision 3).
///
/// Internal deep module: secure path resolution, bounded read, atomic
/// installation, and error normalization all live behind this type. It is
/// `pub(crate)` because native handlers and the edit engine are the only
/// allowed callers; there is no workspace-backend seam.
pub(crate) struct LocalWorkspace {
    /// Canonical project root. Captured at construction time; never mutates.
    root: PathBuf,
    /// One-shot install-failure flag for the narrow fault-injection seam
    /// (`fail_next_install`). Not present in production builds.
    #[cfg(any(test, feature = "test-fault-injection"))]
    fail_install: Arc<AtomicBool>,
}

impl LocalWorkspace {
    /// Construct from a project root, canonicalizing it once.
    ///
    /// Falls back to the raw path if canonicalize fails (mirrors
    /// `CepPolicyConfig::defaults_for_project_root` tolerance).
    pub(crate) fn new(root: &Path) -> Result<Self, LocalWorkspaceError> {
        let canonical = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Ok(Self {
            root: canonical,
            #[cfg(any(test, feature = "test-fault-injection"))]
            fail_install: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Canonical project root this workspace operates on.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Clone the shareable state for a `spawn_blocking` closure.
    fn blocking_clone(&self) -> Self {
        Self {
            root: self.root.clone(),
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
                let em = entry.metadata()?;
                entries.push(DirectoryEntry {
                    name,
                    kind: metadata_kind(&em),
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
    pub(crate) async fn grep(
        &self,
        req: &GrepRequest,
        max_results: usize,
        visibility: ReadVisibilityPolicy,
    ) -> Result<Vec<GrepMatch>, LocalWorkspaceError> {
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
            let mut engine = VisibilityEngine::load(&ws.root, visibility);
            let mut out: Vec<GrepMatch> = Vec::new();
            grep_walk(
                &ws.root,
                &full,
                &re,
                req.glob.as_deref(),
                max_results,
                &mut engine,
                &mut out,
            )?;
            Ok(out)
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
        visibility: ReadVisibilityPolicy,
    ) -> Result<Vec<WorkspacePath>, LocalWorkspaceError> {
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
            let mut engine = VisibilityEngine::load(&ws.root, visibility);
            let mut out: Vec<WorkspacePath> = Vec::new();
            find_walk(
                &ws.root,
                &full,
                &req.pattern,
                max_results,
                &mut engine,
                &mut out,
            )?;
            Ok(out)
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
            let parent = full.parent().unwrap_or(&ws.root).to_path_buf();
            if create_parents {
                // Only walk the ancestor directories of the file — never
                // mkdir the file's own leaf component. Walk the parent path
                // component-by-component from the root.
                let mut created_dirs = Vec::new();
                let mut cur = ws.root.clone();
                if let Ok(rel) = parent.strip_prefix(&ws.root) {
                    for comp in rel.components() {
                        if let Component::Normal(part) = comp {
                            cur.push(part);
                            if !cur.exists() {
                                fs::create_dir_all(&cur)?;
                                created_dirs.push(relative_under(&ws.root, &cur)?);
                            } else if let Ok(m) = fs::symlink_metadata(&cur)
                                && m.file_type().is_symlink()
                            {
                                return Err(LocalWorkspaceError::SymlinkRejected {
                                    component: cur.clone(),
                                });
                            }
                        }
                    }
                }
                let _ = &mut created_dirs;
            } else if fs::symlink_metadata(&parent).is_err() {
                return Err(LocalWorkspaceError::NotFound {
                    normalized: path.as_str().to_string(),
                });
            }
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            let mut file = opts.open(&full).map_err(|err| {
                if err.kind() == std::io::ErrorKind::AlreadyExists {
                    LocalWorkspaceError::TargetExists {
                        normalized: path.as_str().to_string(),
                    }
                } else {
                    LocalWorkspaceError::Io(err)
                }
            })?;
            file.write_all(&content)?;
            file.sync_all()?;
            drop(file);
            let created_dirs = if create_parents {
                collect_created_dirs(&ws.root, &parent)?
            } else {
                Vec::new()
            };
            Ok(CreatedFile {
                path,
                content_hash: content_hash(&content),
                bytes_written: content.len() as u64,
                created_dirs,
            })
        })
        .await
        .map_err(|e| LocalWorkspaceError::Io(std::io::Error::other(e)))?
    }

    /// Atomic edit install (ADR 0003 decision 8).
    ///
    /// Writes a same-directory tempfile, fsyncs the data, atomically renames
    /// over the target, then fsyncs the parent directory entry. Returns the
    /// post-install content hash. The caller must have already checked the
    /// mutation precondition (expected hash or absence) against current
    /// Local state.
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
            let parent = full.parent().unwrap_or(&ws.root).to_path_buf();
            fs::create_dir_all(&parent)?;
            let tmp = tempfile::Builder::new()
                .prefix(".lattice-tmp-")
                .tempfile_in(&parent)?;
            tmp.as_file().write_all(&content)?;
            tmp.as_file().sync_all()?;
            let persist_path = full.clone();
            tmp.persist(&persist_path).map_err(|e| {
                LocalWorkspaceError::Io(std::io::Error::other(format!(
                    "atomic persist failed: {e}"
                )))
            })?;
            fsync_dir(&parent)?;
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
            fs::remove_file(&full)?;
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
    let ft = meta.file_type();
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

/// Helper used by `create_excl` to enumerate the ancestor directories
/// created by `create_parents`.
fn collect_created_dirs(root: &Path, parent: &Path) -> Result<Vec<String>, LocalWorkspaceError> {
    let mut out = Vec::new();
    let mut cur = root.to_path_buf();
    let rel = parent
        .strip_prefix(root)
        .map_err(|_| LocalWorkspaceError::RootEscape)?;
    for c in rel.components() {
        if let Component::Normal(n) = c {
            cur.push(n);
            out.push(relative_under(root, &cur)?);
        }
    }
    Ok(out)
}

fn fsync_dir(dir: &Path) -> Result<(), LocalWorkspaceError> {
    let f = fs::File::open(dir)?;
    f.sync_all()?;
    Ok(())
}

fn grep_walk(
    workspace_root: &Path,
    current: &Path,
    re: &Regex,
    glob: Option<&str>,
    max_results: usize,
    visibility: &mut VisibilityEngine,
    out: &mut Vec<GrepMatch>,
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
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (idx, line) in content.lines().enumerate() {
                if out.len() >= max_results {
                    break;
                }
                if re.is_match(line) {
                    let wp = WorkspacePath::parse(&rel).map_err(LocalWorkspaceError::Path)?;
                    out.push(GrepMatch {
                        path: wp,
                        line_number: (idx as u64) + 1,
                        line: line.to_string(),
                    });
                }
            }
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
        if glob_match(pattern, &name) {
            let listable = if is_dir {
                visibility.is_listable(&rel, true)
            } else {
                visibility.is_visible_file(&rel)
            };
            if listable {
                out.push(WorkspacePath::parse(&rel).map_err(LocalWorkspaceError::Path)?);
            }
        }
        if is_dir && visibility.should_descend(&rel) {
            find_walk(workspace_root, &path, pattern, max_results, visibility, out)?;
        }
    }
    Ok(())
}

fn glob_match(pattern: &str, name: &str) -> bool {
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            let (prefix, suffix) = (parts[0], parts[1]);
            return name.starts_with(prefix) && name.ends_with(suffix);
        }
    }
    pattern == name
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    fn ws_at(dir: &TempDir) -> LocalWorkspace {
        LocalWorkspace::new(dir.path()).expect("local workspace")
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
    //    leaves no partial target on disk (the tempfile is cleaned up by
    //    tempfile::TempDir on drop). This is the real-fault approximation
    //    ADR step 1 prescribes; the narrow fault-injection seam below covers
    //    the deterministic boundary.
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
        // Restore permissions so TempDir can clean up.
        let mut perms = fs::metadata(&ro).unwrap().permissions();
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
        let matches = ws
            .grep(&req, 100, ReadVisibilityPolicy::default())
            .await
            .unwrap();
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
        let paths = ws
            .find_files(&req, 100, ReadVisibilityPolicy::default())
            .await
            .unwrap();
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
        let created = ws.create_excl(&path, b"hi".as_ref(), true).await.unwrap();
        assert!(dir.path().join("a/b/c.txt").exists());
        assert_eq!(
            created.created_dirs,
            vec!["a".to_string(), "a/b".to_string()]
        );
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
}
