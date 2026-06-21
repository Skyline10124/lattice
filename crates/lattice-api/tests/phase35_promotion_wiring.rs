// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! V-09 lattice-api promotion composition and Phase 35/36 boundary coverage.

use std::path::{Path, PathBuf};
use std::process::Command;

use git2::Repository;
use lattice_api::{
    prepare_promotion_target, promote_verified_candidate, ApiError, PromotionLimits,
    PromotionSessionMetadata, PromotionTurnTarget, VerificationResult,
};
use lattice_workspace::{
    promotion::{
        commit, prepare, recover, FaultPoint, ManagedLower, PromotionError, PromotionJournal,
        PromotionRefs, RefComponent, RecoveryOutcome,
    },
    testing::DeterministicWorkspace,
    EgressPolicy, ImageOrTemplate, UpperSpec, Workspace,
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
        verification: VerificationResult {
            candidate_oid: candidate.to_owned(),
            passed: true,
            summary: "test".into(),
        },
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
        Some(oid) => vec![repo
            .find_commit(oid)
            .map_err(|e| PromotionError::Git(e.to_string()))?],
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

async fn commit_candidate_in_upper(ws: &DeterministicWorkspace, handle: &lattice_workspace::UpperHandle) {
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
        assert!(msg.contains("lower dirty"), "expected dirty error, got {msg}");
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
        assert!(PromotionJournal::exists(&promotion_state_dir(fx.repo(), "main")));
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
        let verification = VerificationResult {
            candidate_oid: "0".repeat(40),
            passed: true,
            summary: "stale".into(),
        };

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
        let verification = VerificationResult {
            candidate_oid: candidate_oid.clone(),
            passed: true,
            summary: "verified".into(),
        };

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
        assert_eq!(
            git_rev_parse(fx.repo(), "refs/heads/main"),
            candidate_oid
        );
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
            .find_commit(
                git2::Oid::from_str(&outcome.session_oid).expect("parse session oid"),
            )
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
            let _ = (target.repository_path, target.baseline_oid, metadata, verification, limits);
        }
    }
}

mod phase_boundary {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn manifest_path(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
    }

    #[test]
    fn stream_turn_sources_do_not_reference_promotion_apis() {
        for relative in [
            "src/turn/live.rs",
            "src/turn/execute.rs",
            "src/turn/cep.rs",
        ] {
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
    fn production_code_does_not_fabricate_passing_verification() {
        fn scan_dir(dir: &Path, errors: &mut Vec<String>) {
            for entry in fs::read_dir(dir).expect("read dir") {
                let entry = entry.expect("entry");
                let path = entry.path();
                if path.is_dir() {
                    scan_dir(&path, errors);
                    continue;
                }
                if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = fs::read_to_string(&path).expect("read");
                    if text.contains("passed: true") {
                        errors.push(format!(
                            "{} must not construct passing VerificationResult",
                            path.display()
                        ));
                    }
                }
            }
        }

        let mut errors = Vec::new();
        scan_dir(manifest_path("src").as_path(), &mut errors);
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }

    #[test]
    fn promotion_module_documents_phase36_activation_requirement() {
        let text = fs::read_to_string(manifest_path("src/turn/promotion.rs")).expect("read");
        assert!(text.contains("Phase 36"));
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
        use lattice_workspace::MicrosandboxWorkspace;
        fn _accepts(_ws: &MicrosandboxWorkspace, _target: &PromotionTurnTarget, _meta: &PromotionSessionMetadata, _v: VerificationResult) {}
    }
}
