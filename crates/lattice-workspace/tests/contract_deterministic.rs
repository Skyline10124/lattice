// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Deterministic contract suite — CI lane, no KVM required.
//!
//! Runs the adapter-agnostic [`contract_tests::run_suite`] against
//! [`DeterministicWorkspace`], covering SC5
//! validation rows.
//!
//! This file is compiled whenever `--features=test-utils` is present.
//! It does NOT require the `integration_tests` feature.

// Only compile this integration harness when test-utils is enabled.
#![cfg(feature = "test-utils")]

use lattice_workspace::testing::DeterministicWorkspace;
use lattice_workspace::testing::contract_tests;

/// Main contract gate: run the full adapter-agnostic suite against
/// `DeterministicWorkspace`.  Covers (det tier).
#[tokio::test]
async fn test_deterministic_contract_suite() {
    let ws = DeterministicWorkspace::new().expect("DeterministicWorkspace::new");
    contract_tests::run_suite(&ws).await;
}

/// Policy-recording test (DeterministicWorkspace-specific — uses
/// `requested_policy()` which is not on the `Workspace` trait).
#[tokio::test]
async fn test_deterministic_policy_is_recorded() {
    use lattice_workspace::{EgressPolicy, ImageOrTemplate, UpperSpec, Workspace};

    let ws = DeterministicWorkspace::new().expect("DeterministicWorkspace::new");
    let (repo, oid) = contract_tests::make_fixture_repo();
    let spec = UpperSpec {
        image_or_template: ImageOrTemplate::Image(
            "docker.m.daocloud.io/library/alpine:3.19".into(),
        ),
        baseline_oid: oid,
        host_repo_path: repo.path().to_owned(),
        egress: EgressPolicy::Blocked,
        credentials: vec![],
        timeout_secs: 30,
        cpus: 1,
        memory_mb: 256,
    };
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("create_upper must succeed");

    // DeterministicWorkspace-specific: inspect the recorded policy.
    let recorded = ws.requested_policy(&handle);
    assert!(
        matches!(recorded, Some(EgressPolicy::Blocked)),
        "policy must be recorded as Blocked; got {:?}",
        recorded
    );

    ws.destroy_upper(&handle).await.unwrap();
}

/// Ensure handle.id does not contain the host repo path.
/// Duplicates test_no_lower_path_in_handle from run_suite but is kept here
/// as an explicit named test for CI reporting clarity.
#[tokio::test]
async fn test_no_lower_path_in_handle_explicit() {
    use lattice_workspace::{EgressPolicy, ImageOrTemplate, UpperSpec, Workspace};

    let ws = DeterministicWorkspace::new().expect("DeterministicWorkspace::new");
    let (repo, oid) = contract_tests::make_fixture_repo();
    let host_path = repo.path().to_string_lossy().to_string();
    let spec = UpperSpec {
        image_or_template: ImageOrTemplate::Image(
            "docker.m.daocloud.io/library/alpine:3.19".into(),
        ),
        baseline_oid: oid,
        host_repo_path: repo.path().to_owned(),
        egress: EgressPolicy::Open,
        credentials: vec![],
        timeout_secs: 30,
        cpus: 1,
        memory_mb: 256,
    };
    let handle = ws
        .create_upper(&spec)
        .await
        .expect("create_upper must succeed");

    assert!(
        !handle.id.contains(&host_path),
        "handle.id must not contain host repo path; id={:?}, host_path={host_path}",
        handle.id
    );

    let out = ws
        .exec_upper(&handle, "pwd", &[])
        .await
        .expect("exec_upper pwd must succeed");
    assert!(
        !out.stdout.contains(&host_path),
        "exec stdout must not contain host repo path; stdout={:?}",
        out.stdout
    );

    ws.destroy_upper(&handle).await.unwrap();
}
