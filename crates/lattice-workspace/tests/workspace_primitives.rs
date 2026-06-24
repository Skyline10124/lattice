// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Adapter-neutral primitive contract for [`DeterministicWorkspace`].

#![cfg(feature = "test-utils")]

use std::collections::BTreeMap;

use lattice_workspace::testing::DeterministicWorkspace;
use lattice_workspace::testing::contract_tests;
use lattice_workspace::{
    ExecRequest, FindRequest, GrepRequest, MAX_WORKSPACE_WRITE_BYTES, PathIntent, Workspace,
    WorkspaceError, WorkspacePath,
};

fn assert_no_root_leak(value: &str, host_repo: &str) {
    assert!(
        !value.contains(host_repo),
        "host repo path leaked: {value:?}"
    );
    assert!(
        !value.contains("/workspace"),
        "/workspace leaked: {value:?}"
    );
}

#[tokio::test]
async fn deterministic_primitives_contract() {
    let (repo, oid) = contract_tests::make_fixture_repo();
    let host_repo = repo.path().to_string_lossy().to_string();
    let ws = DeterministicWorkspace::new().expect("workspace");
    let spec = contract_tests::minimal_upper_spec(repo.path().to_owned(), oid);
    let handle = ws.create_upper(&spec).await.expect("create_upper");

    let root = WorkspacePath::project_root();
    let readme = WorkspacePath::parse("README.md").expect("path");

    let facts = ws
        .resolve_path(&handle, &root, PathIntent::ListDirectory)
        .await
        .expect("resolve root");
    assert_no_root_leak(&format!("{facts:?}"), &host_repo);
    assert!(facts.exists);
    assert_eq!(facts.normalized_path, ".");

    let content = ws.read_file(&handle, &readme).await.expect("read");
    assert_no_root_leak(&String::from_utf8_lossy(&content), &host_repo);
    assert!(content.windows(b"fixture".len()).any(|w| w == b"fixture"));

    ws.write_file(&handle, &readme, b"updated fixture\n")
        .await
        .expect("write");
    let updated = ws.read_file(&handle, &readme).await.expect("re-read");
    assert_eq!(updated, b"updated fixture\n");

    let entries = ws.list_dir(&handle, &root).await.expect("list");
    for entry in &entries {
        assert_no_root_leak(&entry.name, &host_repo);
    }
    assert!(entries.iter().any(|e| e.name == "README.md"));

    let grep = GrepRequest {
        pattern: "fixture".into(),
        path: root.clone(),
        glob: None,
        max_results: 10,
    };
    let matches = ws.grep(&handle, &grep).await.expect("grep");
    assert!(!matches.is_empty());
    for m in &matches {
        assert_no_root_leak(m.path.as_str(), &host_repo);
        assert_no_root_leak(&m.line, &host_repo);
    }

    let find = FindRequest {
        pattern: "README.md".into(),
        path: root.clone(),
        max_results: 10,
    };
    let found = ws.find_files(&handle, &find).await.expect("find");
    assert!(found.iter().any(|p| p.as_str() == "README.md"));
    for p in &found {
        assert_no_root_leak(p.as_str(), &host_repo);
    }

    let exec = ExecRequest::new(
        vec!["echo".into(), "test-agent".into()],
        BTreeMap::new(),
        5_000,
        root.clone(),
    )
    .expect("exec request");
    let out = ws.exec(&handle, &exec).await.expect("exec");
    assert_no_root_leak(&out.stdout, &host_repo);
    assert!(out.stdout.contains("test-agent"));

    ws.destroy_upper(&handle).await.expect("destroy");
}

#[tokio::test]
async fn rejects_invalid_paths() {
    let ws = DeterministicWorkspace::new().expect("workspace");
    let (repo, oid) = contract_tests::make_fixture_repo();
    let spec = contract_tests::minimal_upper_spec(repo.path().to_owned(), oid);
    let handle = ws.create_upper(&spec).await.expect("create_upper");

    for bad in ["", "/abs", "../x", "a\0b", "/workspace"] {
        assert!(WorkspacePath::parse(bad).is_err(), "expected reject: {bad}");
    }

    let root = WorkspacePath::project_root();
    assert!(
        ws.read_file(&handle, &root).await.is_err(),
        "root sentinel must be rejected for read"
    );

    ws.destroy_upper(&handle).await.ok();
}

#[tokio::test]
async fn content_too_large_before_write() {
    let ws = DeterministicWorkspace::new().expect("workspace");
    let (repo, oid) = contract_tests::make_fixture_repo();
    let spec = contract_tests::minimal_upper_spec(repo.path().to_owned(), oid);
    let handle = ws.create_upper(&spec).await.expect("create_upper");
    let path = WorkspacePath::parse("big.bin").expect("path");
    let oversized = vec![0u8; MAX_WORKSPACE_WRITE_BYTES + 1];
    let err = ws
        .write_file(&handle, &path, &oversized)
        .await
        .expect_err("oversized write");
    assert!(matches!(err, WorkspaceError::ContentTooLarge { .. }));
    ws.destroy_upper(&handle).await.ok();
}

#[tokio::test]
async fn git_facts_without_bypass() {
    let ws = DeterministicWorkspace::new().expect("workspace");
    let (repo, oid) = contract_tests::make_fixture_repo();
    let host_repo = repo.path().to_string_lossy().to_string();
    let spec = contract_tests::minimal_upper_spec(repo.path().to_owned(), oid);
    let handle = ws.create_upper(&spec).await.expect("create_upper");
    let git_config = WorkspacePath::parse(".git/config").expect("git path");
    let facts = ws
        .resolve_path(&handle, &git_config, PathIntent::Read)
        .await
        .expect("git facts");
    assert_no_root_leak(&format!("{facts:?}"), &host_repo);
    assert!(facts.exists);
    ws.destroy_upper(&handle).await.ok();
}

#[tokio::test]
async fn create_overwrite_delete_intents() {
    let ws = DeterministicWorkspace::new().expect("workspace");
    let (repo, oid) = contract_tests::make_fixture_repo();
    let spec = contract_tests::minimal_upper_spec(repo.path().to_owned(), oid);
    let handle = ws.create_upper(&spec).await.expect("create_upper");
    let new_file = WorkspacePath::parse("new.txt").expect("path");

    let before = ws
        .resolve_path(&handle, &new_file, PathIntent::Create)
        .await
        .expect("preflight create");
    assert!(!before.exists);

    ws.write_file(&handle, &new_file, b"hello")
        .await
        .expect("create write");
    let after = ws
        .resolve_path(&handle, &new_file, PathIntent::Overwrite)
        .await
        .expect("preflight overwrite");
    assert!(after.exists);

    ws.remove_file(&handle, &new_file).await.expect("delete");
    let gone = ws
        .resolve_path(&handle, &new_file, PathIntent::Delete)
        .await
        .expect("preflight delete");
    assert!(!gone.exists);

    ws.destroy_upper(&handle).await.ok();
}
