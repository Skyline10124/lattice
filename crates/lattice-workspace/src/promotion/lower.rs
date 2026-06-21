// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Managed linked Lower worktree provisioning and four-part cleanliness checks.

use std::path::{Path, PathBuf};
use std::process::Command;

use thiserror::Error;

/// Errors surfaced by managed-Lower provisioning and cleanliness inspection.
#[derive(Debug, Error)]
pub enum PromotionError {
    #[error("invalid branch ref `{branch}`: {reason}")]
    InvalidBranchRef { branch: String, reason: String },
    #[error("branch `{branch}` is already checked out at `{path}`")]
    BranchAlreadyAttached { branch: String, path: PathBuf },
    #[error("managed worktree path mismatch: expected `{expected}`, found `{actual}`")]
    WorktreePathMismatch { expected: PathBuf, actual: PathBuf },
    #[error("lower worktree is dirty: {0:?}")]
    Dirty(LowerDirtyReason),
    #[error("git command failed: {context}: {stderr}")]
    GitCommand {
        context: String,
        stderr: String,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("git worktree add failed: {stderr}")]
    WorktreeAddFailed { stderr: String },
    #[error("verification mismatch: expected {expected}, got {actual}")]
    VerificationMismatch { expected: String, actual: String },
    #[error("baseline mismatch: expected {expected}, got {actual}")]
    BaselineMismatch { expected: String, actual: String },
    #[error("candidate artifact size {actual} exceeds limit {limit}")]
    ArtifactTooLarge { actual: u64, limit: u64 },
    #[error("malformed candidate bundle")]
    MalformedBundle,
    #[error("candidate objects fail connectivity checks")]
    ConnectivityFailure,
    #[error("baseline {baseline} is not an ancestor of candidate {candidate}")]
    NotDescendant { baseline: String, candidate: String },
    #[error("object count {count} exceeds limit {limit}")]
    ObjectCountExceeded { count: u64, limit: u64 },
    #[error("object size {size} exceeds limit {limit}")]
    ObjectSizeExceeded { size: u64, limit: u64 },
    #[error("total object bytes {total} exceeds limit {limit}")]
    TotalObjectBytesExceeded { total: u64, limit: u64 },
    #[error("commit count {count} exceeds limit {limit}")]
    CommitCountExceeded { count: u64, limit: u64 },
    #[error("materialize path count {count} exceeds limit {limit}")]
    MaterializePathLimitExceeded { count: u64, limit: u64 },
    #[error("candidate hazard: {0:?}")]
    CandidateHazard(crate::promotion::candidate::CandidateHazard),
    #[error("unsupported path encoding: {path_hex}")]
    UnsupportedPathEncoding { path_hex: String },
    #[error("invalid candidate path: {path}")]
    InvalidPath { path: String },
    #[error("materialize conflict at `{path}`")]
    MaterializeConflict { path: String },
    #[error("promotion journal already exists")]
    JournalExists,
    #[error("promotion journal malformed: {detail}")]
    JournalMalformed { detail: String },
    #[error("simulated promotion crash")]
    SimulatedCrash,
    #[error("git2 error: {0}")]
    Git(String),
    #[error("promotion recovery required before another commit")]
    RecoveryRequired,
    #[error("promotion recovery conflict: {detail}")]
    RecoveryConflict { detail: String },
    #[error("lower promotion lock is held by another process")]
    LockHeld,
    #[error("project baseline conflict: expected {expected}, actual {actual}")]
    BaselineConflict { expected: String, actual: String },
    #[error("session commit callback failed: {0}")]
    CallbackFailed(String),
    #[error("candidate ref already exists")]
    CandidateRefExists,
    #[error(transparent)]
    RefTransaction(#[from] super::refs::RefTransactionError),
}

/// Typed reason Lower failed a cleanliness inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerDirtyReason {
    DetachedHead,
    WrongBranch { expected: String, actual: String },
    StagedChanges,
    UnstagedChanges,
    UntrackedFiles,
    IndexDirty,
}

/// Observed project ref and baseline OID after a clean inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowerBaseline {
    pub project_ref: String,
    pub baseline_oid: String,
}

/// LATTICE-managed paths derived from the host repository common Git directory.
#[derive(Debug, Clone)]
pub struct PromotionPaths {
    pub git_common_dir: PathBuf,
    pub worktree_dir: PathBuf,
    pub promotion_state_dir: PathBuf,
    pub lock_path: PathBuf,
}

/// A managed linked Lower worktree sharing the project object database.
#[derive(Debug, Clone)]
pub struct ManagedLower {
    repository_path: PathBuf,
    branch: String,
    project_ref: String,
    paths: PromotionPaths,
}

impl ManagedLower {
    /// Provision a new managed linked worktree or open an existing one.
    ///
    /// Rejects branches already attached to a non-managed checkout. Never uses
    /// `--force` on `git worktree add`.
    pub fn provision_or_open(repository_path: impl AsRef<Path>, branch: &str) -> Result<Self, PromotionError> {
        let repository_path = repository_path.as_ref().canonicalize()?;
        let project_ref = format!("refs/heads/{branch}");

        validate_branch_ref(branch)?;

        let git_common_dir = git_common_dir(&repository_path)?;
        let paths = promotion_paths(&git_common_dir, branch);
        let worktrees = parse_worktree_list(&repository_path)?;

        if let Some(attached) = worktrees.iter().find(|wt| wt.branch.as_deref() == Some(project_ref.as_str())) {
            if attached.path != paths.worktree_dir {
                return Err(PromotionError::BranchAlreadyAttached {
                    branch: branch.to_owned(),
                    path: attached.path.clone(),
                });
            }
            return Ok(Self {
                repository_path,
                branch: branch.to_owned(),
                project_ref,
                paths,
            });
        }

        if paths.worktree_dir.exists() {
            if worktrees.iter().any(|wt| wt.path == paths.worktree_dir) {
                return Ok(Self {
                    repository_path,
                    branch: branch.to_owned(),
                    project_ref,
                    paths,
                });
            }
            return Err(PromotionError::WorktreePathMismatch {
                expected: paths.worktree_dir.clone(),
                actual: paths.worktree_dir.clone(),
            });
        }

        std::fs::create_dir_all(paths.worktree_dir.parent().expect("worktree parent"))?;

        let output = Command::new("git")
            .arg("-C")
            .arg(&repository_path)
            .args([
                "worktree",
                "add",
                paths.worktree_dir.to_str().expect("utf8 path"),
                branch,
            ])
            .output()?;

        if !output.status.success() {
            return Err(PromotionError::WorktreeAddFailed {
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }

        Ok(Self {
            repository_path,
            branch: branch.to_owned(),
            project_ref,
            paths,
        })
    }

    /// Path to the managed worktree checkout.
    pub fn worktree_path(&self) -> &Path {
        &self.paths.worktree_dir
    }

    /// Promotion state paths (lock, journal) for this Lower.
    pub fn promotion_paths(&self) -> &PromotionPaths {
        &self.paths
    }

    /// Host repository path.
    pub fn repository_path(&self) -> &Path {
        &self.repository_path
    }

    /// Target branch name (without `refs/heads/` prefix).
    pub fn branch(&self) -> &str {
        &self.branch
    }

    /// Run the four-part cleanliness inspection and return the baseline facts.
    pub fn inspect_clean(&self) -> Result<LowerBaseline, PromotionError> {
        self.check_clean()?;
        let baseline_oid = git_output(
            &self.paths.worktree_dir,
            &["rev-parse", "HEAD"],
            "rev-parse HEAD",
        )?;
        Ok(LowerBaseline {
            project_ref: self.project_ref.clone(),
            baseline_oid,
        })
    }

    fn check_clean(&self) -> Result<(), PromotionError> {
        let lower = &self.paths.worktree_dir;

        let head_ref = git_output(lower, &["symbolic-ref", "-q", "HEAD"], "symbolic-ref HEAD");
        match head_ref {
            Ok(actual) if actual == self.project_ref => {}
            Ok(actual) => {
                return Err(PromotionError::Dirty(LowerDirtyReason::WrongBranch {
                    expected: self.project_ref.clone(),
                    actual,
                }));
            }
            Err(PromotionError::GitCommand { .. }) => {
                return Err(PromotionError::Dirty(LowerDirtyReason::DetachedHead));
            }
            Err(other) => return Err(other),
        }

        if !git_stdout(lower, &["ls-files", "-u"], "ls-files unmerged")?.is_empty() {
            return Err(PromotionError::Dirty(LowerDirtyReason::IndexDirty));
        }

        if git_exit(
            lower,
            &[
                "diff-index",
                "--cached",
                "--quiet",
                "--ignore-submodules=none",
                "HEAD",
                "--",
            ],
            "diff-index cached",
        )? != 0
        {
            return Err(PromotionError::Dirty(LowerDirtyReason::StagedChanges));
        }

        if git_exit(
            lower,
            &["diff-files", "--quiet", "--ignore-submodules=none"],
            "diff-files",
        )? != 0
        {
            return Err(PromotionError::Dirty(LowerDirtyReason::UnstagedChanges));
        }

        let untracked = git_stdout(
            lower,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            "ls-files others",
        )?;
        if !untracked.is_empty() {
            return Err(PromotionError::Dirty(LowerDirtyReason::UntrackedFiles));
        }

        Ok(())
    }
}

fn validate_branch_ref(branch: &str) -> Result<(), PromotionError> {
    let full_ref = format!("refs/heads/{branch}");
    let output = Command::new("git")
        .args(["check-ref-format", &full_ref])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(PromotionError::InvalidBranchRef {
            branch: branch.to_owned(),
            reason: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn git_common_dir(repository_path: &Path) -> Result<PathBuf, PromotionError> {
    let raw = git_output(repository_path, &["rev-parse", "--git-common-dir"], "git-common-dir")?;
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(repository_path.join(path))
    }
}

pub(crate) fn promotion_paths(git_common_dir: &Path, branch: &str) -> PromotionPaths {
    let hex = hex_encode(branch);
    let worktree_dir = git_common_dir.join("lattice/worktrees").join(&hex);
    let promotion_state_dir = git_common_dir.join("lattice/promotion").join(&hex);
    let lock_path = promotion_state_dir.join("lower.lock");
    PromotionPaths {
        git_common_dir: git_common_dir.to_path_buf(),
        worktree_dir,
        promotion_state_dir,
        lock_path,
    }
}

fn hex_encode(input: &str) -> String {
    input.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

struct WorktreeEntry {
    path: PathBuf,
    branch: Option<String>,
}

fn parse_worktree_list(repository_path: &Path) -> Result<Vec<WorktreeEntry>, PromotionError> {
    let output = git_stdout(
        repository_path,
        &["worktree", "list", "--porcelain"],
        "worktree list",
    )?;
    let mut entries = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            if let Some(path) = current_path.take() {
                entries.push(WorktreeEntry {
                    path,
                    branch: current_branch.take(),
                });
            }
            current_path = Some(PathBuf::from(rest));
            current_branch = None;
        } else if let Some(rest) = line.strip_prefix("branch ") {
            current_branch = Some(rest.to_owned());
        }
    }
    if let Some(path) = current_path {
        entries.push(WorktreeEntry {
            path,
            branch: current_branch,
        });
    }
    Ok(entries)
}

fn git_output(cwd: &Path, args: &[&str], context: &str) -> Result<String, PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(PromotionError::GitCommand {
            context: context.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn git_stdout(cwd: &Path, args: &[&str], context: &str) -> Result<String, PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()?;
    if output.status.success() || output.status.code() == Some(1) {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(PromotionError::GitCommand {
            context: context.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn git_exit(cwd: &Path, args: &[&str], context: &str) -> Result<i32, PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()?;
    if output.status.success() {
        Ok(0)
    } else if let Some(code) = output.status.code() {
        Ok(code)
    } else {
        Err(PromotionError::GitCommand {
            context: context.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
