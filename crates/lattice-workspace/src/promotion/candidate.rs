// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Candidate bundle import validation, resource limits, and hazard detection.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use super::PromotionError;

/// Host default promotion resource limits (resolved constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromotionLimits {
    pub max_artifact_bytes: u64,
    pub max_object_count: u64,
    pub max_object_bytes: u64,
    pub max_total_object_bytes: u64,
    pub max_commit_count: u64,
    pub max_materialize_paths: u64,
}

impl Default for PromotionLimits {
    fn default() -> Self {
        Self {
            max_artifact_bytes: 128 * 1024 * 1024,
            max_object_count: 100_000,
            max_object_bytes: 64 * 1024 * 1024,
            max_total_object_bytes: 512 * 1024 * 1024,
            max_commit_count: 10_000,
            max_materialize_paths: 100_000,
        }
    }
}

/// Caller-supplied verification evidence with structured binding.
///
/// Use [`VerificationResult::new`] to construct; the constructor rejects
/// empty argv, empty profile hash, and incoherent pass/exit semantics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationResult {
    pub candidate_oid: String,
    pub argv: Vec<String>,
    pub profile_hash: String,
    pub expected_exit_code: i32,
    pub exit_code: Option<i32>,
    pub passed: bool,
    pub summary: String,
}

impl VerificationResult {
    /// Construct validated evidence, rejecting malformed or incoherent inputs.
    ///
    /// - `argv` must be non-empty.
    /// - `profile_hash` must be non-empty.
    /// - `passed` may be true only when `exit_code == Some(expected_exit_code)`.
    pub fn new(
        candidate_oid: String,
        argv: Vec<String>,
        profile_hash: String,
        expected_exit_code: i32,
        exit_code: Option<i32>,
        passed: bool,
        summary: String,
    ) -> Result<Self, super::PromotionError> {
        if argv.is_empty() {
            return Err(super::PromotionError::InvalidEvidence {
                reason: "argv must not be empty".into(),
            });
        }
        if profile_hash.is_empty() {
            return Err(super::PromotionError::InvalidEvidence {
                reason: "profile_hash must not be empty".into(),
            });
        }
        if passed && exit_code != Some(expected_exit_code) {
            return Err(super::PromotionError::InvalidEvidence {
                reason: format!(
                    "passed=true requires exit_code=Some({expected_exit_code}), got {exit_code:?}"
                ),
            });
        }
        Ok(Self {
            candidate_oid,
            argv,
            profile_hash,
            expected_exit_code,
            exit_code,
            passed,
            summary,
        })
    }
}

/// Typed hazard discovered while scanning candidate-range tree transitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateHazard {
    GitmodulesChanged { path: String },
    SymlinkAdded { path: String },
    SymlinkRetargeted { path: String },
    DotGitEntry { path: String },
    GitlinkAdded { path: String },
    GitlinkRemoved { path: String },
    GitlinkRetargeted { path: String },
}

/// Human-readable review fact emitted during validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateReviewFact {
    Hazard(CandidateHazard),
    ObjectCount { count: u64, limit: u64 },
    CommitCount { count: u64, limit: u64 },
}

/// A bounded, connected, hazard-free candidate ready for later promotion stages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCandidate {
    pub baseline_oid: String,
    pub candidate_oid: String,
    pub artifact_len: u64,
    pub review_facts: Vec<CandidateReviewFact>,
    pub verification_argv: Vec<String>,
    pub verification_profile_hash: String,
    pub verification_expected_exit_code: i32,
    pub verification_exit_code: Option<i32>,
}

/// Validate verification binding, import the bundle, and scan the candidate range.
///
/// Does not acquire locks, move refs, or write Lower.
pub fn validate_candidate(
    repository_path: &Path,
    export: &crate::CandidateExport,
    baseline_oid: &str,
    verification: &VerificationResult,
    limits: &PromotionLimits,
) -> Result<ValidatedCandidate, PromotionError> {
    if export.artifact_len() > limits.max_artifact_bytes {
        return Err(PromotionError::ArtifactTooLarge {
            actual: export.artifact_len(),
            limit: limits.max_artifact_bytes,
        });
    }

    if export.baseline_oid() != baseline_oid {
        return Err(PromotionError::BaselineMismatch {
            expected: baseline_oid.to_owned(),
            actual: export.baseline_oid().to_owned(),
        });
    }

    // Evidence binding validation before any bundle import or Lower effect.
    if verification.candidate_oid != export.candidate_oid() {
        return Err(PromotionError::VerificationMismatch {
            expected: export.candidate_oid().to_owned(),
            actual: verification.candidate_oid.clone(),
        });
    }
    if !verification.passed {
        return Err(PromotionError::VerificationMismatch {
            expected: export.candidate_oid().to_owned(),
            actual: verification.candidate_oid.clone(),
        });
    }
    if verification.exit_code != Some(verification.expected_exit_code) {
        return Err(PromotionError::InvalidEvidence {
            reason: format!(
                "passed=true requires exit_code=Some({}), got {:?}",
                verification.expected_exit_code, verification.exit_code,
            ),
        });
    }
    if verification.argv.is_empty() || verification.profile_hash.is_empty() {
        return Err(PromotionError::InvalidEvidence {
            reason: "argv and profile_hash must not be empty".into(),
        });
    }

    run_git(
        repository_path,
        [
            "bundle",
            "verify",
            export.path().to_str().expect("utf8 path"),
        ],
        "bundle verify",
    )
    .map_err(|_| PromotionError::MalformedBundle)?;

    run_git(
        repository_path,
        [
            "bundle",
            "unbundle",
            export.path().to_str().expect("utf8 path"),
        ],
        "bundle unbundle",
    )
    .map_err(|_| PromotionError::MalformedBundle)?;

    run_git(
        repository_path,
        ["fsck", "--strict", export.candidate_oid()],
        "fsck strict",
    )
    .map_err(|_| PromotionError::ConnectivityFailure)?;

    if !git_is_ancestor(repository_path, baseline_oid, export.candidate_oid())? {
        return Err(PromotionError::NotDescendant {
            baseline: baseline_oid.to_owned(),
            candidate: export.candidate_oid().to_owned(),
        });
    }

    let object_stats = enumerate_objects(
        repository_path,
        baseline_oid,
        export.candidate_oid(),
        limits,
    )?;
    scan_commit_hazards(
        repository_path,
        baseline_oid,
        export.candidate_oid(),
        limits,
        &object_stats,
    )?;

    Ok(ValidatedCandidate {
        baseline_oid: baseline_oid.to_owned(),
        candidate_oid: export.candidate_oid().to_owned(),
        artifact_len: export.artifact_len(),
        review_facts: vec![],
        verification_argv: verification.argv.clone(),
        verification_profile_hash: verification.profile_hash.clone(),
        verification_expected_exit_code: verification.expected_exit_code,
        verification_exit_code: verification.exit_code,
    })
}

struct ObjectStats;

fn enumerate_objects(
    repository_path: &Path,
    baseline_oid: &str,
    candidate_oid: &str,
    limits: &PromotionLimits,
) -> Result<ObjectStats, PromotionError> {
    let range = format!("{baseline_oid}..{candidate_oid}");
    let mut rev_list = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args(["rev-list", "--objects", "--no-object-names", &range])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(PromotionError::Io)?;

    let mut cat_file = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args([
            "cat-file",
            "--batch-check=%(objectname) %(objecttype) %(objectsize)",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(PromotionError::Io)?;

    let mut rev_out = rev_list.stdout.take().expect("rev-list stdout");
    let mut cat_in = cat_file.stdin.take().expect("cat-file stdin");
    // Capture the pipe-copy result so an early cat-file death (or a
    // copy/panic) is not silently discarded. A truncated pipe that still
    // exits 0 would otherwise produce incomplete object enumeration and could
    // let an over-limit candidate pass validation.
    let copy_outcome = std::thread::scope(|scope| {
        let handle = scope.spawn(|| std::io::copy(&mut rev_out, &mut cat_in));
        handle.join()
    });
    drop(cat_in);
    match copy_outcome {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(PromotionError::Io(e)),
        Err(_) => {
            return Err(PromotionError::Io(std::io::Error::other(
                "object enumeration pipe thread panicked",
            )));
        }
    }

    let rev_status = rev_list.wait().map_err(PromotionError::Io)?;
    if !rev_status.success() {
        return Err(PromotionError::ConnectivityFailure);
    }

    let cat_out = cat_file.wait_with_output().map_err(PromotionError::Io)?;
    if !cat_out.status.success() {
        return Err(PromotionError::ConnectivityFailure);
    }

    let mut object_count = 0u64;
    let mut total_bytes = 0u64;
    for line in BufReader::new(cat_out.stdout.as_slice()).lines() {
        let line = line.map_err(PromotionError::Io)?;
        if line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _oid = parts.next();
        let obj_type = parts.next().unwrap_or("");
        let size: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
        object_count = object_count.saturating_add(1);
        if object_count > limits.max_object_count {
            return Err(PromotionError::ObjectCountExceeded {
                count: object_count,
                limit: limits.max_object_count,
            });
        }
        if obj_type != "commit" && size > limits.max_object_bytes {
            return Err(PromotionError::ObjectSizeExceeded {
                size,
                limit: limits.max_object_bytes,
            });
        }
        total_bytes = total_bytes.saturating_add(size);
        if total_bytes > limits.max_total_object_bytes {
            return Err(PromotionError::TotalObjectBytesExceeded {
                total: total_bytes,
                limit: limits.max_total_object_bytes,
            });
        }
    }

    Ok(ObjectStats)
}

fn scan_commit_hazards(
    repository_path: &Path,
    baseline_oid: &str,
    candidate_oid: &str,
    limits: &PromotionLimits,
    _stats: &ObjectStats,
) -> Result<(), PromotionError> {
    let range = format!("{baseline_oid}..{candidate_oid}");
    let commits = git_lines(repository_path, &["rev-list", &range], "rev-list commits")?;

    if commits.len() as u64 > limits.max_commit_count {
        return Err(PromotionError::CommitCountExceeded {
            count: commits.len() as u64,
            limit: limits.max_commit_count,
        });
    }

    let repo =
        git2::Repository::open(repository_path).map_err(|e| PromotionError::Git(e.to_string()))?;

    let mut materialize_paths = 0u64;
    for commit_oid in commits {
        let commit = repo
            .find_commit(
                git2::Oid::from_str(&commit_oid).map_err(|e| PromotionError::Git(e.to_string()))?,
            )
            .map_err(|e| PromotionError::Git(e.to_string()))?;
        let commit_tree = commit
            .tree()
            .map_err(|e| PromotionError::Git(e.to_string()))?;

        for parent in commit.parents() {
            let parent_tree = parent
                .tree()
                .map_err(|e| PromotionError::Git(e.to_string()))?;
            let mut diff_opts = git2::DiffOptions::new();
            diff_opts.include_typechange(true);
            diff_opts.include_typechange_trees(true);
            let diff = repo
                .diff_tree_to_tree(Some(&parent_tree), Some(&commit_tree), Some(&mut diff_opts))
                .map_err(|e| PromotionError::Git(e.to_string()))?;

            let mut scan_err: Option<PromotionError> = None;
            diff.foreach(
                &mut |delta, _| {
                    if let Err(err) = inspect_delta(delta) {
                        scan_err = Some(err);
                        return false;
                    }
                    true
                },
                None,
                None,
                None,
            )
            .ok(); // callback stop uses User error; scan_err carries the real hazard
            if let Some(err) = scan_err {
                return Err(err);
            }
        }

        materialize_paths = materialize_paths.saturating_add(count_tree_paths(&commit_tree)?);
        if materialize_paths > limits.max_materialize_paths {
            return Err(PromotionError::MaterializePathLimitExceeded {
                count: materialize_paths,
                limit: limits.max_materialize_paths,
            });
        }
    }

    Ok(())
}

fn count_tree_paths(tree: &git2::Tree) -> Result<u64, PromotionError> {
    let mut count = 0u64;
    let mut path_err: Option<PromotionError> = None;
    tree.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
        if let Err(e) = validate_path_bytes(root, entry.name_bytes()) {
            path_err = Some(e);
            return git2::TreeWalkResult::Abort;
        }
        count = count.saturating_add(1);
        git2::TreeWalkResult::Ok
    })
    .map_err(|e| PromotionError::Git(e.to_string()))?;
    if let Some(e) = path_err {
        return Err(e);
    }
    Ok(count)
}

fn inspect_delta(delta: git2::DiffDelta) -> Result<(), PromotionError> {
    let old = delta.old_file();
    let new = delta.new_file();

    let old_path = path_from_delta_file(old.path(), old.mode());
    let new_path = path_from_delta_file(new.path(), new.mode());

    if let Some(path) = new_path.as_ref().or(old_path.as_ref()) {
        validate_utf8_relative_path(path)?;
        if path.ends_with(".gitmodules") || path == ".gitmodules" {
            match delta.status() {
                git2::Delta::Added
                | git2::Delta::Modified
                | git2::Delta::Deleted
                | git2::Delta::Renamed => {
                    return Err(PromotionError::CandidateHazard(
                        CandidateHazard::GitmodulesChanged { path: path.clone() },
                    ));
                }
                _ => {}
            }
        }
        if path.split('/').any(|c| c == ".git") {
            return Err(PromotionError::CandidateHazard(
                CandidateHazard::DotGitEntry { path: path.clone() },
            ));
        }
    }

    let old_mode = old.mode();
    let new_mode = new.mode();
    let old_gitlink = is_gitlink_mode(old_mode);
    let new_gitlink = is_gitlink_mode(new_mode);
    let old_symlink = is_symlink_mode(old_mode);
    let new_symlink = is_symlink_mode(new_mode);

    if new_gitlink && !old_gitlink {
        return Err(PromotionError::CandidateHazard(
            CandidateHazard::GitlinkAdded {
                path: new_path.clone().unwrap_or_default(),
            },
        ));
    }
    if old_gitlink && !new_gitlink {
        return Err(PromotionError::CandidateHazard(
            CandidateHazard::GitlinkRemoved {
                path: old_path.clone().unwrap_or_default(),
            },
        ));
    }
    if old_gitlink && new_gitlink && old.id() != new.id() {
        return Err(PromotionError::CandidateHazard(
            CandidateHazard::GitlinkRetargeted {
                path: new_path.clone().unwrap_or_default(),
            },
        ));
    }

    if new_symlink && !old_symlink {
        return Err(PromotionError::CandidateHazard(
            CandidateHazard::SymlinkAdded {
                path: new_path.clone().unwrap_or_default(),
            },
        ));
    }
    if old_symlink && new_symlink && old.id() != new.id() {
        return Err(PromotionError::CandidateHazard(
            CandidateHazard::SymlinkRetargeted {
                path: new_path.clone().unwrap_or_default(),
            },
        ));
    }

    Ok(())
}

fn path_from_delta_file(path: Option<&Path>, _mode: git2::FileMode) -> Option<String> {
    path.and_then(|p| p.to_str().map(String::from))
}

fn is_gitlink_mode(mode: git2::FileMode) -> bool {
    mode == git2::FileMode::Commit
}

fn is_symlink_mode(mode: git2::FileMode) -> bool {
    mode == git2::FileMode::Link
}

fn validate_path_bytes(root: &str, name: &[u8]) -> Result<(), PromotionError> {
    let name_str =
        std::str::from_utf8(name).map_err(|_| PromotionError::UnsupportedPathEncoding {
            path_hex: hex_encode_bytes(name),
        })?;
    let full = if root.is_empty() {
        name_str.to_owned()
    } else {
        format!("{root}/{name_str}")
    };
    validate_utf8_relative_path(&full)
}

fn validate_utf8_relative_path(path: &str) -> Result<(), PromotionError> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return Err(PromotionError::InvalidPath { path: path.into() });
    }
    if path.split('/').any(|c| c == ".." || c.is_empty()) {
        return Err(PromotionError::InvalidPath { path: path.into() });
    }
    Ok(())
}

fn hex_encode_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn git_is_ancestor(
    repository_path: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map_err(PromotionError::Io)?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(PromotionError::GitCommand {
            context: "merge-base --is-ancestor".into(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }),
    }
}

fn git_lines(
    repository_path: &Path,
    args: &[&str],
    context: &str,
) -> Result<Vec<String>, PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args(args)
        .output()
        .map_err(PromotionError::Io)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    } else {
        Err(PromotionError::GitCommand {
            context: context.into(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

fn run_git(
    repository_path: &Path,
    args: impl IntoIterator<Item = impl AsRef<std::ffi::OsStr>>,
    context: &str,
) -> Result<(), PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args(args)
        .output()
        .map_err(PromotionError::Io)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(PromotionError::GitCommand {
            context: context.into(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// A candidate durably pinned by a repository ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedCandidate {
    pub candidate_oid: String,
    pub candidate_ref: String,
    pub baseline_oid: String,
}

/// Pin a candidate object by creating a durable ref.
///
/// Uses create-or-verify semantics: first pin creates the ref; subsequent
/// calls with the same OID succeed idempotently; a different OID is rejected
/// as a substitution attempt.
pub fn pin_candidate(
    repository_path: &Path,
    candidate_oid: &str,
    session_id: &super::RefComponent,
    turn_id: &super::RefComponent,
) -> Result<PinnedCandidate, PromotionError> {
    // Verify the candidate OID exists as a commit.
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args(["cat-file", "-t", candidate_oid])
        .output()
        .map_err(PromotionError::Io)?;
    if !output.status.success() {
        return Err(PromotionError::GitCommand {
            context: "cat-file -t candidate".into(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let obj_type = String::from_utf8_lossy(&output.stdout);
    if obj_type.trim() != "commit" {
        return Err(PromotionError::GitCommand {
            context: "pin candidate".into(),
            stderr: format!("expected commit, got {}", obj_type.trim()),
        });
    }

    let candidate_ref = format!(
        "refs/lattice/candidates/{}/{}",
        session_id.as_str(),
        turn_id.as_str()
    );

    // Check if ref already exists.
    let existing = resolve_candidate_ref(repository_path, &candidate_ref)?;
    match existing {
        Some(oid) if oid == candidate_oid => {
            // Idempotent: same OID already pinned.
        }
        Some(oid) => {
            // Substitution: different OID.
            return Err(PromotionError::CandidateSubstitution {
                expected: oid,
                actual: candidate_oid.to_owned(),
            });
        }
        None => {
            // Create the ref.
            let output = Command::new("git")
                .arg("-C")
                .arg(repository_path)
                .args(["update-ref", &candidate_ref, candidate_oid])
                .output()
                .map_err(PromotionError::Io)?;
            if !output.status.success() {
                return Err(PromotionError::GitCommand {
                    context: "update-ref pin candidate".into(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                });
            }
        }
    }

    // Determine baseline by finding the merge-base with refs/heads/main or the
    // first parent of the candidate that is on the project branch.
    // For simplicity, use the ref target as candidate and resolve baseline from
    // the first parent chain.
    let baseline_oid = find_baseline_oid(repository_path, candidate_oid)?;

    Ok(PinnedCandidate {
        candidate_oid: candidate_oid.to_owned(),
        candidate_ref,
        baseline_oid,
    })
}

/// Load and validate a pinned candidate from its ref.
///
/// Performs full validation (ancestry, object closure, hazards, limits)
/// without requiring a bundle — the objects are already in the repository.
pub fn load_pinned_candidate(
    repository_path: &Path,
    baseline_oid: &str,
    session_id: &super::RefComponent,
    turn_id: &super::RefComponent,
    limits: &PromotionLimits,
) -> Result<ValidatedCandidate, PromotionError> {
    let candidate_ref = format!(
        "refs/lattice/candidates/{}/{}",
        session_id.as_str(),
        turn_id.as_str()
    );

    let candidate_oid =
        resolve_candidate_ref(repository_path, &candidate_ref)?.ok_or_else(|| {
            PromotionError::GitCommand {
                context: format!("resolve {candidate_ref}"),
                stderr: "candidate ref not found".into(),
            }
        })?;

    // Verify object type.
    run_git(
        repository_path,
        ["cat-file", "-t", &candidate_oid],
        "cat-file pinned candidate",
    )?;

    // fsck
    run_git(
        repository_path,
        ["fsck", "--strict", &candidate_oid],
        "fsck pinned candidate",
    )
    .map_err(|_| PromotionError::ConnectivityFailure)?;

    // Ancestry check.
    if !git_is_ancestor(repository_path, baseline_oid, &candidate_oid)? {
        return Err(PromotionError::NotDescendant {
            baseline: baseline_oid.to_owned(),
            candidate: candidate_oid.clone(),
        });
    }

    // Object enumeration and limit checks.
    let object_stats = enumerate_objects(repository_path, baseline_oid, &candidate_oid, limits)?;

    // Hazard scan.
    scan_commit_hazards(
        repository_path,
        baseline_oid,
        &candidate_oid,
        limits,
        &object_stats,
    )?;

    Ok(ValidatedCandidate {
        baseline_oid: baseline_oid.to_owned(),
        candidate_oid: candidate_oid.clone(),
        artifact_len: 0, // Pinned candidates have no bundle artifact.
        review_facts: vec![],
        verification_argv: vec![],
        verification_profile_hash: String::new(),
        verification_expected_exit_code: 0,
        verification_exit_code: None,
    })
}

/// Rebuild a [`ValidatedCandidate`] from a pinned candidate ref with fresh
/// verification evidence. Performs all ancestry/object/hazard/limit
/// checks without requiring a bundle.
pub fn prepare_pinned(
    repository_path: &Path,
    baseline_oid: &str,
    session_id: &super::RefComponent,
    turn_id: &super::RefComponent,
    verification: &VerificationResult,
    limits: &PromotionLimits,
) -> Result<ValidatedCandidate, PromotionError> {
    let candidate_ref = format!(
        "refs/lattice/candidates/{}/{}",
        session_id.as_str(),
        turn_id.as_str()
    );

    let candidate_oid =
        resolve_candidate_ref(repository_path, &candidate_ref)?.ok_or_else(|| {
            PromotionError::GitCommand {
                context: format!("resolve {candidate_ref}"),
                stderr: "candidate ref not found".into(),
            }
        })?;

    // Evidence binding check.
    if verification.candidate_oid != candidate_oid {
        return Err(PromotionError::VerificationMismatch {
            expected: candidate_oid.clone(),
            actual: verification.candidate_oid.clone(),
        });
    }
    if !verification.passed {
        return Err(PromotionError::VerificationMismatch {
            expected: candidate_oid.clone(),
            actual: verification.candidate_oid.clone(),
        });
    }
    if verification.exit_code != Some(verification.expected_exit_code) {
        return Err(PromotionError::InvalidEvidence {
            reason: format!(
                "passed=true requires exit_code=Some({}), got {:?}",
                verification.expected_exit_code, verification.exit_code,
            ),
        });
    }

    // fsck
    run_git(
        repository_path,
        ["fsck", "--strict", &candidate_oid],
        "fsck pinned candidate",
    )
    .map_err(|_| PromotionError::ConnectivityFailure)?;

    // Ancestry
    if !git_is_ancestor(repository_path, baseline_oid, &candidate_oid)? {
        return Err(PromotionError::NotDescendant {
            baseline: baseline_oid.to_owned(),
            candidate: candidate_oid.clone(),
        });
    }

    // Object enumeration and limits
    let object_stats = enumerate_objects(repository_path, baseline_oid, &candidate_oid, limits)?;

    // Hazard scan
    scan_commit_hazards(
        repository_path,
        baseline_oid,
        &candidate_oid,
        limits,
        &object_stats,
    )?;

    Ok(ValidatedCandidate {
        baseline_oid: baseline_oid.to_owned(),
        candidate_oid: candidate_oid.clone(),
        artifact_len: 0,
        review_facts: vec![],
        verification_argv: verification.argv.clone(),
        verification_profile_hash: verification.profile_hash.clone(),
        verification_expected_exit_code: verification.expected_exit_code,
        verification_exit_code: verification.exit_code,
    })
}

fn resolve_candidate_ref(
    repository_path: &Path,
    refname: &str,
) -> Result<Option<String>, PromotionError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args(["rev-parse", "--verify", refname])
        .output()
        .map_err(PromotionError::Io)?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        ))
    } else {
        Ok(None)
    }
}

fn find_baseline_oid(
    repository_path: &Path,
    candidate_oid: &str,
) -> Result<String, PromotionError> {
    // Find the first parent of the candidate commit.
    let output = Command::new("git")
        .arg("-C")
        .arg(repository_path)
        .args(["rev-parse", &format!("{candidate_oid}^")])
        .output()
        .map_err(PromotionError::Io)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(PromotionError::GitCommand {
            context: "find baseline (first parent)".into(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}
