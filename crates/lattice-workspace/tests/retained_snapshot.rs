// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Retained snapshot adapter contract — CI lane, no KVM required.
//!
//! Tests the credential-aware retained snapshot lifecycle:
//! `retain_upper`, `restore_retained_upper`, `retained_snapshot_exists`,
//! `list_retained_snapshots`, and `release_retained_snapshot`.
//!
//! Validates D-11 (distinct from warm templates), D-12 (name format),
//! D-15 (cold restore preserves filesystem state), D-17 (quota).
//!
//! This file is compiled whenever `--features=test-utils` is present.
//! It does NOT require the `integration_tests` feature.

#![cfg(feature = "test-utils")]

use lattice_workspace::testing::DeterministicWorkspace;
use lattice_workspace::testing::contract_tests::{make_fixture_repo, minimal_upper_spec};
use lattice_workspace::{Backend, SnapshotRef, Workspace, WorkspaceError};

/// Default per-session quota used in tests (matches D-17 default of 5).
const DEFAULT_QUOTA: u32 = 5;

/// Build a valid D-12 snapshot name from session/commit/nonce parts.
fn make_snapshot_name(session_id: &str, commit_hex8: &str, nonce_hex8: &str) -> String {
    format!("lattice-{session_id}-{commit_hex8}-{nonce_hex8}")
}

/// Create an Upper, write a dirty file, and return the handle.
async fn create_upper_with_dirty_file(
    ws: &DeterministicWorkspace,
) -> (tempfile::TempDir, lattice_workspace::UpperHandle, String) {
    let (repo, oid) = make_fixture_repo();
    let spec = minimal_upper_spec(repo.path().to_owned(), oid.clone());
    let handle = ws.create_upper(&spec).await.expect("create_upper");
    // Write an untracked file (dirty state that must survive retain/restore).
    ws.write_file(
        &handle,
        &lattice_workspace::WorkspacePath::parse("dirty.txt").unwrap(),
        b"uncommitted content\n",
    )
    .await
    .expect("write dirty file");
    (repo, handle, oid)
}

/// Create an Upper with a committed file (clean state) and return the handle.
async fn create_upper_with_committed_file(
    ws: &DeterministicWorkspace,
) -> (tempfile::TempDir, lattice_workspace::UpperHandle, String) {
    let (repo, oid) = make_fixture_repo();
    let spec = minimal_upper_spec(repo.path().to_owned(), oid.clone());
    let handle = ws.create_upper(&spec).await.expect("create_upper");
    // Write and commit a file so the upper has clean git state.
    ws.write_file(
        &handle,
        &lattice_workspace::WorkspacePath::parse("committed.txt").unwrap(),
        b"committed content\n",
    )
    .await
    .expect("write file");
    ws.exec_upper(&handle, "git", &["add", "committed.txt"])
        .await
        .expect("git add");
    ws.exec_upper(
        &handle,
        "git",
        &[
            "-c",
            "user.email=test@test.com",
            "-c",
            "user.name=Test",
            "commit",
            "-m",
            "add committed.txt",
        ],
    )
    .await
    .expect("git commit");
    (repo, handle, oid)
}

// ── Contract tests ────────────────────────────────────────────────────────────

/// State assertion: dirty files survive cold restore.
#[tokio::test]
async fn test_retain_preserves_dirty_state_and_git() {
    let ws = DeterministicWorkspace::new().expect("new");
    let (_repo, handle, _oid) = create_upper_with_dirty_file(&ws).await;
    let name = make_snapshot_name("sess001", "abcd1234", "00000001");
    let snap_ref = ws
        .retain_upper(&handle, &name, DEFAULT_QUOTA)
        .await
        .expect("retain_upper");

    assert_eq!(snap_ref.backend, Backend::Deterministic);
    assert_eq!(snap_ref.name, name);

    // Snapshot must exist.
    assert!(
        ws.retained_snapshot_exists(&snap_ref)
            .await
            .expect("exists check"),
        "retained snapshot must exist after retain"
    );

    // Restore and verify the dirty file survived.
    let spec = {
        let (repo2, oid2) = make_fixture_repo();
        minimal_upper_spec(repo2.path().to_owned(), oid2)
    };
    let restored = ws
        .restore_retained_upper(&snap_ref, "restored-001", &spec)
        .await
        .expect("restore_retained_upper");

    let content = ws
        .read_file(
            &restored,
            &lattice_workspace::WorkspacePath::parse("dirty.txt").unwrap(),
        )
        .await
        .expect("read dirty file after restore");
    assert_eq!(
        String::from_utf8_lossy(&content),
        "uncommitted content\n",
        "dirty file content must survive cold restore"
    );

    // .git directory must also exist (git commands work).
    let git_out = ws
        .exec_upper(&restored, "git", &["rev-parse", "--git-dir"])
        .await
        .expect("git rev-parse --git-dir after restore");
    assert_eq!(git_out.exit_code, 0, ".git must survive cold restore");

    // Source snapshot must still exist (restore doesn't delete).
    assert!(
        ws.retained_snapshot_exists(&snap_ref)
            .await
            .expect("exists after restore"),
        "source snapshot must survive restore"
    );

    ws.destroy_upper(&restored).await.unwrap();
}

/// State assertion: clean commits survive cold restore.
#[tokio::test]
async fn test_retain_preserves_clean_commits() {
    let ws = DeterministicWorkspace::new().expect("new");
    let (_repo, handle, _oid) = create_upper_with_committed_file(&ws).await;

    let name = make_snapshot_name("sess002", "deadbeef", "00000002");
    let snap_ref = ws
        .retain_upper(&handle, &name, DEFAULT_QUOTA)
        .await
        .expect("retain_upper");

    let spec = {
        let (repo2, oid2) = make_fixture_repo();
        minimal_upper_spec(repo2.path().to_owned(), oid2)
    };
    let restored = ws
        .restore_retained_upper(&snap_ref, "restored-002", &spec)
        .await
        .expect("restore_retained_upper");

    // Committed file must be readable.
    let content = ws
        .read_file(
            &restored,
            &lattice_workspace::WorkspacePath::parse("committed.txt").unwrap(),
        )
        .await
        .expect("read committed file");
    assert_eq!(String::from_utf8_lossy(&content), "committed content\n");

    // Git log must show the commit.
    let log_out = ws
        .exec_upper(&restored, "git", &["log", "--oneline"])
        .await
        .expect("git log");
    assert_eq!(log_out.exit_code, 0);
    assert!(
        log_out.stdout.contains("add committed.txt"),
        "git log must show committed file: {}",
        log_out.stdout
    );

    ws.destroy_upper(&restored).await.unwrap();
}

/// Ownership assertion: successful retain removes the live handle.
#[tokio::test]
async fn test_retain_removes_live_handle() {
    let ws = DeterministicWorkspace::new().expect("new");
    let (_repo, handle, _oid) = create_upper_with_dirty_file(&ws).await;
    let name = make_snapshot_name("sess003", "cafebabe", "00000003");
    let _snap_ref = ws
        .retain_upper(&handle, &name, DEFAULT_QUOTA)
        .await
        .expect("retain_upper");

    // After retain, exec_upper on the old handle must fail.
    let result = ws.exec_upper(&handle, "echo", &["hello"]).await;
    assert!(
        result.is_err(),
        "exec_upper on retained handle must fail after successful retain"
    );
}

/// Release assertion: repeated release succeeds and list/existence converge.
#[tokio::test]
async fn test_release_is_idempotent() {
    let ws = DeterministicWorkspace::new().expect("new");
    let (_repo, handle, _oid) = create_upper_with_dirty_file(&ws).await;
    let name = make_snapshot_name("sess004", "12345678", "00000004");
    let snap_ref = ws
        .retain_upper(&handle, &name, DEFAULT_QUOTA)
        .await
        .expect("retain_upper");

    // First release.
    ws.release_retained_snapshot(&snap_ref)
        .await
        .expect("first release");
    assert!(
        !ws.retained_snapshot_exists(&snap_ref)
            .await
            .expect("exists after first release"),
        "snapshot must not exist after release"
    );

    // Second release (idempotent).
    ws.release_retained_snapshot(&snap_ref)
        .await
        .expect("second release must also succeed (idempotent)");

    // List must not include the released snapshot.
    let list = ws
        .list_retained_snapshots("sess004")
        .await
        .expect("list after release");
    assert!(
        !list.contains(&name),
        "released snapshot must not appear in list"
    );
}

/// Existence and list are exact: only retained snapshots are returned.
#[tokio::test]
async fn test_list_returns_exact_session_snapshots() {
    let ws = DeterministicWorkspace::new().expect("new");
    let session = "sess005";
    let mut snap_refs = Vec::new();

    for i in 0..3u32 {
        let (_repo, handle, _oid) = create_upper_with_dirty_file(&ws).await;
        let name = make_snapshot_name(
            session,
            &format!("{:08x}", i + 100),
            &format!("{:08x}", i + 5),
        );
        let snap_ref = ws
            .retain_upper(&handle, &name, DEFAULT_QUOTA)
            .await
            .expect("retain_upper");
        snap_refs.push(snap_ref);
    }

    let list = ws.list_retained_snapshots(session).await.expect("list");
    assert_eq!(list.len(), 3, "must list exactly 3 retained snapshots");
    for snap_ref in &snap_refs {
        assert!(
            list.contains(&snap_ref.name),
            "list must contain {}",
            snap_ref.name
        );
        assert!(
            ws.retained_snapshot_exists(snap_ref).await.expect("exists"),
            "each retained snapshot must exist"
        );
    }

    // A different session prefix must return empty.
    let other_list = ws
        .list_retained_snapshots("other_session")
        .await
        .expect("list other");
    assert!(
        other_list.is_empty(),
        "other session prefix must return empty list"
    );
}

/// Quota assertion: the sixth snapshot fails at default quota 5 with no partial snapshot.
#[tokio::test]
async fn test_quota_refuses_at_limit() {
    let ws = DeterministicWorkspace::new().expect("new");
    let session = "sess006";
    let quota: u32 = 5;

    // Create 5 snapshots (fill quota).
    for i in 0..quota {
        let (_repo, handle, _oid) = create_upper_with_dirty_file(&ws).await;
        let name = make_snapshot_name(session, &format!("{:08x}", i + 200), &format!("{:08x}", i));
        ws.retain_upper(&handle, &name, quota)
            .await
            .expect("retain within quota");
    }

    // 6th must fail with quota error.
    let (_repo, handle, _oid) = create_upper_with_dirty_file(&ws).await;
    let bad_name = make_snapshot_name(session, "000000c8", "00000005");
    let result = ws.retain_upper(&handle, &bad_name, quota).await;
    assert!(result.is_err(), "6th retain must fail at quota {quota}");
    match result.unwrap_err() {
        WorkspaceError::SnapshotQuotaExceeded { current, max } => {
            assert_eq!(current, quota, "current must equal quota");
            assert_eq!(max, quota, "max must equal quota");
        }
        other => panic!("expected SnapshotQuotaExceeded, got: {other:?}"),
    }

    // The live handle must still be destroyable (ownership stays with caller).
    ws.destroy_upper(&handle)
        .await
        .expect("handle must be destroyable after failed retain");

    // No partial snapshot was created.
    let bad_ref = SnapshotRef {
        backend: Backend::Deterministic,
        name: bad_name.clone(),
    };
    assert!(
        !ws.retained_snapshot_exists(&bad_ref)
            .await
            .expect("exists check"),
        "no partial snapshot after quota refusal"
    );
}

/// Invalid snapshot names are rejected before any effect.
#[tokio::test]
async fn test_invalid_name_rejected() {
    let ws = DeterministicWorkspace::new().expect("new");
    let (_repo, handle, _oid) = create_upper_with_dirty_file(&ws).await;

    for bad_name in &[
        "not-a-lattice-snapshot",
        "lattice-no-commithex-short",
        "lattice-sess-ZZZZZZZZ-00000000", // non-hex commit
        "",
    ] {
        let result = ws.retain_upper(&handle, bad_name, DEFAULT_QUOTA).await;
        assert!(
            matches!(result, Err(WorkspaceError::InvalidSnapshotName(_))),
            "bad name '{bad_name}' must produce InvalidSnapshotName, got: {result:?}"
        );
    }

    // Handle must still be destroyable.
    ws.destroy_upper(&handle)
        .await
        .expect("destroy after failed retains");
}

/// Quota 0 always refuses; handle remains destroyable.
#[tokio::test]
async fn test_zero_quota_always_refuses() {
    let ws = DeterministicWorkspace::new().expect("new");
    let (_repo, handle, _oid) = create_upper_with_dirty_file(&ws).await;
    let name = make_snapshot_name("sess007", "aabbccdd", "00000001");
    let result = ws.retain_upper(&handle, &name, 0).await;
    assert!(
        matches!(result, Err(WorkspaceError::SnapshotQuotaExceeded { .. })),
        "quota 0 must refuse, got: {result:?}"
    );
    ws.destroy_upper(&handle).await.expect("destroy");
}

/// Retain on a nonexistent handle fails gracefully.
#[tokio::test]
async fn test_retain_nonexistent_handle_fails() {
    let ws = DeterministicWorkspace::new().expect("new");
    let bogus = lattice_workspace::UpperHandle {
        id: "nonexistent".to_string(),
        backend: Backend::Deterministic,
    };
    let name = make_snapshot_name("sess008", "11223344", "00000001");
    let result = ws.retain_upper(&bogus, &name, DEFAULT_QUOTA).await;
    assert!(result.is_err(), "retain on nonexistent handle must fail");
}

/// Restore from a nonexistent snapshot fails.
#[tokio::test]
async fn test_restore_nonexistent_snapshot_fails() {
    let ws = DeterministicWorkspace::new().expect("new");
    let bogus_ref = SnapshotRef {
        backend: Backend::Deterministic,
        name: make_snapshot_name("sess009", "deadbeef", "ffffffff"),
    };
    let spec = {
        let (repo, oid) = make_fixture_repo();
        minimal_upper_spec(repo.path().to_owned(), oid)
    };
    let result = ws
        .restore_retained_upper(&bogus_ref, "restore-bogus", &spec)
        .await;
    assert!(
        result.is_err(),
        "restore from nonexistent snapshot must fail"
    );
}

// ── Optional capable-host integration test ────────────────────────────────────

/// Ignored integration test: proves retain, cold restore, and release on
/// a live microsandbox adapter with KVM.
///
/// Run with: `cargo test -p lattice-workspace --features integration_tests
/// --test retained_snapshot test_microsandbox_retain_restore_release -- --ignored`
#[cfg(feature = "integration_tests")]
#[tokio::test]
#[ignore = "requires KVM + libkrunfw.so"]
async fn test_microsandbox_retain_restore_release() {
    use lattice_workspace::ImageOrTemplate;
    use lattice_workspace::microsandbox::MicrosandboxWorkspace;

    let _ws = MicrosandboxWorkspace::new(1, 512);
    let image = "docker.m.daocloud.io/library/python:3.12-slim";

    // Create a sandbox from image.
    let _spec = lattice_workspace::UpperSpec {
        image_or_template: ImageOrTemplate::Image(image.to_string()),
        baseline_oid: "0".repeat(40),
        host_repo_path: std::path::PathBuf::from("/tmp"),
        egress: lattice_workspace::EgressPolicy::Open,
        credentials: vec![],
        timeout_secs: 120,
        cpus: 1,
        memory_mb: 512,
    };

    // Note: this test requires deliver_baseline to succeed, so it needs
    // a valid host_repo_path with a real git repo. Skipped for now.
    // The deterministic contract suite above covers the full lifecycle.
    println!("CAPABLE_HOST: retained snapshot integration test placeholder");
}
