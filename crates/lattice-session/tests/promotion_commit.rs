// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Object-only promoted Session commit builder tests.

use git2::Oid;
use lattice_session::{
    PROMOTION_MANIFEST_SCHEMA_VERSION_V2, PromotedSessionCommitInput, PromotedSessionManifest,
    create_promoted_session_commit,
};

const BASELINE_OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CANDIDATE_OID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SESSION_ID: &str = "sess-01";
const TURN_ID: &str = "turn-01";
const COMMITTED_AT: i64 = 1_700_000_000;

fn init_repo() -> (tempfile::TempDir, git2::Repository) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = git2::Repository::init(dir.path()).expect("git init");
    (dir, repo)
}

fn sample_input(parent_session_oid: Option<Oid>) -> PromotedSessionCommitInput {
    PromotedSessionCommitInput {
        session_id: SESSION_ID.to_string(),
        turn_id: TURN_ID.to_string(),
        baseline_oid: BASELINE_OID.to_string(),
        candidate_oid: CANDIDATE_OID.to_string(),
        committed_at_unix_seconds: COMMITTED_AT,
        parent_session_oid,
        target_project_ref: "refs/heads/main".to_string(),
        candidate_ref: Some("refs/lattice/candidate/abc".to_string()),
        execution_status: lattice_session::graph::TurnCommitStatus::Completed,
        verification: None,
    }
}

fn refs_snapshot(repo: &git2::Repository) -> Vec<(String, Option<Oid>)> {
    repo.references()
        .expect("refs iterator")
        .map(|reference| {
            let reference = reference.expect("reference");
            let name = reference.name().expect("ref name").to_string();
            let target = reference.target();
            (name, target)
        })
        .collect()
}

fn read_manifest(repo: &git2::Repository, commit_oid: Oid) -> PromotedSessionManifest {
    let commit = repo.find_commit(commit_oid).expect("commit");
    let tree = commit.tree().expect("commit tree");
    let turn_entry = tree
        .get_name("turn")
        .expect("turn/ entry")
        .to_object(repo)
        .expect("turn object");
    let turn_tree = turn_entry.peel_to_tree().expect("turn tree");
    let manifest_entry = turn_tree
        .get_name("manifest.json")
        .expect("manifest.json entry");
    let blob = manifest_entry
        .to_object(repo)
        .expect("manifest object")
        .peel_to_blob()
        .expect("manifest blob");
    let text = std::str::from_utf8(blob.content()).expect("manifest utf8");
    let value: serde_json::Value = serde_json::from_str(text).expect("manifest json");

    PromotedSessionManifest {
        schema_version: value["schema_version"].as_u64().expect("schema_version") as u32,
        session_id: value["session_id"]
            .as_str()
            .expect("session_id")
            .to_string(),
        turn_id: value["turn_id"].as_str().expect("turn_id").to_string(),
        baseline_oid: value["baseline_oid"]
            .as_str()
            .expect("baseline_oid")
            .to_string(),
        candidate_oid: value["candidate_oid"]
            .as_str()
            .expect("candidate_oid")
            .to_string(),
        disposition: value["disposition"]
            .as_str()
            .expect("disposition")
            .to_string(),
        committed_at_unix_seconds: value["committed_at_unix_seconds"]
            .as_i64()
            .expect("committed_at_unix_seconds"),
        parent_oid: value["parent_oid"].as_str().map(str::to_string),
        target_project_ref: value["target_project_ref"]
            .as_str()
            .expect("target_project_ref")
            .to_string(),
        candidate_ref: value["candidate_ref"].as_str().map(str::to_string),
        execution_status: value["execution_status"]
            .as_str()
            .expect("execution_status")
            .to_string(),
        verification: None,
    }
}

#[test]
fn session_commit_object_is_deterministic_and_ref_neutral() {
    let (_dir, repo) = init_repo();
    let refs_before = refs_snapshot(&repo);
    let input = sample_input(None);

    let first = create_promoted_session_commit(&repo, &input).expect("first commit");
    let second = create_promoted_session_commit(&repo, &input).expect("second commit");
    let refs_after = refs_snapshot(&repo);

    assert_eq!(first, second, "identical input must yield identical OID");
    assert_eq!(
        refs_before, refs_after,
        "object creation must not move refs"
    );

    let manifest = read_manifest(&repo, first);
    assert_eq!(
        manifest,
        PromotedSessionManifest {
            schema_version: PROMOTION_MANIFEST_SCHEMA_VERSION_V2,
            session_id: SESSION_ID.to_string(),
            turn_id: TURN_ID.to_string(),
            baseline_oid: BASELINE_OID.to_string(),
            candidate_oid: CANDIDATE_OID.to_string(),
            disposition: "promoted".to_string(),
            committed_at_unix_seconds: COMMITTED_AT,
            parent_oid: None,
            target_project_ref: "refs/heads/main".to_string(),
            candidate_ref: Some("refs/lattice/candidate/abc".to_string()),
            execution_status: "completed".to_string(),
            verification: None,
        }
    );

    let commit = repo.find_commit(first).expect("created commit");
    assert_eq!(commit.parent_count(), 0, "root commit must have no parent");
}

#[test]
fn session_commit_object_links_optional_parent() {
    let (_dir, repo) = init_repo();
    let root_oid = create_promoted_session_commit(&repo, &sample_input(None)).expect("root commit");

    let refs_before = refs_snapshot(&repo);
    let child_input = sample_input(Some(root_oid));
    let child_oid = create_promoted_session_commit(&repo, &child_input).expect("child commit");
    let refs_after = refs_snapshot(&repo);

    assert_ne!(root_oid, child_oid);
    assert_eq!(
        refs_before, refs_after,
        "parented object creation must not move refs"
    );

    let child = repo.find_commit(child_oid).expect("child commit");
    assert_eq!(
        child.parent_count(),
        1,
        "child must have exactly one parent"
    );
    assert_eq!(
        child.parent_id(0).expect("parent id"),
        root_oid,
        "parent OID must match supplied session ref OID"
    );

    let manifest = read_manifest(&repo, child_oid);
    assert_eq!(manifest.parent_oid, Some(root_oid.to_string()));
}

#[test]
fn promotion_crate_has_no_workspace_or_api_dependency() {
    let manifest = include_str!("../Cargo.toml");
    assert!(
        !manifest.contains("lattice-workspace"),
        "lattice-session must not depend on lattice-workspace"
    );
    assert!(
        !manifest.contains("lattice-api"),
        "lattice-session must not depend on lattice-api"
    );
}

#[test]
fn promotion_module_is_object_only() {
    let source = include_str!("../src/promotion.rs");
    for forbidden in [
        "update-ref",
        "update_ref",
        "set_target",
        "lattice-workspace",
        "lattice-api",
        "repo.reference",
        "references(",
    ] {
        assert!(
            !source.contains(forbidden),
            "promotion.rs must not contain '{forbidden}'"
        );
    }
}
