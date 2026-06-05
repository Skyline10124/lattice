// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Workspace snapshot helpers shared by command fallback and overlay planning.

use lancet_protocol::CepError;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const DEFAULT_SNAPSHOT_MAX_ENTRIES: usize = 100_000;
const DEFAULT_SNAPSHOT_MAX_DEPTH: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotLimits {
    pub(crate) max_entries: usize,
    pub(crate) max_depth: usize,
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_SNAPSHOT_MAX_ENTRIES,
            max_depth: DEFAULT_SNAPSHOT_MAX_DEPTH,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SnapshotFileKind {
    RegularFile,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotEntry {
    pub(crate) content_hash: Option<String>,
    pub(crate) file_kind: SnapshotFileKind,
    pub(crate) mode: Option<u32>,
    pub(crate) symlink_target: Option<String>,
    pub(crate) size_bytes: u64,
}

pub(crate) type WorkspaceSnapshot = BTreeMap<String, SnapshotEntry>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LowerBaseline {
    pub(crate) entries: WorkspaceSnapshot,
    pub(crate) regular_file_contents: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LowerRecheckOutcome {
    pub(crate) status: LowerConsistencyStatus,
    pub(crate) changed_paths: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LowerConsistencyStatus {
    Unchanged,
    DriftDetected,
}

impl LowerConsistencyStatus {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            LowerConsistencyStatus::Unchanged => "unchanged",
            LowerConsistencyStatus::DriftDetected => "drift_detected",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LowerFingerprint {
    pub(crate) entries: BTreeMap<String, LowerFingerprintEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LowerFingerprintEntry {
    pub(crate) file_kind: SnapshotFileKind,
    pub(crate) mode: Option<u32>,
    pub(crate) size_bytes: u64,
    pub(crate) modified_at: Option<SystemTime>,
    pub(crate) symlink_target: Option<String>,
}

pub(crate) fn capture_workspace_snapshot(
    project_root: impl AsRef<Path>,
) -> Result<WorkspaceSnapshot, CepError> {
    capture_workspace_snapshot_with_limits(project_root, SnapshotLimits::default())
}

pub(crate) fn capture_workspace_snapshot_with_limits(
    project_root: impl AsRef<Path>,
    limits: SnapshotLimits,
) -> Result<WorkspaceSnapshot, CepError> {
    let root = project_root.as_ref();
    let mut snapshot = BTreeMap::new();
    let mut entry_count = 0usize;
    capture_recursive_inner(root, root, &mut snapshot, None, limits, 0, &mut entry_count)?;
    Ok(snapshot)
}

fn capture_recursive_inner(
    base: &Path,
    current: &Path,
    snapshot: &mut WorkspaceSnapshot,
    mut regular_file_contents: Option<&mut BTreeMap<String, Vec<u8>>>,
    limits: SnapshotLimits,
    depth: usize,
    entry_count: &mut usize,
) -> Result<(), CepError> {
    if depth > limits.max_depth {
        return Err(snapshot_depth_limit_error(base, current, limits.max_depth));
    }

    let entries = std::fs::read_dir(current).map_err(|e| {
        CepError::internal_error(format!(
            "failed to read directory '{}': {e}",
            current.display()
        ))
    })?;

    for entry in entries {
        let entry =
            entry.map_err(|e| CepError::internal_error(format!("failed to read entry: {e}")))?;

        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        if is_snapshot_skipped_path(&relative) {
            continue;
        }

        *entry_count += 1;
        if *entry_count > limits.max_entries {
            return Err(snapshot_entry_limit_error(&relative, limits.max_entries));
        }

        let (entry_snapshot, content) = capture_snapshot_entry_with_content(&path)?;
        let is_dir = entry_snapshot.file_kind == SnapshotFileKind::Directory;
        if let (Some(contents), Some(content)) = (regular_file_contents.as_deref_mut(), content) {
            contents.insert(relative.clone(), content);
        }
        snapshot.insert(relative, entry_snapshot);
        if is_dir {
            capture_recursive_inner(
                base,
                &path,
                snapshot,
                regular_file_contents.as_deref_mut(),
                limits,
                depth + 1,
                entry_count,
            )?;
        }
    }

    Ok(())
}

fn capture_snapshot_entry(path: &Path) -> Result<SnapshotEntry, CepError> {
    capture_snapshot_entry_with_content(path).map(|(entry, _)| entry)
}

fn capture_snapshot_entry_with_content(
    path: &Path,
) -> Result<(SnapshotEntry, Option<Vec<u8>>), CepError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| {
        CepError::internal_error(format!(
            "failed to read metadata for '{}': {e}",
            path.display()
        ))
    })?;
    let file_type = metadata.file_type();
    let file_kind = snapshot_file_kind(&file_type);
    let content = if file_kind == SnapshotFileKind::RegularFile {
        Some(std::fs::read(path).map_err(|e| {
            CepError::internal_error(format!("failed to read file '{}': {e}", path.display()))
        })?)
    } else {
        None
    };
    let content_hash = content
        .as_ref()
        .map(|content| format!("blake3:{}", blake3::hash(content).to_hex()));
    let symlink_target = if file_kind == SnapshotFileKind::Symlink {
        Some(
            std::fs::read_link(path)
                .map_err(|e| {
                    CepError::internal_error(format!(
                        "failed to read symlink target for '{}': {e}",
                        path.display()
                    ))
                })?
                .to_string_lossy()
                .to_string(),
        )
    } else {
        None
    };

    Ok((
        SnapshotEntry {
            content_hash,
            file_kind,
            mode: normalized_mode(&metadata),
            symlink_target,
            size_bytes: metadata.len(),
        },
        content,
    ))
}

fn snapshot_file_kind(file_type: &std::fs::FileType) -> SnapshotFileKind {
    if file_type.is_symlink() {
        SnapshotFileKind::Symlink
    } else if file_type.is_dir() {
        SnapshotFileKind::Directory
    } else if file_type.is_file() {
        SnapshotFileKind::RegularFile
    } else {
        SnapshotFileKind::Other
    }
}

fn normalized_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    if metadata.is_file() || metadata.is_dir() {
        Some(platform_mode(metadata) & 0o777)
    } else {
        None
    }
}

#[cfg(unix)]
fn platform_mode(metadata: &std::fs::Metadata) -> u32 {
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn platform_mode(metadata: &std::fs::Metadata) -> u32 {
    if metadata.permissions().readonly() {
        0o555
    } else {
        0o755
    }
}

pub(crate) fn is_snapshot_skipped_path(relative: &str) -> bool {
    if relative == "target" || relative.starts_with("target/") {
        return true;
    }

    if relative == ".git" || relative == ".git/hooks" {
        return false;
    }

    if let Some(hook) = relative.strip_prefix(".git/hooks/") {
        return hook.ends_with(".sample");
    }

    relative.starts_with(".git/")
}

pub(crate) fn compute_changed_paths(
    pre: &WorkspaceSnapshot,
    post: &WorkspaceSnapshot,
) -> Vec<String> {
    let mut changed = Vec::new();

    for (path, post_snap) in post {
        match pre.get(path) {
            Some(pre_snap) => {
                if pre_snap != post_snap {
                    changed.push(path.clone());
                }
            }
            None => changed.push(path.clone()),
        }
    }

    for path in pre.keys() {
        if !post.contains_key(path) {
            changed.push(path.clone());
        }
    }

    changed
}

pub(crate) fn capture_lower_baseline(project_root: &Path) -> Result<LowerBaseline, CepError> {
    capture_lower_baseline_with_limits(project_root, SnapshotLimits::default())
}

pub(crate) fn capture_lower_baseline_with_limits(
    project_root: &Path,
    limits: SnapshotLimits,
) -> Result<LowerBaseline, CepError> {
    let mut entries = BTreeMap::new();
    let mut regular_file_contents = BTreeMap::new();
    let mut entry_count = 0usize;
    capture_recursive_inner(
        project_root,
        project_root,
        &mut entries,
        Some(&mut regular_file_contents),
        limits,
        0,
        &mut entry_count,
    )?;
    Ok(LowerBaseline {
        entries,
        regular_file_contents,
    })
}

pub(crate) fn recheck_lower_baseline(
    project_root: &Path,
    baseline: &LowerBaseline,
) -> Result<LowerRecheckOutcome, CepError> {
    let current = capture_workspace_snapshot_with_limits(project_root, SnapshotLimits::default())?;
    let changed_paths = compute_changed_paths(&baseline.entries, &current);
    let status = if changed_paths.is_empty() {
        LowerConsistencyStatus::Unchanged
    } else {
        LowerConsistencyStatus::DriftDetected
    };
    Ok(LowerRecheckOutcome {
        status,
        changed_paths,
    })
}

pub(crate) fn capture_lower_fingerprint(project_root: &Path) -> Result<LowerFingerprint, CepError> {
    capture_lower_fingerprint_with_limits(project_root, SnapshotLimits::default())
}

pub(crate) fn capture_lower_fingerprint_with_limits(
    project_root: &Path,
    limits: SnapshotLimits,
) -> Result<LowerFingerprint, CepError> {
    let mut entries = BTreeMap::new();
    let mut entry_count = 0usize;
    capture_fingerprint_recursive(
        project_root,
        project_root,
        &mut entries,
        limits,
        0,
        &mut entry_count,
    )?;
    Ok(LowerFingerprint { entries })
}

fn capture_fingerprint_recursive(
    base: &Path,
    current: &Path,
    entries: &mut BTreeMap<String, LowerFingerprintEntry>,
    limits: SnapshotLimits,
    depth: usize,
    entry_count: &mut usize,
) -> Result<(), CepError> {
    if depth > limits.max_depth {
        return Err(snapshot_depth_limit_error(base, current, limits.max_depth));
    }

    let dir_entries = std::fs::read_dir(current).map_err(|e| {
        CepError::internal_error(format!(
            "failed to read directory '{}': {e}",
            current.display()
        ))
    })?;

    for entry in dir_entries {
        let entry =
            entry.map_err(|e| CepError::internal_error(format!("failed to read entry: {e}")))?;
        let path = entry.path();
        let relative = path
            .strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();
        if is_snapshot_skipped_path(&relative) {
            continue;
        }

        *entry_count += 1;
        if *entry_count > limits.max_entries {
            return Err(snapshot_entry_limit_error(&relative, limits.max_entries));
        }

        let fingerprint = capture_fingerprint_entry(&path)?;
        let is_dir = fingerprint.file_kind == SnapshotFileKind::Directory;
        entries.insert(relative, fingerprint);
        if is_dir {
            capture_fingerprint_recursive(base, &path, entries, limits, depth + 1, entry_count)?;
        }
    }

    Ok(())
}

fn snapshot_entry_limit_error(relative: &str, max_entries: usize) -> CepError {
    CepError::internal_error(format!(
        "workspace snapshot entry limit exceeded at '{relative}': max_entries={max_entries}"
    ))
}

fn snapshot_depth_limit_error(base: &Path, current: &Path, max_depth: usize) -> CepError {
    let relative = current
        .strip_prefix(base)
        .unwrap_or(current)
        .to_string_lossy();
    CepError::internal_error(format!(
        "workspace snapshot depth limit exceeded at '{relative}': max_depth={max_depth}"
    ))
}

fn capture_fingerprint_entry(path: &Path) -> Result<LowerFingerprintEntry, CepError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|e| {
        CepError::internal_error(format!(
            "failed to read metadata for '{}': {e}",
            path.display()
        ))
    })?;
    let file_kind = snapshot_file_kind(&metadata.file_type());
    let symlink_target = if file_kind == SnapshotFileKind::Symlink {
        Some(
            std::fs::read_link(path)
                .map_err(|e| {
                    CepError::internal_error(format!(
                        "failed to read symlink target for '{}': {e}",
                        path.display()
                    ))
                })?
                .to_string_lossy()
                .to_string(),
        )
    } else {
        None
    };
    Ok(LowerFingerprintEntry {
        file_kind,
        mode: normalized_mode(&metadata),
        size_bytes: metadata.len(),
        modified_at: metadata.modified().ok(),
        symlink_target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn regular_entry(hash: &str) -> SnapshotEntry {
        SnapshotEntry {
            content_hash: Some(hash.to_string()),
            file_kind: SnapshotFileKind::RegularFile,
            mode: Some(0o644),
            symlink_target: None,
            size_bytes: 1,
        }
    }

    #[test]
    fn snapshot_capture_records_regular_file_hashes() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), "content").unwrap();

        let snapshot = capture_workspace_snapshot(dir.path()).unwrap();

        let entry = snapshot.get("file.txt").expect("file snapshot");
        assert!(entry.content_hash.is_some());
        assert_eq!(entry.file_kind, SnapshotFileKind::RegularFile);
    }

    #[test]
    fn snapshot_capture_fails_closed_when_entry_limit_is_exceeded() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("one.txt"), "1").unwrap();
        std::fs::write(dir.path().join("two.txt"), "2").unwrap();

        let err = capture_workspace_snapshot_with_limits(
            dir.path(),
            SnapshotLimits {
                max_entries: 1,
                max_depth: DEFAULT_SNAPSHOT_MAX_DEPTH,
            },
        )
        .unwrap_err();

        assert!(err.message.contains("entry limit exceeded"));
        assert!(err.message.contains("max_entries=1"));
    }

    #[test]
    fn snapshot_capture_fails_closed_when_depth_limit_is_exceeded() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/file.txt"), "content").unwrap();

        let err = capture_workspace_snapshot_with_limits(
            dir.path(),
            SnapshotLimits {
                max_entries: DEFAULT_SNAPSHOT_MAX_ENTRIES,
                max_depth: 0,
            },
        )
        .unwrap_err();

        assert!(err.message.contains("depth limit exceeded"));
        assert!(err.message.contains("max_depth=0"));
    }

    #[test]
    fn snapshot_capture_keeps_hidden_project_files_but_skips_git_and_target() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        std::fs::write(dir.path().join(".github/workflows/ci.yml"), "name: ci").unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]").unwrap();
        std::fs::write(dir.path().join("target/debug/out"), "build").unwrap();

        let snapshot = capture_workspace_snapshot(dir.path()).unwrap();

        assert!(snapshot.contains_key(".github/workflows/ci.yml"));
        assert!(!snapshot.contains_key(".git/config"));
        assert!(!snapshot.contains_key("target/debug/out"));
    }

    #[test]
    fn snapshot_capture_preserves_active_git_hooks_and_skips_samples() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
        std::fs::write(
            dir.path().join(".git/hooks/pre-commit.sample"),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".git/hooks/pre-commit"),
            "#!/bin/sh\ncargo fmt\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".git/objects/whatever"), "x").unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]").unwrap();

        let snapshot = capture_workspace_snapshot(dir.path()).unwrap();

        assert!(snapshot.contains_key(".git/hooks/pre-commit"));
        assert!(!snapshot.contains_key(".git/hooks/pre-commit.sample"));
        assert!(!snapshot.contains_key(".git/objects/whatever"));
        assert!(!snapshot.contains_key(".git/config"));
    }

    #[test]
    fn snapshot_skip_rules_cover_git_hooks_directory_itself() {
        assert!(is_snapshot_skipped_path("target"));
        assert!(is_snapshot_skipped_path("target/debug/out"));
        assert!(is_snapshot_skipped_path(".git/config"));
        assert!(!is_snapshot_skipped_path(".git"));
        assert!(!is_snapshot_skipped_path(".git/hooks"));
        assert!(!is_snapshot_skipped_path(".git/hooks/pre-commit"));
        assert!(is_snapshot_skipped_path(".git/hooks/pre-commit.sample"));
    }

    #[test]
    fn compute_changed_paths_detects_create_update_delete_and_type_change() {
        let mut pre = WorkspaceSnapshot::new();
        pre.insert("updated.txt".into(), regular_entry("old_hash"));
        pre.insert("deleted.txt".into(), regular_entry("deleted_hash"));
        pre.insert(
            "type-change".into(),
            SnapshotEntry {
                content_hash: None,
                file_kind: SnapshotFileKind::Directory,
                mode: Some(0o755),
                symlink_target: None,
                size_bytes: 0,
            },
        );

        let mut post = WorkspaceSnapshot::new();
        post.insert("created.txt".into(), regular_entry("created_hash"));
        post.insert("updated.txt".into(), regular_entry("new_hash"));
        post.insert("type-change".into(), regular_entry("file_hash"));

        let changed = compute_changed_paths(&pre, &post);

        assert_eq!(
            changed,
            vec!["created.txt", "type-change", "updated.txt", "deleted.txt"]
        );
    }

    #[test]
    fn lower_baseline_recheck_reports_unchanged_tree() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), "content").unwrap();

        let baseline = capture_lower_baseline(dir.path()).unwrap();
        let outcome = recheck_lower_baseline(dir.path(), &baseline).unwrap();

        assert_eq!(outcome.status, LowerConsistencyStatus::Unchanged);
        assert!(outcome.changed_paths.is_empty());
    }

    #[test]
    fn lower_baseline_recheck_detects_changed_file_content() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("file.txt"), "content").unwrap();
        let baseline = capture_lower_baseline(dir.path()).unwrap();

        std::fs::write(dir.path().join("file.txt"), "changed").unwrap();
        let outcome = recheck_lower_baseline(dir.path(), &baseline).unwrap();

        assert_eq!(outcome.status, LowerConsistencyStatus::DriftDetected);
        assert_eq!(outcome.changed_paths, vec!["file.txt"]);
    }

    #[test]
    fn lower_baseline_recheck_detects_deleted_and_created_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("deleted.txt"), "content").unwrap();
        let baseline = capture_lower_baseline(dir.path()).unwrap();

        std::fs::remove_file(dir.path().join("deleted.txt")).unwrap();
        std::fs::write(dir.path().join("created.txt"), "new").unwrap();
        let outcome = recheck_lower_baseline(dir.path(), &baseline).unwrap();

        assert_eq!(outcome.status, LowerConsistencyStatus::DriftDetected);
        assert_eq!(outcome.changed_paths, vec!["created.txt", "deleted.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn lower_baseline_recheck_detects_changed_symlink_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::TempDir::new().unwrap();
        symlink("first", dir.path().join("link")).unwrap();
        let baseline = capture_lower_baseline(dir.path()).unwrap();

        std::fs::remove_file(dir.path().join("link")).unwrap();
        symlink("second", dir.path().join("link")).unwrap();
        let outcome = recheck_lower_baseline(dir.path(), &baseline).unwrap();

        assert_eq!(outcome.status, LowerConsistencyStatus::DriftDetected);
        assert_eq!(outcome.changed_paths, vec!["link"]);
    }

    #[test]
    fn lower_baseline_recheck_detects_changed_file_type() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join("entry")).unwrap();
        let baseline = capture_lower_baseline(dir.path()).unwrap();

        std::fs::remove_dir(dir.path().join("entry")).unwrap();
        std::fs::write(dir.path().join("entry"), "file").unwrap();
        let outcome = recheck_lower_baseline(dir.path(), &baseline).unwrap();

        assert_eq!(outcome.status, LowerConsistencyStatus::DriftDetected);
        assert_eq!(outcome.changed_paths, vec!["entry"]);
    }

    #[test]
    fn lower_baseline_includes_untracked_files_without_git_repository() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("untracked.txt"), "content").unwrap();

        let baseline = capture_lower_baseline(dir.path()).unwrap();

        assert!(baseline.entries.contains_key("untracked.txt"));
    }

    #[test]
    fn lower_fingerprint_ignores_skipped_paths_and_detects_metadata_change_without_hashes() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("target/debug")).unwrap();
        std::fs::write(dir.path().join("tracked.txt"), "same content").unwrap();
        std::fs::write(dir.path().join("target/debug/out"), "ignored").unwrap();

        let before = capture_lower_fingerprint(dir.path()).unwrap();
        let mut permissions = std::fs::metadata(dir.path().join("tracked.txt"))
            .unwrap()
            .permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(dir.path().join("tracked.txt"), permissions).unwrap();
        let after = capture_lower_fingerprint(dir.path()).unwrap();

        assert!(!before.entries.contains_key("target/debug/out"));
        assert_ne!(before.entries["tracked.txt"], after.entries["tracked.txt"]);
    }

    #[test]
    fn lower_fingerprint_skips_git_internals_but_keeps_active_hooks() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]").unwrap();
        std::fs::write(dir.path().join(".git/hooks/pre-commit"), "#!/bin/sh\n").unwrap();
        std::fs::write(
            dir.path().join(".git/hooks/pre-commit.sample"),
            "#!/bin/sh\n",
        )
        .unwrap();

        let fingerprint = capture_lower_fingerprint(dir.path()).unwrap();

        assert!(!fingerprint.entries.contains_key(".git/config"));
        assert!(fingerprint.entries.contains_key(".git/hooks/pre-commit"));
        assert!(!fingerprint
            .entries
            .contains_key(".git/hooks/pre-commit.sample"));
    }
}
