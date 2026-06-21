// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! V-03/V-07 adversarial candidate export and validation coverage.

#![cfg(feature = "test-utils")]

use std::path::{Path, PathBuf};
use std::process::Command;

use git2::{IndexAddOption, Repository, Signature};
use lattice_workspace::{
    promotion::{
        validate_candidate, CandidateHazard, PromotionLimits, VerificationResult, PromotionError,
    },
    testing::DeterministicWorkspace,
    EgressPolicy, ImageOrTemplate, UpperSpec, Workspace, WorkspaceError,
};

mod candidate_export {
    use super::*;

    pub(super) fn sig() -> Signature<'static> {
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
        repo.commit(
            Some("HEAD"),
            &sig(),
            &sig(),
            message,
            &tree,
            parents,
        )
        .expect("commit")
    }

    /// Baseline with one child commit (clean candidate ahead of baseline).
    pub(super) fn make_baseline_and_candidate() -> (tempfile::TempDir, String, String) {
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

        (
            dir,
            baseline.to_string(),
            candidate.to_string(),
        )
    }

    fn make_spec(host_repo_path: PathBuf, baseline_oid: String) -> UpperSpec {
        UpperSpec {
            image_or_template: ImageOrTemplate::Image(
                "docker.m.daocloud.io/library/alpine:3.19".into(),
            ),
            baseline_oid,
            host_repo_path,
            egress: EgressPolicy::Blocked,
            credentials: vec![],
            timeout_secs: 30,
            cpus: 1,
            memory_mb: 256,
        }
    }

    pub(super) async fn provision_upper(
        ws: &DeterministicWorkspace,
        baseline_oid: &str,
        host: &Path,
    ) -> lattice_workspace::UpperHandle {
        let spec = make_spec(host.to_path_buf(), baseline_oid.to_owned());
        ws.create_upper(&spec).await.expect("create_upper")
    }

    pub(super) async fn commit_candidate_in_upper(
        ws: &DeterministicWorkspace,
        handle: &lattice_workspace::UpperHandle,
    ) -> String {
        ws.exec_upper(
            handle,
            "sh",
            &[
                "-c",
                "echo candidate > candidate.txt && git add candidate.txt && git -c user.email=t@t.com -c user.name=t commit -m candidate",
            ],
        )
        .await
        .expect("commit in upper");
        let head = ws
            .exec_upper(handle, "git", &["rev-parse", "HEAD"])
            .await
            .expect("head");
        head.stdout.trim().to_string()
    }

    #[tokio::test]
    async fn clean_export_preserves_candidate_oid() {
        let (host_dir, baseline, _) = make_baseline_and_candidate();
        let ws = DeterministicWorkspace::new().expect("ws");
        let handle = provision_upper(&ws, &baseline, host_dir.path()).await;
        let candidate = commit_candidate_in_upper(&ws, &handle).await;

        let export = ws
            .export_candidate(&handle, &baseline, PromotionLimits::default().max_artifact_bytes)
            .await
            .expect("export");
        assert_eq!(export.baseline_oid(), baseline);
        assert_eq!(export.candidate_oid(), candidate);
        assert!(export.artifact_len() > 0);

        let verify = Command::new("git")
            .args(["bundle", "verify", export.path().to_str().unwrap()])
            .current_dir(host_dir.path())
            .output()
            .expect("verify");
        assert!(
            verify.status.success(),
            "bundle must verify: {}",
            String::from_utf8_lossy(&verify.stderr)
        );

        let unbundle = Command::new("git")
            .args(["bundle", "unbundle", export.path().to_str().unwrap()])
            .current_dir(host_dir.path())
            .output()
            .expect("unbundle");
        assert!(unbundle.status.success(), "unbundle must succeed");

        let kind = Command::new("git")
            .args(["cat-file", "-t", &candidate])
            .current_dir(host_dir.path())
            .output()
            .expect("cat-file");
        assert!(kind.status.success());
        assert_eq!(String::from_utf8_lossy(&kind.stdout).trim(), "commit");
    }

    #[tokio::test]
    async fn dirty_staged_rejects_export() {
        let (host_dir, baseline, _) = make_baseline_and_candidate();
        let ws = DeterministicWorkspace::new().expect("ws");
        let handle = provision_upper(&ws, &baseline, host_dir.path()).await;
        let _ = commit_candidate_in_upper(&ws, &handle).await;

        let out = ws
            .exec_upper(&handle, "git", &["add", "candidate.txt"])
            .await
            .expect("git add");
        assert_eq!(out.exit_code, 0);
        // Re-stage after modifying so porcelain shows staged changes.
        ws.exec_upper(
            &handle,
            "sh",
            &["-c", "echo staged >> candidate.txt && git add candidate.txt"],
        )
        .await
        .expect("stage modification");

        let err = ws
            .export_candidate(&handle, &baseline, PromotionLimits::default().max_artifact_bytes)
            .await
            .expect_err("staged changes must reject");
        assert!(matches!(err, WorkspaceError::UpperDirty { .. }));
    }

    #[tokio::test]
    async fn dirty_unstaged_rejects_export() {
        let (host_dir, baseline, _) = make_baseline_and_candidate();
        let ws = DeterministicWorkspace::new().expect("ws");
        let handle = provision_upper(&ws, &baseline, host_dir.path()).await;
        let _ = commit_candidate_in_upper(&ws, &handle).await;

        ws.exec_upper(
            &handle,
            "sh",
            &["-c", "echo dirty >> candidate.txt"],
        )
        .await
        .expect("modify");

        let err = ws
            .export_candidate(&handle, &baseline, PromotionLimits::default().max_artifact_bytes)
            .await
            .expect_err("unstaged changes must reject");
        assert!(matches!(err, WorkspaceError::UpperDirty { .. }));
    }

    #[tokio::test]
    async fn dirty_untracked_rejects_export() {
        let (host_dir, baseline, _) = make_baseline_and_candidate();
        let ws = DeterministicWorkspace::new().expect("ws");
        let handle = provision_upper(&ws, &baseline, host_dir.path()).await;
        let _ = commit_candidate_in_upper(&ws, &handle).await;

        ws.exec_upper(&handle, "sh", &["-c", "echo x > untracked.txt"])
            .await
            .expect("untracked");

        let err = ws
            .export_candidate(&handle, &baseline, PromotionLimits::default().max_artifact_bytes)
            .await
            .expect_err("untracked must reject");
        assert!(matches!(err, WorkspaceError::UpperDirty { .. }));
    }

    #[tokio::test]
    async fn artifact_byte_limit_boundary() {
        let (host_dir, baseline, _) = make_baseline_and_candidate();
        let ws = DeterministicWorkspace::new().expect("ws");
        let handle = provision_upper(&ws, &baseline, host_dir.path()).await;
        let _ = commit_candidate_in_upper(&ws, &handle).await;

        let export = ws
            .export_candidate(&handle, &baseline, u64::MAX)
            .await
            .expect("export");
        let len = export.artifact_len();

        ws.export_candidate(&handle, &baseline, len)
            .await
            .expect("at-limit must succeed");
        let err = ws
            .export_candidate(&handle, &baseline, len.saturating_sub(1))
            .await
            .expect_err("over-limit must fail");
        assert!(matches!(err, WorkspaceError::ArtifactTooLarge { .. }));
    }
}

mod candidate_validation {
    use super::*;

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
        repo.commit(
            Some("HEAD"),
            &sig(),
            &sig(),
            message,
            &tree,
            parents,
        )
        .expect("commit")
    }

    struct HostFixture {
        _dir: tempfile::TempDir,
        repo_path: PathBuf,
        baseline: String,
        candidate: String,
    }

    impl HostFixture {
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
            Self {
                repo_path: dir.path().to_path_buf(),
                baseline: baseline.to_string(),
                candidate: candidate.to_string(),
                _dir: dir,
            }
        }

        async fn export(&self) -> lattice_workspace::CandidateExport {
            let ws = DeterministicWorkspace::new().expect("ws");
            let spec = UpperSpec {
                image_or_template: ImageOrTemplate::Image("img".into()),
                baseline_oid: self.baseline.clone(),
                host_repo_path: self.repo_path.clone(),
                egress: EgressPolicy::Blocked,
                credentials: vec![],
                timeout_secs: 30,
                cpus: 1,
                memory_mb: 256,
            };
            let handle = ws.create_upper(&spec).await.expect("upper");
            candidate_export::commit_candidate_in_upper(&ws, &handle).await;
            ws.export_candidate(
                &handle,
                &self.baseline,
                PromotionLimits::default().max_artifact_bytes,
            )
            .await
            .expect("export")
        }

        fn verification(&self, passed: bool) -> VerificationResult {
            VerificationResult {
                candidate_oid: self.candidate.clone(),
                passed,
                summary: "test".into(),
            }
        }
    }

    fn snapshot_repo_state(repo_path: &Path) -> RepoSnapshot {
        let git_common = git_output(repo_path, &["rev-parse", "--git-common-dir"]).unwrap_or_default();
        let lock_path = PathBuf::from(git_common)
            .join("lattice/promotion")
            .join(hex_branch("main"))
            .join("lower.lock");
        RepoSnapshot {
            project_ref: git_output(repo_path, &["rev-parse", "refs/heads/main"]).unwrap_or_default(),
            session_ref: git_output(
                repo_path,
                &["rev-parse", "refs/lattice/sessions/s1/main"],
            )
            .ok(),
            candidate_ref: git_output(
                repo_path,
                &["rev-parse", "refs/lattice/candidates/s1/t1"],
            )
            .ok(),
            lower_head: None,
            lock_exists: lock_path.exists(),
            journal_exists: lock_path
                .parent()
                .map(|p| p.join("promotion-journal.jsonl").exists())
                .unwrap_or(false),
        }
    }

    fn hex_branch(branch: &str) -> String {
        branch.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct RepoSnapshot {
        project_ref: String,
        session_ref: Option<String>,
        candidate_ref: Option<String>,
        lower_head: Option<String>,
        lock_exists: bool,
        journal_exists: bool,
    }

    fn git_output(cwd: &Path, args: &[&str]) -> Result<String, String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).into_owned())
        }
    }

    fn setup_branch(repo_path: &Path, baseline_oid: &str) {
        Command::new("git")
            .args(["update-ref", "refs/heads/main", baseline_oid])
            .current_dir(repo_path)
            .status()
            .expect("update-ref main");
        Command::new("git")
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .current_dir(repo_path)
            .status()
            .expect("symbolic-ref main");
        Command::new("git")
            .args([
                "update-ref",
                "refs/lattice/sessions/s1/main",
                "refs/heads/main",
            ])
            .current_dir(repo_path)
            .status()
            .expect("session ref");
        Command::new("git")
            .args([
                "update-ref",
                "refs/lattice/candidates/s1/t1",
                "refs/heads/main",
            ])
            .current_dir(repo_path)
            .status()
            .expect("candidate ref");
    }

    #[tokio::test]
    async fn verification_mismatch_rejects_before_import() {
        let fx = HostFixture::new();
        setup_branch(&fx.repo_path, &fx.baseline);
        let export = fx.export().await;
        let before = snapshot_repo_state(&fx.repo_path);
        let mut bad = fx.verification(true);
        bad.candidate_oid = "0".repeat(40);
        let err = validate_candidate(
            &fx.repo_path,
            &export,
            &fx.baseline,
            &bad,
            &PromotionLimits::default(),
        )
        .expect_err("verification mismatch");
        assert!(matches!(err, PromotionError::VerificationMismatch { .. }));
        let after = snapshot_repo_state(&fx.repo_path);
        assert_eq!(before.project_ref, after.project_ref);
        assert_eq!(before.lock_exists, after.lock_exists);
    }

    #[tokio::test]
    async fn failed_verification_rejects() {
        let fx = HostFixture::new();
        setup_branch(&fx.repo_path, &fx.baseline);
        let export = fx.export().await;
        let err = validate_candidate(
            &fx.repo_path,
            &export,
            &fx.baseline,
            &fx.verification(false),
            &PromotionLimits::default(),
        )
        .expect_err("failed verification");
        assert!(matches!(err, PromotionError::VerificationMismatch { .. }));
    }

    #[tokio::test]
    async fn valid_candidate_passes_validation() {
        let fx = HostFixture::new();
        setup_branch(&fx.repo_path, &fx.baseline);
        let export = fx.export().await;
        let validated = validate_candidate(
            &fx.repo_path,
            &export,
            &fx.baseline,
            &VerificationResult {
                candidate_oid: export.candidate_oid().to_string(),
                passed: true,
                summary: "test".into(),
            },
            &PromotionLimits::default(),
        )
        .expect("validate");
        assert_eq!(validated.candidate_oid, export.candidate_oid());
        assert_eq!(validated.baseline_oid, fx.baseline);
    }

    #[tokio::test]
    async fn malformed_bundle_rejects() {
        let fx = HostFixture::new();
        let mut file = tempfile::NamedTempFile::new().expect("temp");
        std::io::Write::write_all(&mut file, b"not a bundle").expect("write");
        let export = lattice_workspace::CandidateExport::from_test_artifact(
            file,
            fx.baseline.clone(),
            fx.candidate.clone(),
            11,
        );
        let err = validate_candidate(
            &fx.repo_path,
            &export,
            &fx.baseline,
            &fx.verification(true),
            &PromotionLimits::default(),
        )
        .expect_err("malformed");
        assert!(matches!(err, PromotionError::MalformedBundle));
    }

    #[tokio::test]
    async fn object_count_limit_boundary() {
        let fx = HostFixture::new();
        setup_branch(&fx.repo_path, &fx.baseline);
        let export = fx.export().await;
        let limits = PromotionLimits {
            max_object_count: 1,
            ..PromotionLimits::default()
        };
        let err = validate_candidate(
            &fx.repo_path,
            &export,
            &fx.baseline,
            &VerificationResult {
                candidate_oid: export.candidate_oid().to_string(),
                passed: true,
                summary: "test".into(),
            },
            &limits,
        )
        .expect_err("object count");
        assert!(
            matches!(err, PromotionError::ObjectCountExceeded { .. }),
            "unexpected error: {err:?}"
        );

        let limits_ok = PromotionLimits {
            max_object_count: 100,
            ..PromotionLimits::default()
        };
        validate_candidate(
            &fx.repo_path,
            &export,
            &fx.baseline,
            &VerificationResult {
                candidate_oid: export.candidate_oid().to_string(),
                passed: true,
                summary: "test".into(),
            },
            &limits_ok,
        )
        .expect("within limit");
    }

    #[tokio::test]
    async fn gitmodules_change_rejects() {
        let (host_dir, baseline, _) = candidate_export::make_baseline_and_candidate();
        setup_branch(host_dir.path(), &baseline);
        let ws = DeterministicWorkspace::new().expect("ws");
        let handle = candidate_export::provision_upper(&ws, &baseline, host_dir.path()).await;
        ws.exec_upper(
            &handle,
            "sh",
            &[
                "-c",
                "printf '[submodule \"s\"]\\n\\tpath = s\\n\\turl = u\\n' > .gitmodules && git add .gitmodules && git -c user.email=t@t.com -c user.name=t commit -m modules",
            ],
        )
        .await
        .expect("commit gitmodules");
        let export = ws
            .export_candidate(&handle, &baseline, PromotionLimits::default().max_artifact_bytes)
            .await
            .expect("export");
        let err = validate_candidate(
            host_dir.path(),
            &export,
            &baseline,
            &VerificationResult {
                candidate_oid: export.candidate_oid().to_string(),
                passed: true,
                summary: "test".into(),
            },
            &PromotionLimits::default(),
        )
        .expect_err("gitmodules");
        assert!(
            matches!(err, PromotionError::CandidateHazard(_)),
            "unexpected error: {err:?}"
        );
        if let PromotionError::CandidateHazard(h) = err {
            assert!(matches!(h, CandidateHazard::GitmodulesChanged { .. }));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_add_rejects() {
        let (host_dir, baseline, _) = candidate_export::make_baseline_and_candidate();
        setup_branch(host_dir.path(), &baseline);
        let ws = DeterministicWorkspace::new().expect("ws");
        let handle = candidate_export::provision_upper(&ws, &baseline, host_dir.path()).await;
        ws.exec_upper(
            &handle,
            "sh",
            &[
                "-c",
                "ln -s target link && git add link && git -c user.email=t@t.com -c user.name=t commit -m link",
            ],
        )
        .await
        .expect("commit symlink");
        let export = ws
            .export_candidate(&handle, &baseline, PromotionLimits::default().max_artifact_bytes)
            .await
            .expect("export");
        let err = validate_candidate(
            host_dir.path(),
            &export,
            &baseline,
            &VerificationResult {
                candidate_oid: export.candidate_oid().to_string(),
                passed: true,
                summary: "test".into(),
            },
            &PromotionLimits::default(),
        )
        .expect_err("symlink");
        assert!(
            matches!(err, PromotionError::CandidateHazard(_)),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn rejection_leaves_refs_unchanged() {
        let fx = HostFixture::new();
        setup_branch(&fx.repo_path, &fx.baseline);
        let export = fx.export().await;
        let before = snapshot_repo_state(&fx.repo_path);
        let _ = validate_candidate(
            &fx.repo_path,
            &export,
            &fx.baseline,
            &fx.verification(false),
            &PromotionLimits::default(),
        );
        let after = snapshot_repo_state(&fx.repo_path);
        assert_eq!(before, after);
    }
}
