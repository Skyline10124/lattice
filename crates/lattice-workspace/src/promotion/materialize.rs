// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Bounded tree-diff materialization plan and durable per-path Lower effects.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use git2::{ObjectType, Oid, Repository};
use serde::{Deserialize, Serialize};

use super::candidate::PromotionLimits;
use super::PromotionError;

/// Git tree entry state recorded in the materialization journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathState {
    Absent,
    Blob { oid: String, mode: u32 },
    Symlink { oid: String },
    Directory,
}

/// One journaled path transition from baseline to candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializeEntry {
    pub path: String,
    pub old: PathState,
    pub new: PathState,
}

/// Immutable, bounded baseline→candidate path plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializationPlan {
    pub baseline_oid: String,
    pub candidate_oid: String,
    entries: Vec<MaterializeEntry>,
}

impl MaterializationPlan {
    pub fn entries(&self) -> &[MaterializeEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Outcome of comparing on-disk state to journaled old/new states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClassification {
    MatchesOld,
    MatchesNew,
    ThirdState,
}

/// Records durability operations for tests.
pub trait DurableFsRecorder: Send + Sync {
    fn record_file_sync(&self, path: &Path);
    fn record_parent_sync(&self, path: &Path);
}

/// Durable filesystem effects with parent-directory fsync.
pub struct DurableFs<'a> {
    worktree_root: &'a Path,
    repo: &'a Repository,
    recorder: Option<&'a dyn DurableFsRecorder>,
}

impl<'a> DurableFs<'a> {
    pub fn new(
        worktree_root: &'a Path,
        repo: &'a Repository,
        recorder: Option<&'a dyn DurableFsRecorder>,
    ) -> Self {
        Self {
            worktree_root,
            repo,
            recorder,
        }
    }

    pub fn apply_entry(&mut self, entry: &MaterializeEntry) -> Result<(), PromotionError> {
        let classification = classify_path(
            self.worktree_root,
            self.repo,
            &entry.path,
            &entry.old,
            &entry.new,
        )?;
        match classification {
            PathClassification::MatchesNew => Ok(()),
            PathClassification::MatchesOld => self.apply_transition(entry),
            PathClassification::ThirdState => Err(PromotionError::MaterializeConflict {
                path: entry.path.clone(),
            }),
        }
    }

    fn apply_transition(&mut self, entry: &MaterializeEntry) -> Result<(), PromotionError> {
        let abs = self.worktree_root.join(&entry.path);
        match (&entry.old, &entry.new) {
            (PathState::Symlink { .. }, PathState::Absent) => {
                fs::remove_file(&abs)?;
                self.sync_parent(&abs)?;
                self.try_remove_empty_parents(&entry.path)?;
            }
            (PathState::Directory, PathState::Absent) => {
                self.rmdir_if_empty(&abs)?;
            }
            (_, PathState::Absent) => {
                if matches!(entry.old, PathState::Blob { .. })
                    && entry.path.contains('/')
                    && let Some(parent) = abs.parent()
                {
                    let sibling_count = fs::read_dir(parent)?
                        .filter_map(Result::ok)
                        .count();
                    if sibling_count > 1 {
                        return Err(PromotionError::MaterializeConflict {
                            path: entry.path.clone(),
                        });
                    }
                }
                remove_path_kind(&entry.path, &abs, &entry.old)?;
                self.sync_parent(&abs)?;
                self.try_remove_empty_parents(&entry.path)?;
            }
            (PathState::Absent, PathState::Directory) => {
                self.mkdir(&abs)?;
            }
            (PathState::Absent, PathState::Blob { oid, mode }) => {
                self.write_blob(&abs, oid, *mode)?;
            }
            (PathState::Blob { .. }, PathState::Blob { oid, mode }) => {
                self.write_blob(&abs, oid, *mode)?;
            }
            (PathState::Absent, PathState::Symlink { .. }) | (_, PathState::Symlink { .. }) => {
                return Err(PromotionError::Git(
                    "symlink creation or retargeting is forbidden".into(),
                ));
            }
            (PathState::Directory, PathState::Directory) => {}
            _ => {
                return Err(PromotionError::Git(format!(
                    "unsupported transition for {}: {:?} -> {:?}",
                    entry.path, entry.old, entry.new
                )));
            }
        }
        Ok(())
    }

    fn write_blob(&mut self, abs: &Path, oid: &str, mode: u32) -> Result<(), PromotionError> {
        if let Some(parent) = abs.parent() {
            self.mkdir(parent)?;
        }
        let content = blob_bytes(self.repo, oid)?;
        let tmp = staged_temp_path(abs);
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&content)?;
            file.sync_all()?;
            self.record_file_sync(&tmp);
            set_mode(&tmp, mode)?;
        }
        fs::rename(&tmp, abs)?;
        self.sync_parent(abs)?;
        set_mode(abs, mode)?;
        Ok(())
    }

    fn mkdir(&mut self, abs: &Path) -> Result<(), PromotionError> {
        if abs.exists() {
            if abs.is_dir() {
                return Ok(());
            }
            return Err(PromotionError::MaterializeConflict {
                path: rel_path_from_root(self.worktree_root, abs),
            });
        }
        if let Some(parent) = abs.parent()
            && !parent.exists()
        {
            self.mkdir(parent)?;
        }
        fs::create_dir(abs)?;
        sync_path(abs, self.recorder, true)?;
        self.sync_parent(abs)?;
        Ok(())
    }

    fn rmdir_if_empty(&mut self, abs: &Path) -> Result<(), PromotionError> {
        if !abs.exists() {
            return Ok(());
        }
        if !abs.is_dir() {
            return Err(PromotionError::MaterializeConflict {
                path: rel_path_from_root(self.worktree_root, abs),
            });
        }
        if fs::read_dir(abs)?.next().is_some() {
            return Err(PromotionError::MaterializeConflict {
                path: rel_path_from_root(self.worktree_root, abs),
            });
        }
        fs::remove_dir(abs)?;
        self.sync_parent(abs)?;
        Ok(())
    }

    fn try_remove_empty_parents(&mut self, rel_path: &str) -> Result<(), PromotionError> {
        let mut current = PathBuf::from(rel_path);
        while current.pop() {
            let abs = self.worktree_root.join(&current);
            if !abs.exists() {
                continue;
            }
            if !abs.is_dir() {
                break;
            }
            if fs::read_dir(&abs)?.next().is_some() {
                break;
            }
            fs::remove_dir(&abs)?;
            self.sync_parent(&abs)?;
        }
        Ok(())
    }

    fn sync_parent(&self, path: &Path) -> Result<(), PromotionError> {
        if let Some(parent) = path.parent() {
            sync_path(parent, self.recorder, false)?;
            if let Some(rec) = self.recorder {
                rec.record_parent_sync(parent);
            }
        }
        Ok(())
    }

    fn record_file_sync(&self, path: &Path) {
        if let Some(rec) = self.recorder {
            rec.record_file_sync(path);
        }
    }
}

/// Build the immutable baseline→candidate path plan without touching Lower.
pub fn build_materialization_plan(
    repository_path: &Path,
    baseline_oid: &str,
    candidate_oid: &str,
    limits: &PromotionLimits,
) -> Result<MaterializationPlan, PromotionError> {
    let repo = Repository::open(repository_path).map_err(|e| PromotionError::Git(e.to_string()))?;
    let baseline_tree = commit_tree(&repo, baseline_oid)?;
    let candidate_tree = commit_tree(&repo, candidate_oid)?;

    let mut baseline_paths = HashMap::new();
    let mut candidate_paths = HashMap::new();
    collect_tree_paths(&repo, &baseline_tree, "", &mut baseline_paths)?;
    collect_tree_paths(&repo, &candidate_tree, "", &mut candidate_paths)?;

    let mut keys: HashSet<String> = baseline_paths.keys().chain(candidate_paths.keys()).cloned().collect();
    let mut entries = Vec::new();
    for path in keys.drain() {
        let old = baseline_paths.remove(&path).unwrap_or(PathState::Absent);
        let new = candidate_paths.remove(&path).unwrap_or(PathState::Absent);
        if old == new {
            continue;
        }
        if matches!(new, PathState::Symlink { .. }) {
            return Err(PromotionError::Git(
                "candidate symlink paths are rejected before materialization".into(),
            ));
        }
        entries.push(MaterializeEntry { path, old, new });
    }

    let count = entries.len() as u64;
    if count > limits.max_materialize_paths {
        return Err(PromotionError::MaterializePathLimitExceeded {
            count,
            limit: limits.max_materialize_paths,
        });
    }

    sort_plan_entries(&mut entries);
    Ok(MaterializationPlan {
        baseline_oid: baseline_oid.to_owned(),
        candidate_oid: candidate_oid.to_owned(),
        entries,
    })
}

/// Classify the current Lower path against journaled old/new states.
pub fn classify_path(
    worktree_root: &Path,
    repo: &Repository,
    rel_path: &str,
    old: &PathState,
    new: &PathState,
) -> Result<PathClassification, PromotionError> {
    let current = read_current_path_state(worktree_root, repo, rel_path)?;
    if state_eq(&current, new, worktree_root, repo, rel_path)? {
        Ok(PathClassification::MatchesNew)
    } else if state_eq(&current, old, worktree_root, repo, rel_path)? {
        Ok(PathClassification::MatchesOld)
    } else {
        Ok(PathClassification::ThirdState)
    }
}

fn sort_plan_entries(entries: &mut [MaterializeEntry]) {
    entries.sort_by(|a, b| {
        let a_delete = matches!(a.new, PathState::Absent);
        let b_delete = matches!(b.new, PathState::Absent);
        let a_create = matches!(a.old, PathState::Absent);
        let b_create = matches!(b.old, PathState::Absent);
        match (a_delete, b_delete) {
            (true, true) => path_depth(&b.path).cmp(&path_depth(&a.path)),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => match (a_create, b_create) {
                (true, true) => path_depth(&a.path).cmp(&path_depth(&b.path)),
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                (false, false) => a.path.cmp(&b.path),
            },
        }
    });
}

fn path_depth(path: &str) -> usize {
    path.matches('/').count()
}

fn commit_tree<'a>(repo: &'a Repository, oid: &str) -> Result<git2::Tree<'a>, PromotionError> {
    let obj = repo
        .revparse_single(oid)
        .map_err(|e| PromotionError::Git(format!("revparse {oid}: {e}")))?;
    let commit = obj
        .peel_to_commit()
        .map_err(|e| PromotionError::Git(format!("peel commit {oid}: {e}")))?;
    commit
        .tree()
        .map_err(|e| PromotionError::Git(format!("tree {oid}: {e}")))
}

fn collect_tree_paths(
    repo: &Repository,
    tree: &git2::Tree<'_>,
    prefix: &str,
    out: &mut HashMap<String, PathState>,
) -> Result<(), PromotionError> {
    for entry in tree.iter() {
        let name = entry
            .name()
            .ok_or_else(|| PromotionError::UnsupportedPathEncoding {
                path_hex: hex_encode(entry.name_bytes()),
            })?;
        let rel = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.kind() {
            Some(ObjectType::Blob) => {
                let oid = entry.id().to_string();
                if entry.filemode() == 0o120000 {
                    out.insert(rel, PathState::Symlink { oid });
                } else {
                    out.insert(
                        rel,
                        PathState::Blob {
                            oid,
                            mode: entry.filemode() as u32,
                        },
                    );
                }
            }
            Some(ObjectType::Tree) => {
                out.insert(rel.clone(), PathState::Directory);
                let subtree_obj = entry
                    .to_object(repo)
                    .map_err(|e| PromotionError::Git(e.to_string()))?;
                let subtree = subtree_obj
                    .as_tree()
                    .ok_or_else(|| PromotionError::Git("expected tree".into()))?;
                collect_tree_paths(repo, subtree, &rel, out)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Read the current Lower path state for recovery replay.
pub(crate) fn read_current_path_state(
    worktree_root: &Path,
    repo: &Repository,
    rel_path: &str,
) -> Result<PathState, PromotionError> {
    let abs = worktree_root.join(rel_path);
    if !abs.exists() {
        return Ok(PathState::Absent);
    }
    #[cfg(unix)]
    {
        let meta = fs::symlink_metadata(&abs)?;
        if meta.is_symlink() {
            let target = fs::read_link(&abs)?;
            let target_str = target.to_str().ok_or_else(|| PromotionError::InvalidPath {
                path: rel_path.to_owned(),
            })?;
            let oid = hash_bytes(repo, target_str.as_bytes())?;
            return Ok(PathState::Symlink { oid });
        }
    }
    if abs.is_dir() {
        return Ok(PathState::Directory);
    }
    let bytes = fs::read(&abs)?;
    let oid = hash_bytes(repo, &bytes)?;
    let mode = file_mode(&abs)?;
    Ok(PathState::Blob { oid, mode })
}

fn state_eq(
    left: &PathState,
    right: &PathState,
    worktree_root: &Path,
    repo: &Repository,
    rel_path: &str,
) -> Result<bool, PromotionError> {
    match (left, right) {
        (PathState::Absent, PathState::Absent) => Ok(true),
        (PathState::Directory, PathState::Directory) => Ok(true),
        (PathState::Blob { oid: a, mode: ma }, PathState::Blob { oid: b, mode: mb }) => {
            Ok(a == b && ma == mb && blob_bytes(repo, a)? == fs::read(worktree_root.join(rel_path))?)
        }
        (PathState::Symlink { oid: a }, PathState::Symlink { oid: b }) => {
            Ok(a == b && blob_bytes(repo, a)? == symlink_target_bytes(worktree_root, rel_path)?)
        }
        _ => Ok(false),
    }
}

fn blob_bytes(repo: &Repository, oid: &str) -> Result<Vec<u8>, PromotionError> {
    let parsed = Oid::from_str(oid).map_err(|e| PromotionError::Git(format!("parse oid {oid}: {e}")))?;
    let blob = repo
        .find_blob(parsed)
        .map_err(|e| PromotionError::Git(format!("find blob {oid}: {e}")))?;
    Ok(blob.content().to_vec())
}

fn hash_bytes(_repo: &Repository, bytes: &[u8]) -> Result<String, PromotionError> {
    Oid::hash_object(ObjectType::Blob, bytes)
        .map(|oid| oid.to_string())
        .map_err(|e| PromotionError::Git(format!("hash object: {e}")))
}

#[cfg(unix)]
fn file_mode(path: &Path) -> Result<u32, PromotionError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o111 != 0 {
        Ok(0o100755)
    } else {
        Ok(0o100644)
    }
}

#[cfg(not(unix))]
fn file_mode(_path: &Path) -> Result<u32, PromotionError> {
    Ok(0o100644)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), PromotionError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    let file_mode = if mode == 0o100755 { 0o755 } else { 0o644 };
    perms.set_mode(file_mode);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), PromotionError> {
    Ok(())
}

fn symlink_target_bytes(worktree_root: &Path, rel_path: &str) -> Result<Vec<u8>, PromotionError> {
    let target = fs::read_link(worktree_root.join(rel_path))?;
    let s = target.to_str().ok_or_else(|| PromotionError::InvalidPath {
        path: rel_path.to_owned(),
    })?;
    Ok(s.as_bytes().to_vec())
}

fn remove_path_kind(rel_path: &str, abs: &Path, old: &PathState) -> Result<(), PromotionError> {
    if !abs.exists() {
        return Ok(());
    }
    match old {
        PathState::Directory => {
            if abs.is_dir() {
                if fs::read_dir(abs)?.next().is_some() {
                    return Err(PromotionError::MaterializeConflict {
                        path: rel_path.to_owned(),
                    });
                }
                fs::remove_dir(abs)?;
            } else {
                return Err(PromotionError::MaterializeConflict {
                    path: rel_path.to_owned(),
                });
            }
        }
        PathState::Symlink { .. } | PathState::Blob { .. } => {
            fs::remove_file(abs)?;
        }
        PathState::Absent => {}
    }
    Ok(())
}

fn staged_temp_path(dest: &Path) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let file_name = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    parent.join(format!(".lattice-stage-{file_name}"))
}

fn sync_path(path: &Path, recorder: Option<&dyn DurableFsRecorder>, file: bool) -> Result<(), PromotionError> {
    let opened = File::open(path)?;
    opened.sync_all()?;
    if file && let Some(rec) = recorder {
        rec.record_file_sync(path);
    }
    Ok(())
}

fn rel_path_from_root(root: &Path, abs: &Path) -> String {
    abs.strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| abs.display().to_string())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub struct VecDurableRecorder {
    pub file_syncs: std::sync::Mutex<Vec<PathBuf>>,
    pub parent_syncs: std::sync::Mutex<Vec<PathBuf>>,
}

impl Default for VecDurableRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl VecDurableRecorder {
    pub fn new() -> Self {
        Self {
            file_syncs: std::sync::Mutex::new(Vec::new()),
            parent_syncs: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl DurableFsRecorder for VecDurableRecorder {
    fn record_file_sync(&self, path: &Path) {
        self.file_syncs.lock().unwrap().push(path.to_path_buf());
    }

    fn record_parent_sync(&self, path: &Path) {
        self.parent_syncs.lock().unwrap().push(path.to_path_buf());
    }
}
