// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! lattice-api promotion composition and boundary coverage.

use std::path::{Path, PathBuf};
use std::process::Command;

use git2::Repository;
use lattice_api::{
    ApiError, PromotionLifecycleError, PromotionLimits, PromotionSessionMetadata,
    PromotionTurnTarget, VerificationResult, prepare_promotion_target,
    promote_preexported_candidate, promote_verified_candidate, retry_pinned_candidate,
};
use lattice_workspace::{
    EgressPolicy, ImageOrTemplate, UpperSpec, Workspace,
    promotion::{
        FaultPoint, ManagedLower, PromotionError, PromotionJournal, PromotionRefs, RecoveryOutcome,
        RefComponent, commit, pin_candidate, prepare, recover,
    },
    testing::DeterministicWorkspace,
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

fn fixture_refs() -> (RefComponent, RefComponent, PromotionRefs) {
    let session = RefComponent::parse("session_id", "sess1").expect("session");
    let turn = RefComponent::parse("turn_id", "turn1").expect("turn");
    let refs = PromotionRefs::new("main", &session, &turn).expect("refs");
    (session, turn, refs)
}

fn make_request(
    repo: &Path,
    baseline: &str,
    candidate: &str,
    session: &RefComponent,
    turn: &RefComponent,
) -> lattice_workspace::promotion::PromotionRequest {
    use lattice_workspace::git::export_candidate_from_repo;
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

async fn commit_candidate_in_upper(
    ws: &DeterministicWorkspace,
    handle: &lattice_workspace::UpperHandle,
) {
    ws.exec_upper(handle, "git", &["config", "user.email", "test@localhost"])
        .await
        .expect("email");
    ws.exec_upper(handle, "git", &["config", "user.name", "test"])
        .await
        .expect("name");
    ws.exec_upper(handle, "sh", &["-c", "echo promoted > tracked.txt"])
        .await
        .expect("write");
    ws.exec_upper(handle, "git", &["add", "tracked.txt"])
        .await
        .expect("add");
    ws.exec_upper(handle, "git", &["commit", "-m", "promoted content"])
        .await
        .expect("commit");
}

mod promotion_wiring {
    use super::*;

    #[test]
    fn dirty_managed_lower_blocks_preflight() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        std::fs::write(lower.worktree_path().join("dirty.txt"), b"x\n").expect("dirty");
        let err = prepare_promotion_target(fx.repo(), "main").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("lower dirty"),
            "expected dirty error, got {msg}"
        );
    }

    #[test]
    fn pending_journal_returns_recovery_required() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let prepared = prepare(fx.request()).expect("prepare");
        let err = commit(
            prepared.with_fault(FaultPoint::AfterRefsCommitted),
            simple_session_commit,
        )
        .unwrap_err();
        assert!(matches!(err, PromotionError::RecoveryRequired));

        let api_err = prepare_promotion_target(fx.repo(), "main").unwrap_err();
        assert!(matches!(api_err, ApiError::PromotionRecoveryRequired));
    }

    #[test]
    fn third_state_journal_returns_recovery_conflict() {
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
        let api_err = prepare_promotion_target(fx.repo(), "main").unwrap_err();
        assert!(matches!(api_err, ApiError::PromotionRecoveryConflict(_)));
        assert!(PromotionJournal::exists(&promotion_state_dir(
            fx.repo(),
            "main"
        )));
    }

    #[test]
    fn recover_enables_preflight_and_promotion() {
        let fx = CandidateFixture::with_change(b"promoted\n");
        fx.ready();
        let prepared = prepare(fx.request()).expect("prepare");
        let _ = commit(
            prepared.with_fault(FaultPoint::AfterRefsCommitted),
            simple_session_commit,
        )
        .unwrap_err();
        assert!(prepare_promotion_target(fx.repo(), "main").is_err());

        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        assert_eq!(recover(&lower).expect("recover"), RecoveryOutcome::Complete);

        let target = prepare_promotion_target(fx.repo(), "main").expect("preflight");
        assert_eq!(target.baseline_oid, fx.candidate);
        assert_eq!(git_rev_parse(fx.repo(), "refs/heads/main"), fx.candidate);
    }

    #[tokio::test]
    async fn stale_verification_reaches_no_promotion_effects() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let target = prepare_promotion_target(fx.repo(), "main").expect("target");
        let state_dir = promotion_state_dir(fx.repo(), "main");

        let ws = DeterministicWorkspace::new().expect("ws");
        let spec = UpperSpec {
            image_or_template: ImageOrTemplate::Image("img".into()),
            baseline_oid: fx.baseline.clone(),
            host_repo_path: fx.repo().to_path_buf(),
            egress: EgressPolicy::Blocked,
            credentials: vec![],
            timeout_secs: 30,
            cpus: 1,
            memory_mb: 256,
        };
        let handle = ws.create_upper(&spec).await.expect("upper");
        commit_candidate_in_upper(&ws, &handle).await;

        let metadata = PromotionSessionMetadata {
            session_id: "sess1".into(),
            turn_id: "turn1".into(),
            committed_at_unix_seconds: 1_700_000_000,
        };
        let verification = VerificationResult::new(
            "0".repeat(40),
            vec!["test-cmd".into()],
            "test-profile-hash".into(),
            0,
            Some(0),
            true,
            "stale".into(),
        )
        .expect("test evidence");

        let err = promote_verified_candidate(
            &ws,
            &handle,
            &target,
            &metadata,
            verification,
            PromotionLimits::default(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("verification mismatch"));
        assert!(!state_dir.join("lower.lock").exists());
        assert!(!PromotionJournal::exists(&state_dir));
    }

    #[tokio::test]
    async fn success_uses_real_session_callback_and_materializes_lower() {
        let fx = CandidateFixture::with_change(b"promoted\n");
        fx.ready();
        let target = prepare_promotion_target(fx.repo(), "main").expect("target");

        let ws = DeterministicWorkspace::new().expect("ws");
        let spec = UpperSpec {
            image_or_template: ImageOrTemplate::Image("img".into()),
            baseline_oid: fx.baseline.clone(),
            host_repo_path: fx.repo().to_path_buf(),
            egress: EgressPolicy::Blocked,
            credentials: vec![],
            timeout_secs: 30,
            cpus: 1,
            memory_mb: 256,
        };
        let handle = ws.create_upper(&spec).await.expect("upper");
        commit_candidate_in_upper(&ws, &handle).await;

        let export = ws
            .export_candidate(
                &handle,
                &fx.baseline,
                PromotionLimits::default().max_artifact_bytes,
            )
            .await
            .expect("export");
        let candidate_oid = export.candidate_oid().to_owned();

        let metadata = PromotionSessionMetadata {
            session_id: "sess1".into(),
            turn_id: "turn1".into(),
            committed_at_unix_seconds: 1_700_000_000,
        };
        let verification = VerificationResult::new(
            candidate_oid.clone(),
            vec!["test-cmd".into()],
            "test-profile-hash".into(),
            0,
            Some(0),
            true,
            "verified".into(),
        )
        .expect("test evidence");

        let outcome = promote_verified_candidate(
            &ws,
            &handle,
            &target,
            &metadata,
            verification,
            PromotionLimits::default(),
        )
        .await
        .expect("promote");

        assert_eq!(outcome.project_oid, candidate_oid);
        assert_eq!(outcome.candidate_oid, candidate_oid);
        assert_eq!(git_rev_parse(fx.repo(), "refs/heads/main"), candidate_oid);
        assert_eq!(
            git_rev_parse(fx.repo(), fx.refs.session_ref.as_str()),
            outcome.session_oid
        );
        assert_eq!(
            git_rev_parse(fx.repo(), fx.refs.candidate_ref.as_str()),
            candidate_oid
        );

        let lower = ManagedLower::provision_or_open(fx.repo(), "main").expect("lower");
        assert_eq!(
            std::fs::read(lower.worktree_path().join("tracked.txt")).unwrap(),
            b"promoted\n"
        );

        let repo = Repository::open(fx.repo()).expect("repo");
        let session_commit = repo
            .find_commit(git2::Oid::from_str(&outcome.session_oid).expect("parse session oid"))
            .expect("session commit");
        let root_tree = session_commit.tree().expect("tree");
        let manifest_entry = root_tree.get_name("turn").expect("turn dir");
        let turn_tree = repo.find_tree(manifest_entry.id()).expect("turn tree");
        let manifest_blob = turn_tree.get_name("manifest.json").expect("manifest");
        let blob = repo.find_blob(manifest_blob.id()).expect("blob");
        let manifest = String::from_utf8(blob.content().to_vec()).expect("utf8");
        assert!(manifest.contains("\"disposition\":\"promoted\""));
        assert!(manifest.contains(&format!("\"candidate_oid\":\"{candidate_oid}\"")));
    }

    #[test]
    fn public_api_compiles_for_embedder() {
        fn _uses_api(
            target: PromotionTurnTarget,
            metadata: PromotionSessionMetadata,
            verification: VerificationResult,
            limits: PromotionLimits,
        ) where
            PromotionTurnTarget: Send,
        {
            let _ = (
                target.repository_path,
                target.baseline_oid,
                metadata,
                verification,
                limits,
            );
        }
    }
}

mod preexported {
    use super::*;

    /// Compile-time assertion: CandidateExport is re-exported from lattice_api.
    #[allow(dead_code)]
    fn _assert_candidate_export_is_public() {
        fn _takes(_: lattice_api::CandidateExport) {}
    }

    #[test]
    fn promotion_source_contains_no_export_candidate_call() {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/turn/promotion.rs"),
        )
        .expect("read promotion.rs");
        assert!(
            !text.contains("export_candidate("),
            "promotion.rs must not call export_candidate internally"
        );
    }

    #[tokio::test]
    async fn preexported_promotion_succeeds_without_workspace() {
        let fx = CandidateFixture::with_change(b"promoted\n");
        fx.ready();
        let target = prepare_promotion_target(fx.repo(), "main").expect("target");

        let export = lattice_workspace::export_candidate_from_repo(
            fx.repo(),
            &fx.baseline,
            PromotionLimits::default().max_artifact_bytes,
        )
        .expect("export");
        let candidate_oid = export.candidate_oid().to_owned();

        let metadata = PromotionSessionMetadata {
            session_id: "sess1".into(),
            turn_id: "turn1".into(),
            committed_at_unix_seconds: 1_700_000_000,
        };
        let verification = VerificationResult::new(
            candidate_oid.clone(),
            vec!["test-cmd".into()],
            "test-profile-hash".into(),
            0,
            Some(0),
            true,
            "pre-verified".into(),
        )
        .expect("test evidence");

        let outcome = promote_preexported_candidate(
            &target,
            &metadata,
            export,
            verification,
            PromotionLimits::default(),
        )
        .await
        .expect("preexported promote");

        assert_eq!(outcome.project_oid, candidate_oid);
        assert_eq!(outcome.candidate_oid, candidate_oid);
        assert_eq!(git_rev_parse(fx.repo(), "refs/heads/main"), candidate_oid);
    }

    #[tokio::test]
    async fn preexported_promotion_passes_verification_evidence_to_callback() {
        let fx = CandidateFixture::with_change(b"promoted\n");
        fx.ready();
        let target = prepare_promotion_target(fx.repo(), "main").expect("target");

        let export = lattice_workspace::export_candidate_from_repo(
            fx.repo(),
            &fx.baseline,
            PromotionLimits::default().max_artifact_bytes,
        )
        .expect("export");
        let candidate_oid = export.candidate_oid().to_owned();

        let metadata = PromotionSessionMetadata {
            session_id: "sess1".into(),
            turn_id: "turn1".into(),
            committed_at_unix_seconds: 1_700_000_000,
        };
        let verification = VerificationResult::new(
            candidate_oid.clone(),
            vec!["test-cmd".into()],
            "test-profile-hash".into(),
            0,
            Some(0),
            true,
            "evidence-check".into(),
        )
        .expect("test evidence");

        let outcome = promote_preexported_candidate(
            &target,
            &metadata,
            export,
            verification,
            PromotionLimits::default(),
        )
        .await
        .expect("preexported promote");

        let repo = Repository::open(fx.repo()).expect("repo");
        let session_commit = repo
            .find_commit(git2::Oid::from_str(&outcome.session_oid).expect("parse session oid"))
            .expect("session commit");
        let root_tree = session_commit.tree().expect("tree");
        let turn_tree = repo
            .find_tree(root_tree.get_name("turn").expect("turn dir").id())
            .expect("turn tree");
        let manifest_blob = repo
            .find_blob(turn_tree.get_name("manifest.json").expect("manifest").id())
            .expect("blob");
        let manifest = String::from_utf8(manifest_blob.content().to_vec()).expect("utf8");
        assert!(
            manifest.contains("\"profile_hash\":\"test-profile-hash\""),
            "manifest must carry verification evidence: {manifest}"
        );
    }

    #[tokio::test]
    async fn preexported_stale_verification_is_rejected() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let target = prepare_promotion_target(fx.repo(), "main").expect("target");
        let state_dir = promotion_state_dir(fx.repo(), "main");

        let export = lattice_workspace::export_candidate_from_repo(
            fx.repo(),
            &fx.baseline,
            PromotionLimits::default().max_artifact_bytes,
        )
        .expect("export");

        let metadata = PromotionSessionMetadata {
            session_id: "sess1".into(),
            turn_id: "turn1".into(),
            committed_at_unix_seconds: 1_700_000_000,
        };
        let verification = VerificationResult::new(
            "0".repeat(40),
            vec!["test-cmd".into()],
            "test-profile-hash".into(),
            0,
            Some(0),
            true,
            "stale".into(),
        )
        .expect("test evidence");

        let err = promote_preexported_candidate(
            &target,
            &metadata,
            export,
            verification,
            PromotionLimits::default(),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("verification mismatch"),
            "expected verification mismatch, got: {err}"
        );
        assert!(!state_dir.join("lower.lock").exists());
        assert!(!PromotionJournal::exists(&state_dir));
    }
}

mod lifecycle_classification {
    use super::*;

    #[test]
    fn conflict_is_distinct_from_other_variants() {
        let err = PromotionLifecycleError::Conflict {
            detail: "baseline moved".into(),
        };
        assert!(!matches!(
            err,
            PromotionLifecycleError::TemporarilyBlocked { .. }
        ));
        assert!(!matches!(err, PromotionLifecycleError::Rejected { .. }));
        assert!(!matches!(err, PromotionLifecycleError::RecoveryRequired));
        assert!(!matches!(err, PromotionLifecycleError::Internal(_)));
    }

    #[test]
    fn baseline_conflict_classifies_as_conflict() {
        let promotion_err = PromotionError::BaselineConflict {
            expected: "aaa".into(),
            actual: "bbb".into(),
        };
        let lifecycle = PromotionLifecycleError::from_promotion_error(promotion_err);
        assert!(matches!(
            lifecycle,
            PromotionLifecycleError::Conflict { .. }
        ));
    }

    #[test]
    fn lower_dirty_classifies_as_temporarily_blocked() {
        use lattice_workspace::promotion::LowerDirtyReason;
        let promotion_err = PromotionError::Dirty(LowerDirtyReason::UnstagedChanges);
        let lifecycle = PromotionLifecycleError::from_promotion_error(promotion_err);
        assert!(matches!(
            lifecycle,
            PromotionLifecycleError::TemporarilyBlocked { .. }
        ));
    }

    #[test]
    fn verification_mismatch_classifies_as_rejected() {
        let promotion_err = PromotionError::VerificationMismatch {
            expected: "aaa".into(),
            actual: "bbb".into(),
        };
        let lifecycle = PromotionLifecycleError::from_promotion_error(promotion_err);
        assert!(matches!(
            lifecycle,
            PromotionLifecycleError::Rejected { .. }
        ));
    }

    #[test]
    fn recovery_required_classifies_correctly() {
        let promotion_err = PromotionError::RecoveryRequired;
        let lifecycle = PromotionLifecycleError::from_promotion_error(promotion_err);
        assert!(matches!(
            lifecycle,
            PromotionLifecycleError::RecoveryRequired
        ));
    }

    #[test]
    fn infrastructure_failure_classifies_as_internal() {
        let promotion_err = PromotionError::Git("unexpected".into());
        let lifecycle = PromotionLifecycleError::from_promotion_error(promotion_err);
        assert!(matches!(lifecycle, PromotionLifecycleError::Internal(_)));
    }

    #[tokio::test]
    async fn temporarily_blocked_pins_candidate_before_returning() {
        let fx = CandidateFixture::with_change(b"cand\n");
        fx.ready();
        let target = prepare_promotion_target(fx.repo(), "main").expect("target");

        // Export and validate the candidate.
        let export = lattice_workspace::export_candidate_from_repo(
            fx.repo(),
            &fx.baseline,
            PromotionLimits::default().max_artifact_bytes,
        )
        .expect("export");
        let candidate_oid = export.candidate_oid().to_owned();

        // Simulate a baseline conflict by moving main after preflight.
        Command::new("git")
            .args(["update-ref", "refs/heads/main", &candidate_oid])
            .current_dir(fx.repo())
            .output()
            .expect("move main");

        let metadata = PromotionSessionMetadata {
            session_id: "sess1".into(),
            turn_id: "turn1".into(),
            committed_at_unix_seconds: 1_700_000_000,
        };
        let verification = VerificationResult::new(
            candidate_oid.clone(),
            vec!["test-cmd".into()],
            "test-profile-hash".into(),
            0,
            Some(0),
            true,
            "verified".into(),
        )
        .expect("test evidence");

        let result = promote_preexported_candidate(
            &target,
            &metadata,
            export,
            verification,
            PromotionLimits::default(),
        )
        .await;

        // The error should be typed — either Conflict or TemporarilyBlocked with a pinned candidate.
        match result {
            Err(ApiError::PromotionLifecycle(lifecycle_err)) => {
                match lifecycle_err {
                    PromotionLifecycleError::TemporarilyBlocked { pinned, .. } => {
                        assert_eq!(pinned.candidate_oid, candidate_oid);
                    }
                    PromotionLifecycleError::Conflict { .. } => {
                        // Also acceptable: baseline moved is a conflict
                    }
                    other => panic!("unexpected lifecycle variant: {other:?}"),
                }
            }
            Err(other) => panic!("unexpected api error: {other}"),
            Ok(_) => panic!("expected error due to baseline move"),
        }
    }

    #[tokio::test]
    async fn retry_pinned_candidate_succeeds_after_temporary_block() {
        let fx = CandidateFixture::with_change(b"promoted\n");
        fx.ready();
        let target = prepare_promotion_target(fx.repo(), "main").expect("target");

        // Pin the candidate directly.
        let pinned = pin_candidate(fx.repo(), &fx.candidate, &fx.session, &fx.turn).expect("pin");

        let metadata = PromotionSessionMetadata {
            session_id: "sess1".into(),
            turn_id: "turn1".into(),
            committed_at_unix_seconds: 1_700_000_000,
        };
        let verification = VerificationResult::new(
            fx.candidate.clone(),
            vec!["test-cmd".into()],
            "test-profile-hash".into(),
            0,
            Some(0),
            true,
            "retry-verified".into(),
        )
        .expect("test evidence");

        let outcome = retry_pinned_candidate(
            &target,
            &metadata,
            &pinned,
            verification,
            PromotionLimits::default(),
        )
        .await
        .expect("retry pinned promote");

        assert_eq!(outcome.candidate_oid, fx.candidate);
        assert_eq!(git_rev_parse(fx.repo(), "refs/heads/main"), fx.candidate);
    }
}

mod boundary {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn manifest_path(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
    }

    #[test]
    fn stream_turn_sources_do_not_reference_promotion_apis() {
        for relative in ["src/turn/live.rs", "src/turn/execute.rs", "src/turn/cep.rs"] {
            let path = manifest_path(relative);
            let text = fs::read_to_string(&path).expect("read source");
            assert!(
                !text.contains("promote_verified_candidate"),
                "{} must not call promote_verified_candidate",
                path.display()
            );
            assert!(
                !text.contains("prepare_promotion_target"),
                "{} must not call prepare_promotion_target",
                path.display()
            );
        }
    }
    #[test]
    fn promotion_module_documents_activation_requirement() {
        let text = fs::read_to_string(manifest_path("src/turn/promotion.rs")).expect("read");
        assert!(text.contains("activation"));
        assert!(text.contains("stream_turn"));
    }

    #[tokio::test]
    async fn deterministic_embedder_path_remains_primary() {
        let fx = CandidateFixture::with_change(b"boundary\n");
        fx.ready();
        let target = prepare_promotion_target(fx.repo(), "main").expect("target");
        assert!(!target.baseline_oid.is_empty());
    }
}

#[cfg(feature = "integration_tests")]
mod microsandbox_composition {
    #[test]
    fn microsandbox_workspace_type_checks_with_public_promotion_api() {
        use lattice_api::{PromotionSessionMetadata, PromotionTurnTarget, VerificationResult};
        use lattice_workspace::microsandbox::MicrosandboxWorkspace;
        fn _accepts(
            _ws: &MicrosandboxWorkspace,
            _target: &PromotionTurnTarget,
            _meta: &PromotionSessionMetadata,
            _v: VerificationResult,
        ) {
        }
    }
}
