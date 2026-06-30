// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! KVM-gated integration tests for `MicrosandboxWorkspace`.
//!
//! These tests run ONLY with `--features=integration_tests` and require:
//!   - KVM (`/dev/kvm` accessible)
//!   - `libkrunfw.so` present (`~/.microsandbox/lib/libkrunfw.so`)
//!   - A reachable image mirror (`docker.m.daocloud.io` — `index.docker.io` is blocked here)
//!   - `agentd` binary in `~/.cargo/registry/src/*/build/agentd`
//!
//! # Tests
//!
//! - `test_template_create_and_restore`: creates an Upper from
//!   a fresh image, stops it, snapshots as a template, then restores a new CoW
//!   Upper from the snapshot and asserts exec succeeds.
//!
//! - `test_egress_blocked_live`: creates an Upper with
//!   `EgressPolicy::Blocked` and verifies that `wget` cannot reach an external host.
//!
//! - `test_microsandbox_contract_suite`:
//!   builds a git-ready warm template, then runs the adapter-agnostic contract
//!   suite from 33-04 against `MicrosandboxWorkspace` from
//!   `ImageOrTemplate::Template` under `EgressPolicy::Blocked` — proving the
//!   suite no longer needs a per-Upper `apt-get install git` (the prior >595s
//!   timeout) and that a git baseline survives Blocked egress.
//!
//! - `test_git_ready_template_has_git_under_blocked_egress`: builds a git-ready
//!   template, restores an Upper from it under
//!   `EgressPolicy::Blocked`, and asserts `git rev-parse --show-toplevel`
//!   exits 0 — git came from the template, not apt-get.
//!
//! # Image registry note
//!
//! `index.docker.io` is blocked on this host.  All image refs use:
//!   `docker.m.daocloud.io/library/<image>:<tag>`

#![cfg(feature = "integration_tests")]

use lattice_workspace::microsandbox::MicrosandboxWorkspace;
use lattice_workspace::testing::contract_tests::{minimal_upper_spec_with, run_suite_with};
use lattice_workspace::{EgressPolicy, ImageOrTemplate, UpperSpec, Workspace};

/// Base OCI image used for git-ready template builds in this suite.
///
/// `python:3.12-slim` is Debian-based (so `APT_GET_GIT` works during the
/// one-time template bake) and ships `sh` (needed by the base64 file injection
/// in `deliver_baseline`).  `index.docker.io` is blocked on this host, so the
/// daocloud mirror is used.
const GIT_READY_BASE_IMAGE: &str = "docker.m.daocloud.io/library/python:3.12-slim";

// ── Helper ───────────────────────────────────────────────────────────────────

/// Create a minimal git repository in a tempdir for integration tests.
///
/// Returns `(TempDir, baseline_oid_hex)`.  Keeps the `TempDir` alive for the
/// duration of the test so the path remains valid during `create_upper`.
fn make_live_fixture_repo() -> (tempfile::TempDir, String) {
    let dir = tempfile::TempDir::new().expect("tempdir for fixture");
    let repo = git2::Repository::init(dir.path()).expect("git init");
    let sig = git2::Signature::now("live-test", "lt@localhost").expect("sig");
    let file_path = dir.path().join("README.md");
    std::fs::write(&file_path, b"# live fixture\n").expect("write fixture");
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

/// Build a minimal `UpperSpec` suitable for a live KVM test.
///
/// `baseline_oid` is a real commit OID from `make_live_fixture_repo`.
/// The `TempDir` must be kept alive for the duration of the test.
fn minimal_live_spec(
    image: &str,
    egress: EgressPolicy,
    host_repo_path: std::path::PathBuf,
    baseline_oid: String,
) -> UpperSpec {
    UpperSpec {
        image_or_template: ImageOrTemplate::Image(image.to_string()),
        baseline_oid,
        host_repo_path,
        egress,
        credentials: vec![],
        timeout_secs: 120,
        cpus: 1,
        memory_mb: 512,
    }
}

// ── template create and restore ────────────────────────────────────────────

/// `template_create` produces a named SDK snapshot; a new Upper
/// can be restored from it via `Sandbox::builder().from_snapshot()`.
///
/// Flow:
/// 1. `create_upper` with python:3.12-slim image (has git + sh) → live Upper.
/// 2. Write a sentinel file via `exec_upper` to verify CoW isolation.
/// 3. `template_create` → named snapshot.
/// 4. `restore_upper` → new CoW Upper from the snapshot.
/// 5. Assert `exec_upper` on the restored Upper succeeds (it booted).
/// 6. Assert the sentinel file is present in the restored Upper.
/// 7. `destroy_upper` for the restored Upper.
///
/// # Image choice
///
/// `python:3.12-slim` is used because it has `git` (needed for deliver_baseline)
/// and `sh` (needed for inject_dir_to_vm base64 injection).  `alpine:3.19` lacks
/// `git` by default.
#[tokio::test]
async fn test_template_create_and_restore() {
    // python:3.12-slim has git and sh (alpine does not have git by default).
    const IMAGE: &str = "docker.m.daocloud.io/library/python:3.12-slim";
    // Use a UUID-based template name to avoid collision with leftovers from prior runs.
    let template_name = format!(
        "lattice-test-v05-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );

    let ws = MicrosandboxWorkspace::from_env();

    // Create a real fixture repo so git_archive_to_tempdir succeeds.
    let (_fixture_dir, baseline_oid) = make_live_fixture_repo();

    // Step 1: create an Upper from a fresh image.
    let spec = minimal_live_spec(
        IMAGE,
        EgressPolicy::Open,
        _fixture_dir.path().to_path_buf(),
        baseline_oid.clone(),
    );
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("create_upper from image must succeed on KVM host");

    // Step 2: write a sentinel file to verify it survives into the snapshot.
    // Use /root/ not /tmp/ — /tmp may be a ramdisk volatile across snapshots.
    // Wave 0 A4 used /root/wave0_a4_marker successfully.
    let write_out = ws
        .exec_upper(
            &handle,
            "sh",
            &["-c", "echo V05_SENTINEL > /root/v05_sentinel"],
        )
        .await
        .expect("exec sh to write sentinel must succeed");
    assert_eq!(
        write_out.exit_code, 0,
        "sentinel write must exit 0; stderr: {:?}",
        write_out.stderr
    );

    // Step 3: template_create (stops sandbox, snapshots it).
    let snap_name = ws
        .template_create(&handle, &template_name)
        .await
        .expect("template_create must succeed on KVM host");
    assert_eq!(
        snap_name, template_name,
        "returned snapshot name must match"
    );

    // Step 4: restore a new CoW Upper from the snapshot.
    let restored_spec = UpperSpec {
        image_or_template: ImageOrTemplate::Template(template_name.clone()),
        ..spec.clone()
    };
    let new_name = format!(
        "lattice-test-v05-restored-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let new_name = new_name.as_str();
    let restored_handle = ws
        .restore_upper(&template_name, new_name, &restored_spec)
        .await
        .expect("restore_upper from snapshot must succeed on KVM host");

    // Step 5: exec inside the restored Upper.
    let echo_out = ws
        .exec_upper(&restored_handle, "echo", &["restored-ok"])
        .await
        .expect("exec_upper on restored Upper must succeed");
    assert_eq!(echo_out.exit_code, 0, "echo in restored Upper must exit 0");
    assert!(
        echo_out.stdout.contains("restored-ok"),
        "stdout must contain 'restored-ok'; got: {:?}",
        echo_out.stdout
    );

    // Step 6: sentinel must be readable in the restored Upper (CoW over snapshot).
    let read_out = ws
        .exec_upper(&restored_handle, "cat", &["/root/v05_sentinel"])
        .await
        .expect("cat sentinel in restored Upper must succeed");
    assert_eq!(read_out.exit_code, 0, "cat sentinel must exit 0");
    assert!(
        read_out.stdout.contains("V05_SENTINEL"),
        "restored Upper must contain sentinel; stdout: {:?}",
        read_out.stdout
    );

    // Step 7: teardown.
    ws.destroy_upper(&restored_handle)
        .await
        .expect("destroy_upper on restored Upper must succeed");
}

// ── blocked egress live enforcement ───────────────────────────────────────

/// `EgressPolicy::Blocked` denies external host reachability
/// inside an Upper on a live KVM host.
///
/// Uses `wget -q -O - https://example.com` which exits non-zero when the
/// connection is refused / times out.  Alpine `wget` is BusyBox wget.
///
/// # Image note
///
/// `alpine:3.19` is used here because the egress test only needs `wget` and
/// `sh` (present in alpine by default).  `deliver_baseline` still requires
/// `git` inside the VM — alpine does not have `git`, so `deliver_baseline` will
/// fail at the `git init` step.  We accept this: the purpose is to
/// verify that egress enforcement fires, not to verify baseline delivery.
/// A workaround: use a small debian image that has git, or install git in the
/// VM first.  For simplicity, we use `python:3.12-slim` (has git + wget via apt).
#[tokio::test]
async fn test_egress_blocked_live() {
    // python:3.12-slim has git (needed for deliver_baseline) and wget can be
    // installed; however to keep this test fast we use alpine and skip the
    // git-init failure by installing git first.
    // Simplest: use python:3.12-slim which has apt and git pre-installed.
    const IMAGE: &str = "docker.m.daocloud.io/library/python:3.12-slim";

    let ws = MicrosandboxWorkspace::from_env();

    // Create a real fixture repo so git_archive_to_tempdir succeeds.
    let (_fixture_dir, baseline_oid) = make_live_fixture_repo();

    let spec = minimal_live_spec(
        IMAGE,
        EgressPolicy::Blocked,
        _fixture_dir.path().to_path_buf(),
        baseline_oid,
    );
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("create_upper with Blocked egress must succeed on KVM host");

    // python:3.12-slim has Python 3 available. Use it to attempt a network connection.
    // This avoids depending on curl/wget which may not be installed.
    // Python's urllib.request.urlopen with a short timeout will fail on blocked egress.
    let out = ws
        .exec_upper(
            &handle,
            "python3",
            &[
                "-c",
                "import urllib.request, socket; \
                 socket.setdefaulttimeout(5); \
                 urllib.request.urlopen('https://example.com', timeout=5); \
                 print('CONNECTED')",
            ],
        )
        .await
        .expect("exec_upper python3 must not panic (exit code may be non-zero)");

    assert_ne!(
        out.exit_code, 0,
        "blocked egress must cause python3 urllib to fail (exit non-zero); \
         stdout: {:?}, stderr: {:?}",
        out.stdout, out.stderr
    );

    ws.destroy_upper(&handle)
        .await
        .expect("destroy_upper must succeed");
}

// ── contract suite from a git-ready template ──────────────────────────────

/// Run the adapter-agnostic contract suite (33-04) against `MicrosandboxWorkspace`
/// from a **git-ready warm template** under `EgressPolicy::Blocked`.
///
/// This is the production-adapter verification required by (deferred from
/// 33-04 to 33-05) and the closure (gap-closure Plan 33-06).  On a KVM
/// host, the full suite proves on the real microsandbox
/// backend — now without a per-Upper `apt-get install git` (the prior >595s
/// timeout) and under Blocked egress (proving git survives without network).
///
/// Flow:
/// 1. Build a git-ready template once with `build_git_ready_template`.
/// 2. Run the whole suite via `run_suite_with`, where each contract test's
///    `UpperSpec` is `ImageOrTemplate::Template(<template>)` + `EgressPolicy::Blocked`.
/// 3. The suite uses `make_fixture_repo()` internally for each test, so
///    `baseline_oid` is a real commit OID and `host_repo_path` is a real tempdir.
#[tokio::test]
async fn test_microsandbox_contract_suite() {
    let ws = MicrosandboxWorkspace::from_env();

    // Build the git-ready template once; every Upper in the suite restores from it.
    let template_name = format!(
        "lattice-test-gitready-suite-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    ws.build_git_ready_template(GIT_READY_BASE_IMAGE, &template_name)
        .await
        .expect("contract suite: build_git_ready_template must succeed on KVM host");

    // Run the full suite from the template under Blocked egress. The factory
    // overrides image/template + egress; the fixture repo + OID stay per-test.
    let template_for_factory = template_name.clone();
    run_suite_with(&ws, move |host_repo_path, baseline_oid| {
        minimal_upper_spec_with(
            ImageOrTemplate::Template(template_for_factory.clone()),
            EgressPolicy::Blocked,
            host_repo_path,
            baseline_oid,
        )
    })
    .await;
}

// ── git root survives Blocked egress via template ───────────────────────────

/// an Upper restored from a git-ready
/// template has a functional `/workspace` `.git` root **even under
/// `EgressPolicy::Blocked`**, because git came from the template rather than a
/// per-Upper `apt-get install git` (which Blocked egress would prevent).
///
/// Flow:
/// 1. Build a git-ready template with `build_git_ready_template`.
/// 2. `create_upper` a fresh Upper FROM that template with `EgressPolicy::Blocked`
///    (`ImageOrTemplate::Template`). `create_upper(Template)` = `from_snapshot`
///    (git comes from the template) + `deliver_baseline` (creates `/workspace` and
///    runs `git init/add/commit` using the template's git — no per-Upper apt-get,
///    so it succeeds under Blocked egress). `restore_upper` is the wrong primitive
///    here: it is a raw CoW restore of an already-delivered workspace snapshot
///    (`template_create` output) and does not deliver a baseline, so it would leave
///    the Upper without a `/workspace` root.
/// 3. Assert `git rev-parse --show-toplevel` exits 0 inside the Upper.
/// 4. Tear the Upper down with `destroy_upper`.
#[tokio::test]
async fn test_git_ready_template_has_git_under_blocked_egress() {
    let ws = MicrosandboxWorkspace::from_env();

    // Real fixture repo so deliver_baseline's git_archive_to_tempdir succeeds.
    let (_fixture_dir, baseline_oid) = make_live_fixture_repo();

    // Step 1: build the git-ready template (open egress for the one-time bake).
    let template_name = format!(
        "lattice-test-gitready-blocked-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    ws.build_git_ready_template(GIT_READY_BASE_IMAGE, &template_name)
        .await
        .expect("build_git_ready_template must succeed on KVM host");

    // Step 2: provision a fresh Upper FROM the git-ready template under Blocked
    // egress via create_upper. create_upper(Template) = from_snapshot (git comes
    // from the template) + deliver_baseline (creates /workspace, git init/add/
    // commit using the template's git — no apt-get, so it works under Blocked).
    // restore_upper is NOT used here: it is a raw CoW restore of an already-
    // delivered workspace snapshot and would leave the Upper without /workspace.
    let upper_spec = minimal_upper_spec_with(
        ImageOrTemplate::Template(template_name.clone()),
        EgressPolicy::Blocked,
        _fixture_dir.path().to_path_buf(),
        baseline_oid,
    );
    let handle = ws
        .create_upper(&upper_spec)
        .await
        .expect("create_upper from git-ready template under Blocked egress must succeed");

    // Step 3: git must be available and /workspace must be a git root.
    let out = ws
        .exec_upper(&handle, "git", &["rev-parse", "--show-toplevel"])
        .await
        .expect("exec_upper git rev-parse must succeed");
    assert_eq!(
        out.exit_code, 0,
        "git rev-parse --show-toplevel must exit 0 under Blocked egress (git from template); \
         stdout: {:?}, stderr: {:?}",
        out.stdout, out.stderr
    );
    assert!(
        !out.stdout.trim().is_empty(),
        "git rev-parse --show-toplevel must return a path; stdout: {:?}",
        out.stdout
    );

    // Step 4: teardown.
    ws.destroy_upper(&handle)
        .await
        .expect("destroy_upper must succeed");
}
