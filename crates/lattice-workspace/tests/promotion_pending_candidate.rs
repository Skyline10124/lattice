// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Pin/load/idempotency/reachability/retry tests for
//! pending-promotion candidate durability.

use std::path::{Path, PathBuf};
use std::process::Command;

use git2::{IndexAddOption, Repository, Signature};
use lattice_workspace::promotion::{
    PromotionError, PromotionLimits, RefComponent, VerificationResult, load_pinned_candidate,
    pin_candidate, prepare_pinned,
};

fn sig() -> Signature<'static> {
    Signature::now("promo-test", "pt@localhost").expect("sig")
}

fn init_repo(dir: &Path) -> Repository {
    Repository::init(dir).expect("git init")
}

fn commit_all(repo: &Repository, message: &str, parents: &[&git2::Commit]) -> git2::Oid {
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), IndexAddOption::DEFAULT, None)
        .expect("add_all");
    index.write().expect("index write");
    let tree_id = index.write_tree().expect("write_tree");
    let tree = repo.find_tree(tree_id).expect("tree");
    repo.commit(Some("HEAD"), &sig(), &sig(), message, &tree, parents)
        .expect("commit")
}

struct RepoFixture {
    _dir: tempfile::TempDir,
    repo_path: PathBuf,
    baseline: String,
    candidate: String,
}

impl RepoFixture {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = init_repo(dir.path());
        std::fs::write(dir.path().join("root.txt"), b"root\n").expect("write");
        let root = commit_all(&repo, "root", &[]);
        std::fs::write(dir.path().join("child.txt"), b"child\n").expect("write");
        let baseline = commit_all(
            &repo,
            "baseline",
            &[&repo.find_commit(root).expect("parent")],
        );
        std::fs::write(dir.path().join("candidate.txt"), b"candidate\n").expect("write");
        let candidate = commit_all(
            &repo,
            "candidate",
            &[&repo.find_commit(baseline).expect("parent")],
        );
        // Set up branch ref
        Command::new("git")
            .args(["update-ref", "refs/heads/main", &baseline.to_string()])
            .current_dir(dir.path())
            .status()
            .expect("update-ref main");
        Command::new("git")
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .current_dir(dir.path())
            .status()
            .expect("symbolic-ref main");
        Self {
            repo_path: dir.path().to_path_buf(),
            baseline: baseline.to_string(),
            candidate: candidate.to_string(),
            _dir: dir,
        }
    }

    fn session_id() -> RefComponent {
        RefComponent::parse("session_id", "sess1").expect("session id")
    }

    fn turn_id() -> RefComponent {
        RefComponent::parse("turn_id", "turn1").expect("turn id")
    }
}

fn candidate_ref_name(session_id: &RefComponent, turn_id: &RefComponent) -> String {
    format!(
        "refs/lattice/candidates/{}/{}",
        session_id.as_str(),
        turn_id.as_str()
    )
}

fn resolve_ref(repo_path: &Path, refname: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "--verify", refname])
        .output()
        .expect("rev-parse");
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        None
    }
}

#[test]
fn pin_creates_candidate_ref_with_exact_oid() {
    let fx = RepoFixture::new();
    let pinned = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("pin");
    assert_eq!(pinned.candidate_oid, fx.candidate);
    assert_eq!(pinned.baseline_oid, fx.baseline);

    let ref_oid = resolve_ref(
        &fx.repo_path,
        &candidate_ref_name(&RepoFixture::session_id(), &RepoFixture::turn_id()),
    );
    assert_eq!(ref_oid.as_deref(), Some(fx.candidate.as_str()));
}

#[test]
fn pin_is_idempotent_for_same_oid() {
    let fx = RepoFixture::new();
    let _first = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("first pin");
    let second = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("idempotent pin");
    assert_eq!(second.candidate_oid, fx.candidate);
}

#[test]
fn pin_rejects_different_oid() {
    let fx = RepoFixture::new();
    let _first = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("first pin");

    let different_oid = fx.baseline.clone();
    let err = pin_candidate(
        &fx.repo_path,
        &different_oid,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect_err("substitution must be rejected");
    assert!(
        matches!(err, PromotionError::CandidateSubstitution { .. }),
        "unexpected error: {err:?}"
    );

    // Verify ref was NOT moved
    let ref_oid = resolve_ref(
        &fx.repo_path,
        &candidate_ref_name(&RepoFixture::session_id(), &RepoFixture::turn_id()),
    );
    assert_eq!(ref_oid.as_deref(), Some(fx.candidate.as_str()));
}

#[test]
fn load_pinned_candidate_validates_and_returns() {
    let fx = RepoFixture::new();
    let _pinned = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("pin");

    let loaded = load_pinned_candidate(
        &fx.repo_path,
        &fx.baseline,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
        &PromotionLimits::default(),
    )
    .expect("load pinned");
    assert_eq!(loaded.candidate_oid, fx.candidate);
    assert_eq!(loaded.baseline_oid, fx.baseline);
}

#[test]
fn load_pinned_rejects_missing_ref() {
    let fx = RepoFixture::new();
    let err = load_pinned_candidate(
        &fx.repo_path,
        &fx.baseline,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
        &PromotionLimits::default(),
    )
    .expect_err("missing ref must fail");
    assert!(
        err.to_string().contains("candidate ref")
            || err.to_string().contains("resolve")
            || err.to_string().contains("missing"),
        "unexpected error: {err}"
    );
}

#[test]
fn candidate_remains_resolvable_after_pin() {
    let fx = RepoFixture::new();
    let _pinned = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("pin");

    // Verify the candidate OID is still a valid commit object
    let output = Command::new("git")
        .arg("-C")
        .arg(&fx.repo_path)
        .args(["cat-file", "-t", &fx.candidate])
        .output()
        .expect("cat-file");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "commit");
}

#[test]
fn prepare_pinned_rebuilds_validated_candidate() {
    let fx = RepoFixture::new();
    let _pinned = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("pin");

    let evidence = VerificationResult::new(
        fx.candidate.clone(),
        vec!["test-cmd".into()],
        "abc123".into(),
        0,
        Some(0),
        true,
        "ok".into(),
    )
    .expect("evidence");

    let validated = prepare_pinned(
        &fx.repo_path,
        &fx.baseline,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
        &evidence,
        &PromotionLimits::default(),
    )
    .expect("prepare_pinned");
    assert_eq!(validated.candidate_oid, fx.candidate);
    assert_eq!(validated.baseline_oid, fx.baseline);
}

#[test]
fn prepare_pinned_rejects_verification_mismatch() {
    let fx = RepoFixture::new();
    let _pinned = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("pin");

    let bad_evidence = VerificationResult::new(
        "0".repeat(40),
        vec!["test-cmd".into()],
        "abc123".into(),
        0,
        Some(0),
        true,
        "stale".into(),
    )
    .expect("evidence");

    let err = prepare_pinned(
        &fx.repo_path,
        &fx.baseline,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
        &bad_evidence,
        &PromotionLimits::default(),
    )
    .expect_err("mismatch must reject");
    assert!(
        matches!(err, PromotionError::VerificationMismatch { .. }),
        "unexpected error: {err:?}"
    );
}

#[test]
fn prepare_pinned_runs_hazard_and_limit_checks() {
    let fx = RepoFixture::new();
    let _pinned = pin_candidate(
        &fx.repo_path,
        &fx.candidate,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
    )
    .expect("pin");

    let evidence = VerificationResult::new(
        fx.candidate.clone(),
        vec!["test-cmd".into()],
        "abc123".into(),
        0,
        Some(0),
        true,
        "ok".into(),
    )
    .expect("evidence");

    // Use extremely low object count to trigger limit
    let limits = PromotionLimits {
        max_object_count: 1,
        ..PromotionLimits::default()
    };

    let err = prepare_pinned(
        &fx.repo_path,
        &fx.baseline,
        &RepoFixture::session_id(),
        &RepoFixture::turn_id(),
        &evidence,
        &limits,
    )
    .expect_err("limit must trigger");
    assert!(
        matches!(err, PromotionError::ObjectCountExceeded { .. }),
        "unexpected error: {err:?}"
    );
}
