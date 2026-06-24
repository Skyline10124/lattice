// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Exact-baseline identity contract coverage for both workspace adapters.

#![cfg(feature = "test-utils")]

use std::path::PathBuf;

use lattice_workspace::{
    EgressPolicy, ImageOrTemplate, UpperSpec, Workspace, WorkspaceError,
    testing::DeterministicWorkspace,
};

/// Host repo with a two-commit chain so the baseline commit has a parent.
fn make_multi_parent_fixture() -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = git2::Repository::init(dir.path()).expect("git init");
    let sig = git2::Signature::now("promo-test", "pt@localhost").expect("sig");

    std::fs::write(dir.path().join("root.txt"), b"root\n").expect("write root");
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .expect("add_all");
    index.write().expect("index write");
    let root_tree = repo
        .find_tree(index.write_tree().expect("write_tree"))
        .expect("tree");
    let root_oid = repo
        .commit(Some("HEAD"), &sig, &sig, "root", &root_tree, &[])
        .expect("root commit");

    std::fs::write(dir.path().join("child.txt"), b"child\n").expect("write child");
    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .expect("add_all");
    index.write().expect("index write");
    let child_tree = repo
        .find_tree(index.write_tree().expect("write_tree"))
        .expect("tree");
    let baseline_oid = repo
        .commit(
            Some("HEAD"),
            &sig,
            &sig,
            "baseline",
            &child_tree,
            &[&repo.find_commit(root_oid).expect("parent")],
        )
        .expect("baseline commit");

    (dir, baseline_oid.to_string())
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

#[tokio::test]
async fn deterministic_exact_baseline() {
    let (repo_dir, baseline_oid) = make_multi_parent_fixture();
    let ws = DeterministicWorkspace::new().expect("DeterministicWorkspace::new");
    let spec = make_spec(repo_dir.path().to_owned(), baseline_oid.clone());
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("create_upper must provision exact baseline");

    let head = ws
        .exec_upper(&handle, "git", &["rev-parse", "HEAD"])
        .await
        .expect("rev-parse HEAD");
    assert_eq!(head.exit_code, 0);
    assert_eq!(
        head.stdout.trim(),
        baseline_oid,
        "Upper HEAD must equal host baseline OID"
    );

    let parents = ws
        .exec_upper(&handle, "git", &["cat-file", "-p", "HEAD"])
        .await
        .expect("cat-file HEAD");
    assert_eq!(parents.exit_code, 0);
    assert!(
        parents
            .stdout
            .lines()
            .any(|line| line.starts_with("parent ")),
        "baseline commit must retain parent lines; got:\n{}",
        parents.stdout
    );

    ws.destroy_upper(&handle).await.unwrap();
}

#[tokio::test]
async fn deterministic_invalid_baseline_oid_rejected_before_handle() {
    let (repo_dir, baseline_oid) = make_multi_parent_fixture();
    let ws = DeterministicWorkspace::new().expect("DeterministicWorkspace::new");
    let mut spec = make_spec(repo_dir.path().to_owned(), baseline_oid);
    spec.baseline_oid = "not-a-valid-oid".into();

    let err = ws
        .create_upper(&spec)
        .await
        .expect_err("invalid OID must fail before handle registration");
    assert!(
        matches!(err, WorkspaceError::InvalidOid(_)),
        "expected InvalidOid, got {err:?}"
    );
}

#[tokio::test]
async fn deterministic_missing_baseline_oid_rejected_before_handle() {
    let (repo_dir, _) = make_multi_parent_fixture();
    let ws = DeterministicWorkspace::new().expect("DeterministicWorkspace::new");
    let spec = make_spec(
        repo_dir.path().to_owned(),
        "0000000000000000000000000000000000000000".into(),
    );

    let err = ws
        .create_upper(&spec)
        .await
        .expect_err("missing baseline object must fail before handle registration");
    assert!(
        matches!(err, WorkspaceError::Git(_)),
        "expected Git error for missing object, got {err:?}"
    );
}

#[cfg(feature = "integration_tests")]
mod microsandbox {
    use super::*;
    use lattice_workspace::{Backend, microsandbox::MicrosandboxWorkspace};

    #[tokio::test]
    #[ignore = "requires KVM and live microsandbox host"]
    async fn microsandbox_exact_baseline() {
        let (repo_dir, baseline_oid) = make_multi_parent_fixture();
        let ws = MicrosandboxWorkspace::from_env();
        let spec = UpperSpec {
            image_or_template: ImageOrTemplate::Template("lattice-git-ready-base".into()),
            baseline_oid: baseline_oid.clone(),
            host_repo_path: repo_dir.path().to_owned(),
            egress: EgressPolicy::Blocked,
            credentials: vec![],
            timeout_secs: 120,
            cpus: 1,
            memory_mb: 512,
        };

        let handle = ws
            .create_upper(&spec)
            .await
            .expect("microsandbox create_upper");
        assert_eq!(handle.backend, Backend::Microsandbox);

        let head = ws
            .exec_upper(&handle, "git", &["-C", "/workspace", "rev-parse", "HEAD"])
            .await
            .expect("rev-parse HEAD");
        assert_eq!(head.exit_code, 0);
        assert_eq!(head.stdout.trim(), baseline_oid);

        ws.destroy_upper(&handle).await.unwrap();
    }
}
