// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! V-04/V-08 managed Lower provisioning, cleanliness, and lock coverage.

use std::path::{Path, PathBuf};
use std::process::Command;

use lattice_workspace::promotion::{LowerDirtyReason, ManagedLower, PromotionError};

fn init_repo(dir: &Path, branch: &str) -> String {
    Command::new("git")
        .args(["init", "-b", branch])
        .current_dir(dir)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@localhost"])
        .current_dir(dir)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(dir)
        .output()
        .expect("git config name");
    std::fs::write(dir.join("README.md"), b"init\n").expect("write readme");
    Command::new("git")
        .args(["add", "README.md"])
        .current_dir(dir)
        .output()
        .expect("git add");
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(dir)
        .output()
        .expect("git commit");
    git_rev_parse(dir)
}

fn git_rev_parse(dir: &Path) -> String {
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

fn git_common_dir(repo: &Path) -> PathBuf {
    let raw = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "--git-common-dir"])
            .current_dir(repo)
            .output()
            .expect("common dir")
            .stdout,
    )
    .expect("utf8")
    .trim()
    .to_owned();
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        repo.join(path)
    }
}

fn managed_worktree_path(repo: &Path, branch: &str) -> PathBuf {
    let hex = branch
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    git_common_dir(repo).join("lattice/worktrees").join(hex)
}

fn dirty_reason(err: PromotionError) -> LowerDirtyReason {
    match err {
        PromotionError::Dirty(reason) => reason,
        other => panic!("expected dirty error, got {other:?}"),
    }
}

mod managed_lower {
    use super::*;

    #[test]
    fn provisions_linked_worktree_and_clean_baseline() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        detach_primary(dir.path());
        let lower = ManagedLower::provision_or_open(dir.path(), "main").expect("provision");
        assert_eq!(
            lower.worktree_path(),
            managed_worktree_path(dir.path(), "main")
        );
        let baseline = lower.inspect_clean().expect("clean");
        assert_eq!(baseline.project_ref, "refs/heads/main");
        assert_eq!(baseline.baseline_oid, git_rev_parse(lower.worktree_path()));
    }

    #[test]
    fn reopens_existing_managed_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        detach_primary(dir.path());
        let first = ManagedLower::provision_or_open(dir.path(), "main").expect("first");
        let second = ManagedLower::provision_or_open(dir.path(), "main").expect("second");
        assert_eq!(first.worktree_path(), second.worktree_path());
    }

    #[test]
    fn rejects_branch_already_attached_elsewhere() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        let err = ManagedLower::provision_or_open(dir.path(), "main").unwrap_err();
        assert!(matches!(
            err,
            PromotionError::BranchAlreadyAttached { branch, .. } if branch == "main"
        ));
    }

    #[test]
    fn rejects_staged_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "dev");
        let lower = provision_detached_lower(dir.path(), "dev");
        std::fs::write(lower.worktree_path().join("staged.txt"), b"x\n").expect("write");
        Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(lower.worktree_path())
            .output()
            .expect("stage");
        assert_eq!(
            dirty_reason(lower.inspect_clean().unwrap_err()),
            LowerDirtyReason::StagedChanges
        );
    }

    #[test]
    fn rejects_unstaged_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "dev");
        let lower = provision_detached_lower(dir.path(), "dev");
        std::fs::write(lower.worktree_path().join("README.md"), b"dirty\n").expect("write");
        assert_eq!(
            dirty_reason(lower.inspect_clean().unwrap_err()),
            LowerDirtyReason::UnstagedChanges
        );
    }

    #[test]
    fn rejects_untracked_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "dev");
        let lower = provision_detached_lower(dir.path(), "dev");
        std::fs::write(lower.worktree_path().join("new.txt"), b"n\n").expect("write");
        assert_eq!(
            dirty_reason(lower.inspect_clean().unwrap_err()),
            LowerDirtyReason::UntrackedFiles
        );
    }

    #[test]
    fn allows_ignored_untracked_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "dev");
        std::fs::write(dir.path().join(".gitignore"), b"ignored/\n").expect("gitignore");
        Command::new("git")
            .args(["add", ".gitignore"])
            .current_dir(dir.path())
            .output()
            .expect("add gitignore");
        Command::new("git")
            .args(["commit", "-m", "ignore"])
            .current_dir(dir.path())
            .output()
            .expect("commit gitignore");

        let lower = provision_detached_lower(dir.path(), "dev");
        std::fs::create_dir_all(lower.worktree_path().join("ignored")).expect("mkdir");
        std::fs::write(lower.worktree_path().join("ignored/x.txt"), b"x\n").expect("write");
        lower.inspect_clean().expect("ignored files allowed");
    }

    #[test]
    fn rejects_detached_head() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "dev");
        let lower = provision_detached_lower(dir.path(), "dev");
        let oid = git_rev_parse(lower.worktree_path());
        Command::new("git")
            .args(["checkout", "--detach", &oid])
            .current_dir(lower.worktree_path())
            .output()
            .expect("detach");
        assert_eq!(
            dirty_reason(lower.inspect_clean().unwrap_err()),
            LowerDirtyReason::DetachedHead
        );
    }

    #[test]
    fn rejects_wrong_branch() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        Command::new("git")
            .args(["checkout", "-b", "feature"])
            .current_dir(dir.path())
            .output()
            .expect("feature branch");

        let lower = provision_detached_lower(dir.path(), "feature");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(lower.worktree_path())
            .output()
            .expect("checkout main in lower");
        match lower.inspect_clean().unwrap_err() {
            PromotionError::Dirty(LowerDirtyReason::WrongBranch { expected, actual }) => {
                assert_eq!(expected, "refs/heads/feature");
                assert_eq!(actual, "refs/heads/main");
            }
            other => panic!("expected wrong branch, got {other:?}"),
        }
    }

    #[test]
    fn rejects_index_dirty_unmerged() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        Command::new("git")
            .args(["checkout", "-b", "conflict"])
            .current_dir(dir.path())
            .output()
            .expect("conflict branch");
        std::fs::write(dir.path().join("README.md"), b"ours\n").expect("write ours");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("add ours");
        Command::new("git")
            .args(["commit", "-m", "ours"])
            .current_dir(dir.path())
            .output()
            .expect("commit ours");
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(dir.path())
            .output()
            .expect("back main");
        std::fs::write(dir.path().join("README.md"), b"theirs\n").expect("write theirs");
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(dir.path())
            .output()
            .expect("add theirs");
        Command::new("git")
            .args(["commit", "-m", "theirs"])
            .current_dir(dir.path())
            .output()
            .expect("commit theirs");

        let lower = provision_detached_lower(dir.path(), "conflict");
        Command::new("git")
            .args(["merge", "main"])
            .current_dir(lower.worktree_path())
            .output()
            .expect("merge conflict");
        assert_eq!(
            dirty_reason(lower.inspect_clean().unwrap_err()),
            LowerDirtyReason::IndexDirty
        );
    }

    fn detach_primary(repo: &Path) {
        Command::new("git")
            .args(["checkout", "--detach", "HEAD"])
            .current_dir(repo)
            .output()
            .expect("detach primary");
    }

    /// Provision managed Lower on a branch not checked out in the primary worktree.
    fn provision_detached_lower(repo: &Path, branch: &str) -> ManagedLower {
        detach_primary(repo);
        ManagedLower::provision_or_open(repo, branch).expect("provision detached")
    }
}

mod lower_lock {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use lattice_workspace::promotion::{
        DurabilityRecorder, LowerLockGuard, PromotionError, StaleLockDisposition,
        TestIdentityReader, recover_stale_lock,
    };

    #[derive(Default)]
    struct RecordingDurability {
        fsyncs: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl DurabilityRecorder for RecordingDurability {
        fn record_parent_fsync(&self, path: &std::path::Path) {
            self.fsyncs.lock().unwrap().push(path.to_path_buf());
        }
    }

    fn lock_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        TestIdentityReader,
        Arc<RecordingDurability>,
    ) {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join("lower.lock");
        let reader = TestIdentityReader::new("boot-a");
        let durability = Arc::new(RecordingDurability::default());
        (dir, lock_path, reader, durability)
    }

    #[test]
    fn same_identity_is_live_same_pid_different_ticks_is_stale() {
        let (_dir, lock_path, reader, _dur) = lock_fixture();
        reader.register_process(4242, 100);
        std::fs::write(
            &lock_path,
            r#"{"lock_id":"abc","pid":4242,"boot_id":"boot-a","process_start_ticks":100,"acquired_unix_ms":1}"#,
        )
        .expect("write lock");

        let live = LowerLockGuard::inspect_existing(&lock_path, &reader)
            .expect("inspect")
            .expect("some");
        assert_eq!(live, StaleLockDisposition::Live);

        reader.reuse_pid(4242, 200);
        let stale = LowerLockGuard::inspect_existing(&lock_path, &reader)
            .expect("inspect")
            .expect("some");
        assert!(matches!(stale, StaleLockDisposition::Stale { .. }));
    }

    #[test]
    fn acquire_and_drop_records_parent_fsync() {
        let (_dir, lock_path, reader, durability) = lock_fixture();
        reader.register_process(std::process::id(), 50);
        {
            let _guard = LowerLockGuard::acquire(
                &lock_path,
                &reader,
                Some(Arc::clone(&durability) as Arc<dyn DurabilityRecorder>),
            )
            .expect("acquire");
            assert!(lock_path.exists());
            assert!(!durability.fsyncs.lock().unwrap().is_empty());
        }
        assert!(!lock_path.exists());
        assert!(durability.fsyncs.lock().unwrap().len() >= 2);
    }

    #[test]
    fn recovery_callback_failure_leaves_stale_lock() {
        let (_dir, lock_path, reader, durability) = lock_fixture();
        reader.register_process(9001, 10);
        std::fs::create_dir_all(lock_path.parent().unwrap()).expect("parent");
        std::fs::write(
            &lock_path,
            r#"{"lock_id":"stale-1","pid":9001,"boot_id":"boot-a","process_start_ticks":10,"acquired_unix_ms":1}"#,
        )
        .expect("write stale lock");
        reader.kill_process(9001);

        let err = recover_stale_lock(
            &lock_path,
            "stale-1",
            &reader,
            Some(durability.as_ref()),
            || {
                Err(PromotionError::GitCommand {
                    context: "journal replay".into(),
                    stderr: "boom".into(),
                })
            },
        )
        .unwrap_err();
        assert!(matches!(err, PromotionError::GitCommand { .. }));
        assert!(lock_path.exists());
    }

    #[test]
    fn successful_recovery_removes_stale_lock() {
        let (_dir, lock_path, reader, durability) = lock_fixture();
        reader.register_process(9002, 20);
        std::fs::create_dir_all(lock_path.parent().unwrap()).expect("parent");
        std::fs::write(
            &lock_path,
            r#"{"lock_id":"stale-2","pid":9002,"boot_id":"boot-a","process_start_ticks":20,"acquired_unix_ms":1}"#,
        )
        .expect("write stale lock");
        reader.kill_process(9002);

        recover_stale_lock(
            &lock_path,
            "stale-2",
            &reader,
            Some(durability.as_ref()),
            || Ok(()),
        )
        .expect("recover");
        assert!(!lock_path.exists());
    }

    #[test]
    fn malformed_lock_json_is_not_auto_deleted() {
        let (_dir, lock_path, reader, _dur) = lock_fixture();
        std::fs::create_dir_all(lock_path.parent().unwrap()).expect("parent");
        std::fs::write(&lock_path, "{not-json").expect("write malformed");
        let disposition = LowerLockGuard::inspect_existing(&lock_path, &reader)
            .expect("inspect")
            .expect("some");
        assert_eq!(disposition, StaleLockDisposition::Malformed);
        assert!(lock_path.exists());
    }
}
