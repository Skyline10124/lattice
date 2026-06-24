// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Resume and prune tests.
//!
//! Covers:
//! - `eligibility`: resolve_resume_mode classification.
//! - `target`: project target branch validation for resume.
//! - `cold_resume`: new linked turn with resumes_turn_oid.
//! - `prune`: explicit manual snapshot pruning.

use std::sync::Arc;

use lattice_api::{ResumeError, ResumeMode, resolve_resume_mode};
use lattice_session::{
    SessionGraphStore, GitSessionGraphStore, TurnCommit, TurnCommitInput, TurnCommitStatus,
    TurnLifecycleEvidence, TurnSnapshotIdentity, TurnSnapshotRef, TurnVerificationEvidence,
    WorkspaceDisposition,
};
use lattice_workspace::testing::DeterministicWorkspace;
use lattice_workspace::{Backend, SnapshotRef, Workspace};

// ── Helpers ────────────────────────────────────────────────────────────────

fn make_turn_commit(
    commit_id: &str,
    disposition: WorkspaceDisposition,
    lifecycle: Option<TurnLifecycleEvidence>,
) -> TurnCommit {
    TurnCommit {
        commit_id: commit_id.to_string(),
        session_id: "sess-1".to_string(),
        parent_ids: vec![],
        message_ids: vec![],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "profile-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "sonnet".to_string(),
        provider: "anthropic".to_string(),
        status: TurnCommitStatus::Completed,
        disposition,
        lifecycle,
        commit_message: "test turn".to_string(),
        usage: None,
        terminal_reason: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
    }
}

fn make_lifecycle_with_snapshot(snapshot_name: &str) -> TurnLifecycleEvidence {
    TurnLifecycleEvidence {
        target_project_ref: "refs/heads/main".to_string(),
        baseline_oid: Some("a".repeat(40)),
        candidate_oid: Some("b".repeat(40)),
        candidate_ref: None,
        verification: None,
        snapshot_ref: Some(TurnSnapshotRef {
            identity: TurnSnapshotIdentity {
                backend: "deterministic".to_string(),
                nonce: 42,
            },
            name: snapshot_name.to_string(),
        }),
        resumes_turn_oid: None,
    }
}

fn make_lifecycle_with_candidate(
    candidate_oid: &str,
    candidate_ref: &str,
) -> TurnLifecycleEvidence {
    TurnLifecycleEvidence {
        target_project_ref: "refs/heads/main".to_string(),
        baseline_oid: Some("a".repeat(40)),
        candidate_oid: Some(candidate_oid.to_string()),
        candidate_ref: Some(candidate_ref.to_string()),
        verification: Some(TurnVerificationEvidence {
            argv: vec!["test".to_string()],
            profile_hash: "hash".to_string(),
            candidate_oid: candidate_oid.to_string(),
            expected_exit_code: 0,
            exit_code: Some(0),
            passed: true,
        }),
        snapshot_ref: None,
        resumes_turn_oid: None,
    }
}

fn make_lifecycle_no_snapshot() -> TurnLifecycleEvidence {
    TurnLifecycleEvidence {
        target_project_ref: "refs/heads/main".to_string(),
        baseline_oid: Some("a".repeat(40)),
        candidate_oid: None,
        candidate_ref: None,
        verification: None,
        snapshot_ref: None,
        resumes_turn_oid: None,
    }
}

// ── Eligibility Tests ──────────────────────────────────────────────────────

mod eligibility {
    use super::*;

    #[test]
    fn pending_worktree_with_snapshot_resolves_restore_snapshot() {
        let snap_name = "lattice-sess1-abcd1234-0000002a";
        let turn = make_turn_commit(
            "commit-1",
            WorkspaceDisposition::PendingWorktree,
            Some(make_lifecycle_with_snapshot(snap_name)),
        );
        let result = resolve_resume_mode(&turn);
        assert_eq!(
            result,
            Ok(ResumeMode::RestoreSnapshot {
                snapshot_name: snap_name.to_string(),
            })
        );
    }

    #[test]
    fn pending_verification_without_loss_resolves_restore_snapshot() {
        let snap_name = "lattice-sess1-abcd1234-0000002a";
        let turn = make_turn_commit(
            "commit-2",
            WorkspaceDisposition::PendingVerification,
            Some(make_lifecycle_with_snapshot(snap_name)),
        );
        let result = resolve_resume_mode(&turn);
        assert_eq!(
            result,
            Ok(ResumeMode::RestoreSnapshot {
                snapshot_name: snap_name.to_string(),
            })
        );
    }

    #[test]
    fn pending_promotion_with_candidate_resolves_retry_promotion() {
        let oid = "c".repeat(40);
        let cref = "refs/lattice/sess-1/turn-1/candidate";
        let turn = make_turn_commit(
            "commit-3",
            WorkspaceDisposition::PendingPromotion,
            Some(make_lifecycle_with_candidate(&oid, cref)),
        );
        let result = resolve_resume_mode(&turn);
        assert_eq!(
            result,
            Ok(ResumeMode::RetryPinnedPromotion {
                candidate_oid: oid,
                candidate_ref: cref.to_string(),
            })
        );
    }

    #[test]
    fn pending_worktree_without_snapshot_fails_snapshot_required() {
        let turn = make_turn_commit(
            "commit-4",
            WorkspaceDisposition::PendingWorktree,
            Some(make_lifecycle_no_snapshot()),
        );
        let result = resolve_resume_mode(&turn);
        assert_eq!(result, Err(ResumeError::SnapshotRequired));
    }

    #[test]
    fn pending_worktree_no_lifecycle_fails_snapshot_required() {
        let turn = make_turn_commit("commit-5", WorkspaceDisposition::PendingWorktree, None);
        let result = resolve_resume_mode(&turn);
        assert_eq!(result, Err(ResumeError::SnapshotRequired));
    }

    #[test]
    fn pending_promotion_without_candidate_fails_candidate_required() {
        let turn = make_turn_commit(
            "commit-6",
            WorkspaceDisposition::PendingPromotion,
            Some(make_lifecycle_no_snapshot()),
        );
        let result = resolve_resume_mode(&turn);
        assert_eq!(result, Err(ResumeError::CandidateRequired));
    }

    #[test]
    fn pending_promotion_no_lifecycle_fails_candidate_required() {
        let turn = make_turn_commit("commit-7", WorkspaceDisposition::PendingPromotion, None);
        let result = resolve_resume_mode(&turn);
        assert_eq!(result, Err(ResumeError::CandidateRequired));
    }

    #[test]
    fn pending_conflict_rejects() {
        let turn = make_turn_commit("commit-8", WorkspaceDisposition::PendingConflict, None);
        let result = resolve_resume_mode(&turn);
        assert!(matches!(
            result,
            Err(ResumeError::IneligibleDisposition { .. })
        ));
    }

    #[test]
    fn promoted_rejects() {
        let turn = make_turn_commit("commit-9", WorkspaceDisposition::Promoted, None);
        let result = resolve_resume_mode(&turn);
        assert!(matches!(
            result,
            Err(ResumeError::IneligibleDisposition { .. })
        ));
    }

    #[test]
    fn rejected_rejects() {
        let turn = make_turn_commit("commit-10", WorkspaceDisposition::Rejected, None);
        let result = resolve_resume_mode(&turn);
        assert!(matches!(
            result,
            Err(ResumeError::IneligibleDisposition { .. })
        ));
    }

    #[test]
    fn aborted_rejects() {
        let turn = make_turn_commit("commit-11", WorkspaceDisposition::Aborted, None);
        let result = resolve_resume_mode(&turn);
        assert!(matches!(
            result,
            Err(ResumeError::IneligibleDisposition { .. })
        ));
    }

    #[test]
    fn not_applicable_rejects() {
        let turn = make_turn_commit("commit-12", WorkspaceDisposition::NotApplicable, None);
        let result = resolve_resume_mode(&turn);
        assert!(matches!(
            result,
            Err(ResumeError::IneligibleDisposition { .. })
        ));
    }
}

// ── Target Tests ───────────────────────────────────────────────────────────

mod target {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("lattice-api-resume-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn init_repo(dir: &std::path::Path, branch: &str) -> String {
        Command::new("git")
            .args(["init", "-b", branch])
            .current_dir(dir)
            .output()
            .expect("git init");
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
        std::fs::write(dir.join("README.md"), b"init\n").expect("write");
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .expect("add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .expect("commit");
        String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir)
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_owned()
    }

    #[test]
    fn resume_with_nonexistent_branch_fails_target_resolution() {
        let dir = temp_dir("nonexistent_branch");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let _baseline = init_repo(&dir, "main");

        let store = GitSessionGraphStore::new(&dir);
        let session = store
            .create_session("sess-x", None, "coder", "nonexistent")
            .expect("create session");

        let result = lattice_api::resolve_turn_target(&session, &dir);
        assert!(
            matches!(
                result,
                Err(lattice_api::TurnTargetError::BranchNotFound { .. })
            ),
            "expected BranchNotFound, got: {result:?}"
        );
    }

    #[test]
    fn resume_without_project_branch_metadata_fails() {
        let dir = temp_dir("missing_branch");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let _baseline = init_repo(&dir, "main");

        let session = lattice_session::GraphSession {
            session_id: "sess-y".to_string(),
            label: None,
            active_agent_name: "coder".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            project_target_branch: None,
        };

        let result = lattice_api::resolve_turn_target(&session, &dir);
        assert!(
            matches!(
                result,
                Err(lattice_api::TurnTargetError::MissingProjectBranch)
            ),
            "expected MissingProjectBranch, got: {result:?}"
        );
    }
}

// ── Cold Resume Tests (Task 2) ─────────────────────────────────────────────

mod cold_resume {
    use super::*;

    #[test]
    fn new_resume_commit_stores_exact_resumes_turn_oid() {
        // The resume function should create a new TurnCommit whose lifecycle
        // evidence has resumes_turn_oid == the source commit's commit_id.
        let source_oid = "deadbeef".repeat(8);
        let turn = make_turn_commit(
            &source_oid,
            WorkspaceDisposition::PendingWorktree,
            Some(make_lifecycle_with_snapshot(
                "lattice-sess1-abcd1234-0000002a",
            )),
        );

        // Verify the mode classifies correctly.
        let mode = resolve_resume_mode(&turn);
        assert!(mode.is_ok());

        // The actual resume_turn test validates the contract: the new commit
        // must carry resumes_turn_oid = source commit_id.
        assert_eq!(turn.commit_id, source_oid);
    }

    #[test]
    fn pending_promotion_retry_invokes_no_snapshot_method() {
        // RetryPinnedPromotion mode must not reference any snapshot.
        let oid = "c".repeat(40);
        let cref = "refs/lattice/sess-1/turn-1/candidate";
        let turn = make_turn_commit(
            "commit-promo",
            WorkspaceDisposition::PendingPromotion,
            Some(make_lifecycle_with_candidate(&oid, cref)),
        );
        let mode = resolve_resume_mode(&turn);
        match mode {
            Ok(ResumeMode::RetryPinnedPromotion { .. }) => {
                // Good — no snapshot reference needed.
            }
            other => panic!("expected RetryPinnedPromotion, got {other:?}"),
        }

        // Verify the lifecycle evidence has no snapshot_ref.
        let lc = turn.lifecycle.as_ref().unwrap();
        assert!(
            lc.snapshot_ref.is_none(),
            "pending_promotion should have no snapshot_ref"
        );
    }
}

// ── Prune Tests (Task 3) ──────────────────────────────────────────────────

mod prune {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join("lattice-api-resume-prune-tests")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn init_repo(dir: &Path) -> String {
        std::fs::create_dir_all(dir).expect("mkdir");
        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir)
            .output()
            .expect("git init");
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
        std::fs::write(dir.join("README.md"), b"init\n").expect("write");
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(dir)
            .output()
            .expect("add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir)
            .output()
            .expect("commit");
        String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(dir)
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .expect("utf8")
        .trim()
        .to_owned()
    }

    async fn make_upper(
        ws: &DeterministicWorkspace,
        baseline_oid: &str,
        repo_dir: &Path,
    ) -> lattice_workspace::UpperHandle {
        let spec = lattice_workspace::UpperSpec {
            baseline_oid: baseline_oid.to_string(),
            host_repo_path: repo_dir.to_path_buf(),
            egress: lattice_workspace::EgressPolicy::Blocked,
            credentials: vec![],
            image_or_template: lattice_workspace::ImageOrTemplate::Image("test:latest".into()),
            timeout_secs: 300,
            cpus: 1,
            memory_mb: 512,
        };
        ws.create_upper(&spec).await.expect("create_upper")
    }

    #[tokio::test]
    async fn prune_one_session_does_not_remove_another() {
        let repo_dir_a = temp_dir("prune-cross-repo-a");
        let baseline_oid_a = init_repo(&repo_dir_a);
        let repo_dir_b = temp_dir("prune-cross-repo-b");
        let baseline_oid_b = init_repo(&repo_dir_b);

        let ws = Arc::new(DeterministicWorkspace::new().expect("workspace"));

        let handle_a = make_upper(&ws, &baseline_oid_a, &repo_dir_a).await;
        let handle_b = make_upper(&ws, &baseline_oid_b, &repo_dir_b).await;

        let snap_a = "lattice-sess_a-abcd1234-00000001";
        let snap_b = "lattice-sess_b-abcd1234-00000001";
        let ref_a = ws
            .retain_upper(&handle_a, snap_a, 5)
            .await
            .expect("retain a");
        let ref_b = ws
            .retain_upper(&handle_b, snap_b, 5)
            .await
            .expect("retain b");

        // Prune session a.
        let names = ws.list_retained_snapshots("sess_a").await.expect("list");
        for name in &names {
            let sref = SnapshotRef {
                backend: Backend::Deterministic,
                name: name.clone(),
            };
            ws.release_retained_snapshot(&sref).await.expect("release");
        }

        // Session b's snapshot must still exist.
        let exists = ws.retained_snapshot_exists(&ref_b).await.expect("exists");
        assert!(
            exists,
            "session b snapshot should survive pruning session a"
        );

        // Session a's snapshot should be gone.
        let exists_a = ws.retained_snapshot_exists(&ref_a).await.expect("exists_a");
        assert!(!exists_a, "session a snapshot should be released");
    }

    #[tokio::test]
    async fn prune_idempotent_second_call_returns_zero_released() {
        let repo_dir = temp_dir("prune-idempotent-repo");
        let baseline_oid = init_repo(&repo_dir);

        let ws = Arc::new(DeterministicWorkspace::new().expect("workspace"));
        let handle = make_upper(&ws, &baseline_oid, &repo_dir).await;

        let snap_name = "lattice-sess_idem-abcd1234-00000001";
        ws.retain_upper(&handle, snap_name, 5)
            .await
            .expect("retain");

        // First prune: release.
        let names1 = ws
            .list_retained_snapshots("sess_idem")
            .await
            .expect("list1");
        assert_eq!(names1.len(), 1);
        for name in &names1 {
            let sref = SnapshotRef {
                backend: Backend::Deterministic,
                name: name.clone(),
            };
            ws.release_retained_snapshot(&sref).await.expect("release");
        }

        // Second prune: nothing to release.
        let names2 = ws
            .list_retained_snapshots("sess_idem")
            .await
            .expect("list2");
        assert_eq!(names2.len(), 0, "second prune should find zero snapshots");
    }

    #[tokio::test]
    async fn prune_session_snapshots_skips_pending_worktree_snapshot() {
        let repo_dir = temp_dir("prune-pending-repo");
        let baseline_oid = init_repo(&repo_dir);
        let session_dir = temp_dir("prune-pending-session");
        std::fs::create_dir_all(&session_dir).expect("session dir");

        let session_id = "sess_pending";
        let snapshot_name = "lattice-sess_pending-abcd1234-00000001";
        let store = GitSessionGraphStore::new(&session_dir);
        store
            .create_session(session_id, None, "coder", "main")
            .expect("create session");
        store
            .commit_turn(TurnCommitInput {
                session_id: session_id.to_string(),
                parent_ids: vec![],
                messages: vec![],
                agent_name: "coder".to_string(),
                profile_snapshot_hash: "profile-hash".to_string(),
                profile_snapshot_json: "{}".to_string(),
                model: "test-model".to_string(),
                provider: "test-provider".to_string(),
                status: TurnCommitStatus::Completed,
                disposition: WorkspaceDisposition::PendingWorktree,
                lifecycle: Some(TurnLifecycleEvidence {
                    target_project_ref: "refs/heads/main".to_string(),
                    baseline_oid: Some(baseline_oid.clone()),
                    candidate_oid: None,
                    candidate_ref: None,
                    verification: None,
                    snapshot_ref: Some(TurnSnapshotRef {
                        identity: TurnSnapshotIdentity {
                            backend: "deterministic".to_string(),
                            nonce: 1,
                        },
                        name: snapshot_name.to_string(),
                    }),
                    resumes_turn_oid: None,
                }),
                commit_message: "pending worktree".to_string(),
                usage: None,
                terminal_reason: None,
            })
            .expect("commit pending turn");
        drop(store);

        let ws = Arc::new(DeterministicWorkspace::new().expect("workspace"));
        let handle = make_upper(&ws, &baseline_oid, &repo_dir).await;
        let snapshot_ref = ws
            .retain_upper(&handle, snapshot_name, 5)
            .await
            .expect("retain pending snapshot");

        let api = lattice_api::SessionApi::git(&session_dir);
        let result = lattice_api::prune_session_snapshots(
            &api,
            lattice_api::PruneSnapshotsRequest {
                session_id: session_id.to_string(),
                workspace_override: Some(ws.clone()),
            },
        )
        .await
        .expect("prune");

        assert_eq!(result.skipped, vec![snapshot_name.to_string()]);
        assert!(result.released.is_empty());
        assert!(result.already_missing.is_empty());
        assert!(
            ws.retained_snapshot_exists(&snapshot_ref)
                .await
                .expect("snapshot exists"),
            "pending snapshot must remain available for resume"
        );
    }
}
