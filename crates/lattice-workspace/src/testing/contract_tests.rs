// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Adapter-agnostic contract test suite for the [`Workspace`] trait.
//!
//! Call [`run_suite`] with any [`Workspace`] implementation to verify the
//! contract.  This module is used by:
//!
//! - `tests/contract_deterministic.rs` — CI lane, no KVM, uses
//!   [`DeterministicWorkspace`].
//! - `tests/contract_microsandbox.rs` (future, 33-05) — KVM lane, uses
//!   `MicrosandboxWorkspace`.
//!
//! # Test fixture
//!
//! Each test function creates its own git-repo fixture via
//! [`make_fixture_repo`] so that tests are independent.

use std::path::PathBuf;

use crate::{EgressPolicy, ImageOrTemplate, UpperSpec, Workspace};

// ── Fixture helpers ──────────────────────────────────────────────────────────

/// Create a minimal git repository in an OS tempdir with one committed file.
///
/// Returns `(TempDir, baseline_oid_hex)`.  The caller must keep the
/// `TempDir` alive for the duration of the test.
pub fn make_fixture_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = git2::Repository::init(dir.path()).expect("git init");
    let sig = git2::Signature::now("contract-test", "ct@localhost").expect("sig");

    // Write one file so the commit tree is non-empty.
    let file_path = dir.path().join("README.md");
    std::fs::write(&file_path, b"# fixture\n").expect("write fixture file");

    let mut index = repo.index().expect("index");
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .expect("add_all");
    index.write().expect("index write");
    let tree_id = index.write_tree().expect("write_tree");
    let tree = repo.find_tree(tree_id).expect("find_tree");
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
        .expect("commit");

    (dir, oid.to_string())
}

/// Construct a minimal [`UpperSpec`] from a fixture repo.
///
/// Defaults to a `python:3.12-slim` OCI image under [`EgressPolicy::Open`].
/// For callers that need to drive the suite from a warm template or a
/// different egress policy (e.g. the production git-ready-template lane under
/// [`EgressPolicy::Blocked`]), use [`minimal_upper_spec_with`].
pub fn minimal_upper_spec(host_repo_path: PathBuf, baseline_oid: String) -> UpperSpec {
    // python:3.12-slim is used so that the production adapter (MicrosandboxWorkspace)
    // can execute `git init/add/commit` inside the VM (required by deliver_baseline).
    // DeterministicWorkspace ignores this field and uses a local tempdir.
    // EgressPolicy::Open is required for the production adapter so that apt-get
    // can install git inside the VM when python:3.12-slim is used (git is not
    // pre-installed in slim images).  DeterministicWorkspace ignores egress.
    minimal_upper_spec_with(
        ImageOrTemplate::Image("docker.m.daocloud.io/library/python:3.12-slim".into()),
        EgressPolicy::Open,
        host_repo_path,
        baseline_oid,
    )
}

/// Construct a minimal [`UpperSpec`] overriding the rootfs source and egress.
///
/// This is the factory the production contract lane uses to run the suite from
/// a git-ready warm template (`ImageOrTemplate::Template`) under
/// [`EgressPolicy::Blocked`] (WS-04 / M-33-05), while the deterministic lane
/// keeps using [`minimal_upper_spec`]'s `Image` + `Open` defaults unchanged.
pub fn minimal_upper_spec_with(
    image_or_template: ImageOrTemplate,
    egress: EgressPolicy,
    host_repo_path: PathBuf,
    baseline_oid: String,
) -> UpperSpec {
    UpperSpec {
        image_or_template,
        baseline_oid,
        host_repo_path,
        egress,
        credentials: vec![],
        timeout_secs: 30,
        cpus: 1,
        memory_mb: 256,
    }
}

// ── Contract suite ───────────────────────────────────────────────────────────

/// Run the full adapter-agnostic contract suite against `ws`.
///
/// Panics on the first failing assertion (like a normal `#[test]`).
///
/// Convenience wrapper over [`run_suite_with`] using [`minimal_upper_spec`]
/// (an `Image` + `Open`-egress spec).  The deterministic CI lane calls this and
/// keeps passing unchanged; the production lane calls [`run_suite_with`]
/// directly with a git-ready-template factory.
pub async fn run_suite<W: Workspace>(ws: &W) {
    run_suite_with(ws, |host_repo_path, baseline_oid| {
        minimal_upper_spec(host_repo_path, baseline_oid)
    })
    .await;
}

/// Run the full adapter-agnostic contract suite, building each test's
/// [`UpperSpec`] from a caller-supplied factory.
///
/// `make_spec(host_repo_path, baseline_oid)` is invoked once per contract test
/// with that test's fresh fixture repo, so the caller controls the rootfs
/// source (`ImageOrTemplate::Image` vs `Template`) and egress policy while the
/// fixture/OID stay per-test.  This is what lets the production lane run the
/// whole suite from a git-ready warm template under
/// [`EgressPolicy::Blocked`](crate::EgressPolicy::Blocked) (WS-04 / M-33-05)
/// without forking the test bodies.
///
/// Panics on the first failing assertion (like a normal `#[test]`).
pub async fn run_suite_with<W, F>(ws: &W, make_spec: F)
where
    W: Workspace,
    F: Fn(PathBuf, String) -> UpperSpec + Clone,
{
    test_create_upper_returns_handle(ws, make_spec.clone()).await;
    test_exec_captures_stdout(ws, make_spec.clone()).await;
    test_no_lower_path_in_handle(ws, make_spec.clone()).await;
    test_baseline_content(ws, make_spec.clone()).await;
    test_upper_git_root(ws, make_spec.clone()).await;
    test_destroy_is_idempotent(ws, make_spec).await;
}

// ── Individual contract tests ────────────────────────────────────────────────

/// V-01 — WS-01/WS-02: `create_upper` succeeds and returns a non-empty handle.
async fn test_create_upper_returns_handle<W, F>(ws: &W, make_spec: F)
where
    W: Workspace,
    F: Fn(PathBuf, String) -> UpperSpec,
{
    let (repo, oid) = make_fixture_repo();
    let spec = make_spec(repo.path().to_owned(), oid);
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("test_create_upper_returns_handle: create_upper must succeed");
    assert!(!handle.id.is_empty(), "handle.id must be non-empty");
    ws.destroy_upper(&handle).await.unwrap();
}

/// V-02 — WS-02: `exec_upper` captures stdout.
async fn test_exec_captures_stdout<W, F>(ws: &W, make_spec: F)
where
    W: Workspace,
    F: Fn(PathBuf, String) -> UpperSpec,
{
    let (repo, oid) = make_fixture_repo();
    let spec = make_spec(repo.path().to_owned(), oid);
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("test_exec_captures_stdout: create_upper must succeed");
    let out = ws
        .exec_upper(&handle, "echo", &["hello-contract"])
        .await
        .expect("test_exec_captures_stdout: exec_upper must succeed");
    assert_eq!(out.exit_code, 0, "echo must exit 0");
    assert!(
        out.stdout.contains("hello-contract"),
        "stdout must contain 'hello-contract'; got: {:?}",
        out.stdout
    );
    ws.destroy_upper(&handle).await.unwrap();
}

/// V-03 / T-33-01 — WS-02: host repo path does not appear in `handle.id`
/// or in `exec_upper` stdout.
async fn test_no_lower_path_in_handle<W, F>(ws: &W, make_spec: F)
where
    W: Workspace,
    F: Fn(PathBuf, String) -> UpperSpec,
{
    let (repo, oid) = make_fixture_repo();
    let host_path = repo.path().to_string_lossy().to_string();
    let spec = make_spec(repo.path().to_owned(), oid);
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("test_no_lower_path_in_handle: create_upper must succeed");

    // The handle's id must never contain the host repo path.
    assert!(
        !handle.id.contains(&host_path),
        "handle.id must not contain host repo path '{}'; got id: {:?}",
        host_path,
        handle.id
    );

    // exec stdout (e.g. `pwd`) must not echo the host Lower path.
    let out = ws
        .exec_upper(&handle, "pwd", &[])
        .await
        .expect("test_no_lower_path_in_handle: exec_upper pwd must succeed");
    assert!(
        !out.stdout.contains(&host_path),
        "exec stdout must not contain host repo path '{}'; stdout: {:?}",
        host_path,
        out.stdout
    );

    ws.destroy_upper(&handle).await.unwrap();
}

/// V-04 — WS-03: a file committed at baseline OID exists in the Upper worktree.
async fn test_baseline_content<W, F>(ws: &W, make_spec: F)
where
    W: Workspace,
    F: Fn(PathBuf, String) -> UpperSpec,
{
    let (repo, oid) = make_fixture_repo();
    let spec = make_spec(repo.path().to_owned(), oid);
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("test_baseline_content: create_upper must succeed");

    // The fixture repo contains README.md — it must be readable inside the Upper.
    let out = ws
        .exec_upper(&handle, "cat", &["README.md"])
        .await
        .expect("test_baseline_content: exec_upper cat must succeed");
    assert_eq!(out.exit_code, 0, "cat README.md must exit 0");
    assert!(
        out.stdout.contains("fixture"),
        "README.md must contain 'fixture'; stdout: {:?}",
        out.stdout
    );

    ws.destroy_upper(&handle).await.unwrap();
}

/// V-04 — WS-03: `git rev-parse --show-toplevel` inside the Upper returns a
/// path that exists (the Upper has its own `.git` root).
async fn test_upper_git_root<W, F>(ws: &W, make_spec: F)
where
    W: Workspace,
    F: Fn(PathBuf, String) -> UpperSpec,
{
    let (repo, oid) = make_fixture_repo();
    let spec = make_spec(repo.path().to_owned(), oid);
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("test_upper_git_root: create_upper must succeed");

    let out = ws
        .exec_upper(&handle, "git", &["rev-parse", "--show-toplevel"])
        .await
        .expect("test_upper_git_root: exec_upper must succeed");
    assert_eq!(
        out.exit_code, 0,
        "git rev-parse --show-toplevel must exit 0"
    );
    let toplevel = out.stdout.trim();
    assert!(
        !toplevel.is_empty(),
        "git rev-parse --show-toplevel must return a path"
    );

    ws.destroy_upper(&handle).await.unwrap();
}

/// V-05 (det tier) — `destroy_upper` is idempotent: a second call succeeds.
async fn test_destroy_is_idempotent<W, F>(ws: &W, make_spec: F)
where
    W: Workspace,
    F: Fn(PathBuf, String) -> UpperSpec,
{
    let (repo, oid) = make_fixture_repo();
    let spec = make_spec(repo.path().to_owned(), oid);
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("test_destroy_is_idempotent: create_upper must succeed");
    ws.destroy_upper(&handle)
        .await
        .expect("first destroy_upper must succeed");
    ws.destroy_upper(&handle)
        .await
        .expect("second destroy_upper must also succeed (idempotent)");
}
