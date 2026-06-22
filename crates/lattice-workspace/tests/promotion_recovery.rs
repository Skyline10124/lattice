// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! V-06 materialization and journal recovery crash matrix.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use git2::Repository;
use lattice_workspace::promotion::{
    DurableFs, DurableFsRecorder, FaultPoint, JournalHeader, MaterializationPlan,
    PathClassification, PromotionError, PromotionJournal, PromotionLimits, RecoveryDirection,
    RecoveryOutcome, build_materialization_plan, classify_path, parse_journal, recover_journal,
    resolve_recovery_direction, run_materialization,
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
    git_rev_parse(dir)
}

fn git_config(dir: &Path) {
    Command::new("git")
        .args(["config", "user.email", "test@localhost"])
        .current_dir(dir)
        .output()
        .expect("email");
    Command::new("git")
        .args(["config", "user.name", "test"])
        .current_dir(dir)
        .output()
        .expect("name");
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
            .expect("common")
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

fn promotion_state_dir(repo: &Path, branch: &str) -> PathBuf {
    let hex = branch
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    git_common_dir(repo).join("lattice/promotion").join(hex)
}

struct RecordingRecorder {
    inner: lattice_workspace::promotion::VecDurableRecorder,
}

impl RecordingRecorder {
    fn new() -> Self {
        Self {
            inner: lattice_workspace::promotion::VecDurableRecorder::new(),
        }
    }
}

impl DurableFsRecorder for RecordingRecorder {
    fn record_file_sync(&self, path: &Path) {
        self.inner.record_file_sync(path);
    }

    fn record_parent_sync(&self, path: &Path) {
        self.inner.record_parent_sync(path);
    }
}

mod materialization {
    use super::*;

    #[test]
    fn add_modify_delete_and_executable_transitions_converge() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        std::fs::write(dir.path().join("plain.txt"), b"v1\n").expect("write");
        std::fs::write(dir.path().join("tool.sh"), b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(dir.path().join("tool.sh"))
                .unwrap()
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(dir.path().join("tool.sh"), perms).unwrap();
        }
        std::fs::create_dir(dir.path().join("keep")).expect("mkdir");
        std::fs::write(dir.path().join("keep/file.txt"), b"stay\n").expect("write");
        git_commit_all(dir.path(), "baseline content");
        let baseline = git_rev_parse(dir.path());

        std::fs::write(dir.path().join("plain.txt"), b"v2\n").expect("write");
        std::fs::write(dir.path().join("new.txt"), b"added\n").expect("write");
        std::fs::remove_file(dir.path().join("tool.sh")).expect("remove");
        git_commit_all(dir.path(), "candidate content");
        let candidate = git_rev_parse(dir.path());

        let plan = build_materialization_plan(
            dir.path(),
            &baseline,
            &candidate,
            &PromotionLimits::default(),
        )
        .expect("plan");
        assert!(!plan.is_empty());

        let worktree = dir.path().join("lower");
        std::fs::create_dir_all(&worktree).expect("mkdir lower");
        checkout_tree(dir.path(), &baseline, &worktree);

        let repo = Repository::open(dir.path()).expect("repo");
        let mut fs = DurableFs::new(&worktree, &repo, None);
        for entry in plan.entries() {
            fs.apply_entry(entry).expect("apply");
        }

        assert_eq!(std::fs::read(worktree.join("plain.txt")).unwrap(), b"v2\n");
        assert_eq!(std::fs::read(worktree.join("new.txt")).unwrap(), b"added\n");
        assert!(!worktree.join("tool.sh").exists());
        assert_eq!(
            std::fs::read(worktree.join("keep/file.txt")).unwrap(),
            b"stay\n"
        );
    }

    #[test]
    fn non_empty_directory_blocks_removal() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        std::fs::create_dir(dir.path().join("dir")).expect("mkdir");
        std::fs::write(dir.path().join("dir/file.txt"), b"x\n").expect("write");
        git_commit_all(dir.path(), "baseline");
        let baseline = git_rev_parse(dir.path());

        std::fs::remove_file(dir.path().join("dir/file.txt")).expect("remove");
        git_commit_all(dir.path(), "candidate");
        let candidate = git_rev_parse(dir.path());

        let plan = build_materialization_plan(
            dir.path(),
            &baseline,
            &candidate,
            &PromotionLimits::default(),
        )
        .expect("plan");
        let worktree = dir.path().join("lower");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        checkout_tree(dir.path(), &baseline, &worktree);
        std::fs::write(worktree.join("dir/unrelated.txt"), b"user\n").expect("user file");

        let repo = Repository::open(dir.path()).expect("repo");
        let mut fs = DurableFs::new(&worktree, &repo, None);
        let err = fs
            .apply_entry(plan.entries().first().expect("entry"))
            .unwrap_err();
        assert!(matches!(err, PromotionError::MaterializeConflict { .. }));
        assert!(worktree.join("dir/unrelated.txt").exists());
    }

    #[test]
    fn third_state_returns_conflict_without_mutation() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        std::fs::write(dir.path().join("file.txt"), b"old\n").expect("write");
        git_commit_all(dir.path(), "baseline");
        let baseline = git_rev_parse(dir.path());

        std::fs::write(dir.path().join("file.txt"), b"new\n").expect("write");
        git_commit_all(dir.path(), "candidate");
        let candidate = git_rev_parse(dir.path());

        let plan = build_materialization_plan(
            dir.path(),
            &baseline,
            &candidate,
            &PromotionLimits::default(),
        )
        .expect("plan");
        let worktree = dir.path().join("lower");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        checkout_tree(dir.path(), &baseline, &worktree);
        std::fs::write(worktree.join("file.txt"), b"third\n").expect("third");

        let repo = Repository::open(dir.path()).expect("repo");
        let entry = plan.entries().first().expect("entry");
        let class =
            classify_path(&worktree, &repo, &entry.path, &entry.old, &entry.new).expect("class");
        assert_eq!(class, PathClassification::ThirdState);
        let mut fs = DurableFs::new(&worktree, &repo, None);
        assert!(matches!(
            fs.apply_entry(entry).unwrap_err(),
            PromotionError::MaterializeConflict { .. }
        ));
        assert_eq!(
            std::fs::read(worktree.join("file.txt")).unwrap(),
            b"third\n"
        );
    }

    #[test]
    fn durable_fs_syncs_file_before_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        std::fs::write(dir.path().join("a.txt"), b"one\n").expect("write");
        git_commit_all(dir.path(), "baseline");
        let baseline = git_rev_parse(dir.path());

        std::fs::write(dir.path().join("b.txt"), b"two\n").expect("write");
        git_commit_all(dir.path(), "candidate");
        let candidate = git_rev_parse(dir.path());

        let plan = build_materialization_plan(
            dir.path(),
            &baseline,
            &candidate,
            &PromotionLimits::default(),
        )
        .expect("plan");
        let worktree = dir.path().join("lower");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        checkout_tree(dir.path(), &baseline, &worktree);

        let repo = Repository::open(dir.path()).expect("repo");
        let recorder = Arc::new(RecordingRecorder::new());
        let mut fs = DurableFs::new(&worktree, &repo, Some(recorder.as_ref()));
        for entry in plan.entries() {
            fs.apply_entry(entry).expect("apply");
        }

        let file_syncs = recorder.inner.file_syncs.lock().unwrap();
        let parent_syncs = recorder.inner.parent_syncs.lock().unwrap();
        assert!(!file_syncs.is_empty());
        assert!(!parent_syncs.is_empty());
        for file in file_syncs.iter() {
            if let Some(parent) = file.parent() {
                assert!(
                    parent_syncs.iter().any(|p| p == parent),
                    "missing parent sync for {}",
                    file.display()
                );
            }
        }
    }
}

mod journal_recovery {
    use super::*;

    fn fixture_repo() -> (tempfile::TempDir, String, String, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path(), "main");
        std::fs::write(dir.path().join("tracked.txt"), b"base\n").expect("write");
        git_commit_all(dir.path(), "baseline");
        let baseline = git_rev_parse(dir.path());

        std::fs::write(dir.path().join("tracked.txt"), b"cand\n").expect("write");
        std::fs::write(dir.path().join("added.txt"), b"added\n").expect("write");
        git_commit_all(dir.path(), "candidate");
        let candidate = git_rev_parse(dir.path());

        let worktree = dir.path().join("lower");
        std::fs::create_dir_all(&worktree).expect("mkdir");
        checkout_tree(dir.path(), &baseline, &worktree);
        let state_dir = promotion_state_dir(dir.path(), "main");
        (dir, baseline, candidate, worktree, state_dir)
    }

    fn header(baseline: &str, candidate: &str, plan: MaterializationPlan) -> JournalHeader {
        JournalHeader {
            journal_id: uuid::Uuid::new_v4().to_string(),
            project_ref: "refs/heads/main".into(),
            baseline_oid: baseline.to_owned(),
            candidate_oid: candidate.to_owned(),
            path_plan: plan,
        }
    }

    #[test]
    fn crash_matrix_recovers_toward_candidate_ref() {
        let (dir, baseline, candidate, worktree, state_dir) = fixture_repo();
        let plan = build_materialization_plan(
            dir.path(),
            &baseline,
            &candidate,
            &PromotionLimits::default(),
        )
        .expect("plan");
        let hdr = header(&baseline, &candidate, plan);

        for fault in crash_points(plan_len(&hdr)) {
            std::fs::remove_dir_all(&state_dir).ok();
            std::fs::create_dir_all(&state_dir).expect("state");
            checkout_tree(dir.path(), &baseline, &worktree);

            let result = run_materialization(
                dir.path(),
                &worktree,
                &state_dir,
                hdr.clone(),
                &candidate,
                "session-oid",
                &candidate,
                Some(fault),
            );
            assert!(matches!(
                result.unwrap_err(),
                PromotionError::SimulatedCrash
            ));
            assert!(PromotionJournal::exists(&state_dir));

            let outcome =
                recover_journal(dir.path(), &worktree, &state_dir, &candidate).expect("recover");
            assert_eq!(outcome, RecoveryOutcome::Complete);
            assert_eq!(
                std::fs::read(worktree.join("tracked.txt")).unwrap(),
                b"cand\n"
            );
            assert_eq!(
                std::fs::read(worktree.join("added.txt")).unwrap(),
                b"added\n"
            );
            assert!(!PromotionJournal::exists(&state_dir));
        }
    }

    #[test]
    fn recovery_after_baseline_ref_preserves_baseline() {
        let (dir, baseline, candidate, worktree, state_dir) = fixture_repo();
        let plan = build_materialization_plan(
            dir.path(),
            &baseline,
            &candidate,
            &PromotionLimits::default(),
        )
        .expect("plan");
        let hdr = header(&baseline, &candidate, plan);
        std::fs::create_dir_all(&state_dir).expect("state");

        run_materialization(
            dir.path(),
            &worktree,
            &state_dir,
            hdr,
            &candidate,
            "session-oid",
            &candidate,
            Some(FaultPoint::AfterEffect { index: 0 }),
        )
        .expect_err("crash");

        let outcome =
            recover_journal(dir.path(), &worktree, &state_dir, &baseline).expect("recover");
        assert_eq!(outcome, RecoveryOutcome::Complete);
        assert_eq!(
            std::fs::read(worktree.join("tracked.txt")).unwrap(),
            b"base\n"
        );
        assert!(!worktree.join("added.txt").exists());
    }

    #[test]
    fn third_ref_and_third_state_preserve_journal() {
        let (dir, baseline, candidate, worktree, state_dir) = fixture_repo();
        let plan = build_materialization_plan(
            dir.path(),
            &baseline,
            &candidate,
            &PromotionLimits::default(),
        )
        .expect("plan");
        let hdr = header(&baseline, &candidate, plan);
        std::fs::create_dir_all(&state_dir).expect("state");
        PromotionJournal::create_new(&state_dir, hdr).expect("journal");

        assert_eq!(
            resolve_recovery_direction(
                "deadbeef",
                &parse_journal(&PromotionJournal::path_in(&state_dir))
                    .unwrap()
                    .header
            ),
            RecoveryDirection::RefConflict
        );
        let outcome =
            recover_journal(dir.path(), &worktree, &state_dir, "deadbeef").expect("recover");
        assert!(matches!(outcome, RecoveryOutcome::RecoveryConflict { .. }));
        assert!(PromotionJournal::exists(&state_dir));

        std::fs::write(worktree.join("tracked.txt"), b"user\n").expect("third");
        let outcome =
            recover_journal(dir.path(), &worktree, &state_dir, &candidate).expect("recover");
        assert!(matches!(outcome, RecoveryOutcome::RecoveryConflict { .. }));
        assert_eq!(
            std::fs::read(worktree.join("tracked.txt")).unwrap(),
            b"user\n"
        );
    }

    #[test]
    fn torn_final_line_is_tolerated_malformed_middle_is_fatal() {
        let (dir, baseline, candidate, _worktree, state_dir) = fixture_repo();
        let plan = build_materialization_plan(
            dir.path(),
            &baseline,
            &candidate,
            &PromotionLimits::default(),
        )
        .expect("plan");
        let hdr = header(&baseline, &candidate, plan);
        std::fs::create_dir_all(&state_dir).expect("state");
        let path = PromotionJournal::path_in(&state_dir);
        std::fs::write(
            &path,
            format!(
                "{}\n{{\"kind\":\"finished\"",
                serde_json::to_string(&lattice_workspace::promotion::JournalRecord::Header(
                    hdr.clone()
                ))
                .unwrap()
            ),
        )
        .expect("write torn");
        let parsed = parse_journal(&path).expect("torn tolerated");
        assert!(parsed.torn_final_line);

        std::fs::write(
            &path,
            format!(
                "{}\n{{not-json}}\n{}",
                serde_json::to_string(&lattice_workspace::promotion::JournalRecord::Header(hdr))
                    .unwrap(),
                serde_json::to_string(&lattice_workspace::promotion::JournalRecord::Finished)
                    .unwrap()
            ),
        )
        .expect("write bad");
        assert!(matches!(
            parse_journal(&path).unwrap_err(),
            PromotionError::JournalMalformed { .. }
        ));
    }

    fn crash_points(plan_len: usize) -> Vec<FaultPoint> {
        let mut points = vec![
            FaultPoint::AfterHeaderSync,
            FaultPoint::AfterRefsCommitted,
            FaultPoint::AfterFinished,
            FaultPoint::BeforeJournalRemove,
        ];
        for index in 0..plan_len {
            points.push(FaultPoint::AfterIntent { index });
            points.push(FaultPoint::AfterEffect { index });
            points.push(FaultPoint::AfterComplete { index });
        }
        points
    }

    fn plan_len(hdr: &JournalHeader) -> usize {
        hdr.path_plan.len()
    }
}

fn checkout_tree(repo: &Path, oid: &str, dest: &Path) {
    lattice_workspace::git::checkout_tree_to_dir(repo, oid, dest).expect("checkout");
}
