// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! V-01 through V-08 integrated promotion orchestration coverage.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use git2::Repository;
use lattice_workspace::git::export_candidate_from_repo;
use lattice_workspace::promotion::{
    FaultPoint, ManagedLower, PromotionError, PromotionJournal, PromotionLimits, PromotionRefs,
    RecoveryOutcome, RefComponent, TestIdentityReader, VerificationResult, commit, prepare,
    recover,
};

fn init_repo(dir: &Path, branch: &str) -> String {
    Command::new("git")
        .args(["init", "-b", branch])
        .current_dir(dir)
        .output()
        .expect("git init");
    git_config(dir);
    std::fs::write(dir.join("README.md"), b"init\n").expect("write");
    git_commit_all(dir, "init");
    git_rev_parse(dir, "HEAD")
}

fn git_config(dir: &Path) {
    for args in [
        ["config", "user.email", "test@localhost"],
        ["config", "user.name", "test"],
    ] {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git config");
    }
}

fn git_commit_all(dir: &Path, message: &str) {
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("add");
    Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(dir)
        .output()
        .expect("commit");
}

fn git_rev_parse(dir: &Path, spec: &str) -> String {
    String::from_utf8(
        Command::new("git")
            .args(["rev-parse", spec])
            .current_dir(dir)
            .output()
            .expect("rev-parse")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_owned()
}

fn detach_primary(repo: &Path) {
    Command::new("git")
        .args(["checkout", "--detach", "HEAD"])
        .current_dir(repo)
        .output()
        .expect("detach");
}

fn pin_main_to_baseline(repo: &Path, baseline: &str) {
    Command::new("git")
        .args(["update-ref", "refs/heads/main", baseline])
        .current_dir(repo)
        .output()
        .expect("pin main");
}

fn fixture_refs() -> (RefComponent, RefComponent, PromotionRefs) {
    let session = RefComponent::parse("session_id", "sess1").expect("session");
    let turn = RefComponent::parse("turn_id", "turn1").expect("turn");
    let refs = PromotionRefs::new("main", &session, &turn).expect("refs");
    (session, turn, refs)
}

fn promotion_state_dir(repo: &Path, branch: &str) -> PathBuf {
    let hex = branch
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let common = git_rev_parse(repo, "--git-common-dir");
    let common_path = PathBuf::from(&common);
    let common_path = if common_path.is_absolute() {
        common_path
    } else {
        repo.join(common_path)
    };
    common_path.join("lattice/promotion").join(hex)
}

fn snapshot_refs(repo: &Path, refs: &PromotionRefs) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for name in [
        refs.project_ref.as_str(),
        refs.session_ref.as_str(),
        refs.candidate_ref.as_str(),
    ] {
        let output = Command::new("git")
            .args(["rev-parse", name])
            .current_dir(repo)
            .output()
            .expect("rev-parse");
        if output.status.success() {
            map.insert(
                name.to_owned(),
                String::from_utf8_lossy(&output.stdout).trim().to_owned(),
            );
        }
    }
    map
}

fn make_request(
    repo: &Path,
    baseline: &str,
    candidate: &str,
    session: &RefComponent,
    turn: &RefComponent,
) -> lattice_workspace::promotion::PromotionRequest {
    let export = export_candidate_from_repo(
        repo,
        baseline,
        PromotionLimits::default().max_artifact_bytes,
    )
    .expect("export");
    assert_eq!(export.candidate_oid(), candidate);
    lattice_workspace::promotion::PromotionRequest {
        repository_path: repo.to_path_buf(),
        branch: "main".into(),
        export,
        verification: VerificationResult::new(
            candidate.to_owned(),
            vec!["test-cmd".into()],
            "test-profile-hash".into(),
            0,
            Some(0),
            true,
            "test".into(),
        )
        .expect("test evidence"),
        session_id: session.clone(),
        turn_id: turn.clone(),
        limits: PromotionLimits::default(),
    }
}

fn simple_session_commit(
    repo: &Repository,
    parent: Option<git2::Oid>,
) -> Result<git2::Oid, PromotionError> {
    let sig = git2::Signature::now("test", "test@localhost")
        .map_err(|e| PromotionError::Git(e.to_string()))?;
    let tree = repo
        .head()
        .map_err(|e| PromotionError::Git(e.to_string()))?
        .peel_to_commit()
        .map_err(|e| PromotionError::Git(e.to_string()))?
        .tree_id();
    let tree = repo
        .find_tree(tree)
        .map_err(|e| PromotionError::Git(e.to_string()))?;
    let parents: Vec<git2::Commit> = match parent {
        Some(oid) => vec![
            repo.find_commit(oid)
                .map_err(|e| PromotionError::Git(e.to_string()))?,
        ],
        None => Vec::new(),
    };
    repo.commit(
        None,
        &sig,
        &sig,
        "session commit",
        &tree,
        &parents.iter().collect::<Vec<_>>(),
    )
    .map_err(|e| PromotionError::Git(e.to_string()))
}

struct CandidateFixture {
    dir: tempfile::TempDir,
    baseline: String,
    candidate: String,
    session: RefComponent,
    turn: RefComponent,
    refs: PromotionRefs,
}

impl CandidateFixture {
    fn with_change(content: &[u8]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        std::fs::write(dir.path().join("tracked.txt"), b"base\n").expect("write");
        git_commit_all(dir.path(), "baseline");
        let baseline = git_rev_parse(dir.path(), "HEAD");
        std::fs::write(dir.path().join("tracked.txt"), content).expect("write");
        git_commit_all(dir.path(), "candidate");
        let candidate = git_rev_parse(dir.path(), "HEAD");
        let (session, turn, refs) = fixture_refs();
        Self {
            dir,
            baseline,
            candidate,
            session,
            turn,
            refs,
        }
    }

    fn repo(&self) -> &Path {
        self.dir.path()
    }

    fn ready(&self) {
        detach_primary(self.repo());
        pin_main_to_baseline(self.repo(), &self.baseline);
    }

    fn request(&self) -> lattice_workspace::promotion::PromotionRequest {
        make_request(
            self.repo(),
            &self.baseline,
            &self.candidate,
            &self.session,
            &self.turn,
        )
    }
}

mod promotion_commit_failures {
    use super::*;

    #[test]
    fn prepare_rejection_creates_no_lock_or_journal() {
        let fx = CandidateFixture::with_change(b"cand\n");
        let mut req = fx.request();
        req.verification.candidate_oid = "0".repeat(40);
        let state_dir = promotion_state_dir(fx.repo(), "main");
        assert!(prepare(req).is_err());
        assert!(!state_dir.join("lower.lock").exists());
        assert!(!PromotionJournal::exists(&state_dir));
    }

    #[test]
    fn stale_project_ref_blocks_before_callback() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prepared = prepare(fx.request()).expect("prepare");
        let other = git_rev_parse(fx.repo(), "HEAD");
        Command::new("git")
            .args(["update-ref", "refs/heads/main", &other])
            .current_dir(fx.repo())
            .output()
            .expect("move ref");
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_cb = Arc::clone(&calls);
        let err = commit(prepared, move |repo, parent| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
            simple_session_commit(repo, parent)
        })
        .unwrap_err();
        assert!(matches!(err, PromotionError::BaselineConflict { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn dirty_lower_blocks_before_callback() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prepared = prepare(fx.request()).expect("prepare");
        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        std::fs::write(lower.worktree_path().join("dirty.txt"), b"x\n").expect("dirty");
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_cb = Arc::clone(&calls);
        let err = commit(prepared, move |repo, parent| {
            calls_cb.fetch_add(1, Ordering::SeqCst);
            simple_session_commit(repo, parent)
        })
        .unwrap_err();
        assert!(matches!(err, PromotionError::Dirty(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn callback_error_leaves_refs_and_lower_unchanged() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let before_refs = snapshot_refs(fx.repo(), &fx.refs);
        let prepared = prepare(fx.request()).expect("prepare");
        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        let lower_before = git_rev_parse(lower.worktree_path(), "HEAD");
        let err = commit(prepared, |_repo, _parent| {
            Err(PromotionError::CallbackFailed("boom".into()))
        })
        .unwrap_err();
        assert!(matches!(err, PromotionError::CallbackFailed(_)));
        assert_eq!(snapshot_refs(fx.repo(), &fx.refs), before_refs);
        assert_eq!(git_rev_parse(lower.worktree_path(), "HEAD"), lower_before);
    }

    #[test]
    fn existing_candidate_ref_with_same_oid_succeeds_via_verify() {
        // D-14: already-pinned candidate participates as verify in transaction.
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        Command::new("git")
            .args(["update-ref", fx.refs.candidate_ref.as_str(), &fx.candidate])
            .current_dir(fx.repo())
            .output()
            .expect("seed candidate ref");
        let prepared = prepare(fx.request()).expect("prepare");
        let outcome = commit(prepared, simple_session_commit).expect("pinned retry must succeed");
        assert_eq!(outcome.candidate_oid, fx.candidate);
    }

    #[test]
    fn callback_uses_observed_session_parent_and_cas() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prior_session = git_rev_parse(fx.repo(), "HEAD");
        Command::new("git")
            .args(["update-ref", fx.refs.session_ref.as_str(), &prior_session])
            .current_dir(fx.repo())
            .output()
            .expect("seed session ref");
        let prepared = prepare(fx.request()).expect("prepare");
        let outcome = commit(prepared, |repo, parent| {
            assert_eq!(parent.map(|p| p.to_string()), Some(prior_session.clone()));
            simple_session_commit(repo, parent)
        })
        .expect("commit");
        assert_eq!(
            snapshot_refs(fx.repo(), &fx.refs)
                .get(fx.refs.session_ref.as_str())
                .map(String::as_str),
            Some(outcome.session_oid.as_str())
        );
    }

    #[test]
    fn absent_session_ref_uses_create_without_parent() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prepared = prepare(fx.request()).expect("prepare");
        commit(prepared, |repo, parent| {
            assert!(parent.is_none());
            simple_session_commit(repo, parent)
        })
        .expect("commit");
        assert!(snapshot_refs(fx.repo(), &fx.refs).contains_key(fx.refs.session_ref.as_str()));
    }

    #[test]
    fn lock_precedes_journal_and_lower_writes() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prepared = prepare(fx.request()).expect("prepare");
        let state_dir = promotion_state_dir(fx.repo(), "main");
        let lock_path = state_dir.join("lower.lock");
        commit(prepared, |repo, parent| {
            assert!(lock_path.exists(), "lock must exist before callback");
            assert!(
                PromotionJournal::exists(&state_dir),
                "journal must exist before callback"
            );
            simple_session_commit(repo, parent)
        })
        .expect("commit");
    }
}

mod promotion_success {
    use super::*;

    #[test]
    fn clean_candidate_advances_refs_and_materializes_lower() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prepared = prepare(fx.request()).expect("prepare");
        let outcome = commit(prepared, simple_session_commit).expect("commit");
        assert_eq!(outcome.project_oid, fx.candidate);
        assert_eq!(outcome.candidate_oid, fx.candidate);
        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        assert_eq!(
            std::fs::read(lower.worktree_path().join("tracked.txt")).unwrap(),
            b"cand\n"
        );
        let refs = snapshot_refs(fx.repo(), &fx.refs);
        assert_eq!(refs.get(fx.refs.project_ref.as_str()), Some(&fx.candidate));
        assert_eq!(
            refs.get(fx.refs.candidate_ref.as_str()),
            Some(&fx.candidate)
        );
    }

    #[test]
    fn noop_candidate_keeps_project_oid_while_session_and_candidate_refs_advance() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        std::fs::write(dir.path().join("same.txt"), b"same\n").expect("write");
        git_commit_all(dir.path(), "baseline");
        let baseline = git_rev_parse(dir.path(), "HEAD");
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "noop"])
            .current_dir(dir.path())
            .output()
            .expect("empty commit");
        let candidate = git_rev_parse(dir.path(), "HEAD");
        assert_ne!(baseline, candidate);
        let (session, turn, refs) = fixture_refs();
        detach_primary(dir.path());
        pin_main_to_baseline(dir.path(), &baseline);
        let prepared = prepare(make_request(
            dir.path(),
            &baseline,
            &candidate,
            &session,
            &turn,
        ))
        .expect("prepare");
        let outcome = commit(prepared, simple_session_commit).expect("commit");
        assert_eq!(outcome.project_oid, candidate);
        assert_eq!(outcome.candidate_oid, candidate);
        let snap = snapshot_refs(dir.path(), &refs);
        assert_eq!(snap.get(refs.project_ref.as_str()), Some(&candidate));
        assert_ne!(snap.get(refs.session_ref.as_str()), Some(&baseline));
        assert_eq!(snap.get(refs.candidate_ref.as_str()), Some(&candidate));
        assert_eq!(
            std::fs::read(dir.path().join("same.txt")).unwrap(),
            b"same\n"
        );
    }
}

mod promotion_recovery_gate {
    use super::*;

    #[test]
    fn post_ref_materialization_fault_requires_recovery_before_retry() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prepared = prepare(fx.request())
            .expect("prepare")
            .with_fault(FaultPoint::AfterRefsCommitted);
        let err = commit(prepared.clone(), simple_session_commit).unwrap_err();
        assert!(matches!(err, PromotionError::RecoveryRequired));
        let state_dir = promotion_state_dir(fx.repo(), "main");
        assert!(PromotionJournal::exists(&state_dir));

        let retry_err = commit(prepared, simple_session_commit).unwrap_err();
        assert!(
            matches!(
                retry_err,
                PromotionError::RecoveryRequired
                    | PromotionError::BaselineConflict { .. }
                    | PromotionError::CandidateRefExists
            ),
            "unexpected retry error: {retry_err:?}"
        );

        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        let outcome = recover(&lower).expect("recover");
        assert_eq!(outcome, RecoveryOutcome::Complete);
        assert!(!PromotionJournal::exists(&state_dir));

        // First promotion completed after recovery; create a follow-on candidate.
        std::fs::write(fx.repo().join("follow.txt"), b"follow\n").expect("write");
        git_commit_all(fx.repo(), "follow-on");
        let new_candidate = git_rev_parse(fx.repo(), "HEAD");
        pin_main_to_baseline(fx.repo(), &fx.candidate);
        let turn2 = RefComponent::parse("turn_id", "turn2").expect("turn2");
        let req2 = make_request(
            fx.repo(),
            &fx.candidate,
            &new_candidate,
            &fx.session,
            &turn2,
        );
        let prepared_final = prepare(req2).expect("prepare final");
        commit(prepared_final, simple_session_commit).expect("final commit");
    }

    #[test]
    fn third_state_conflict_preserves_journal() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prepared = prepare(fx.request()).expect("prepare");
        let err = commit(
            prepared.with_fault(FaultPoint::AfterRefsCommitted),
            simple_session_commit,
        )
        .unwrap_err();
        assert!(matches!(err, PromotionError::RecoveryRequired));
        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        std::fs::write(lower.worktree_path().join("tracked.txt"), b"third\n").expect("third");
        let outcome = recover(&lower).expect("recover");
        assert!(matches!(outcome, RecoveryOutcome::RecoveryConflict { .. }));
        assert!(PromotionJournal::exists(&promotion_state_dir(
            fx.repo(),
            "main"
        )));
    }
}

mod promotion_only_lower_writer {
    use super::*;
    use lattice_workspace::promotion::LowerLockGuard;

    #[test]
    fn only_commit_and_recover_mutate_managed_lower() {
        let fx = CandidateFixture::with_change(b"sentinel\n");
        fx.ready();
        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        let tracked = lower.worktree_path().join("tracked.txt");
        let before = std::fs::read(&tracked).expect("read tracked");

        let prepared = prepare(fx.request()).expect("prepare");
        assert_eq!(std::fs::read(&tracked).unwrap(), before);

        let reader = TestIdentityReader::new("boot-test");
        reader.register_process(std::process::id(), 1);
        {
            let _guard = LowerLockGuard::acquire(&lower.promotion_paths().lock_path, &reader, None)
                .expect("lock");
        }
        assert_eq!(std::fs::read(&tracked).unwrap(), before);

        commit(prepared, simple_session_commit).expect("commit");
        assert_eq!(std::fs::read(&tracked).unwrap(), b"sentinel\n");

        let after_commit = std::fs::read(&tracked).unwrap();
        let outcome = recover(&lower).expect("recover");
        assert_eq!(outcome, RecoveryOutcome::Complete);
        assert_eq!(std::fs::read(&tracked).unwrap(), after_commit);
    }
}
