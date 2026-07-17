// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Diff reports unique commits per side.
//!
//! Creates a feature branch from a base commit, adds one commit on main and
//! one on feature. Diff main vs feature reports the main-only commit in
//! `only_in_from` and the feature-only commit in `only_in_to`, with the base
//! as `common_ancestor_id`.


use lattice_session::test_helpers::{assistant_msg, user_msg};
use lattice_session::{
    GitSessionGraphStore, SessionGraphStore, TurnCommitInput, TurnCommitStatus,
    WorkspaceDisposition,
};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    let _ = lattice_session::test_utils::init_empty_repo(dir);
}

fn turn_input(session_id: &str, parent_ids: Vec<String>, label: &str) -> TurnCommitInput {
    TurnCommitInput {
        session_id: session_id.to_string(),
        parent_ids,
        messages: vec![
            user_msg(&format!("{label} user")),
            assistant_msg(&format!("{label} assistant")),
        ],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: label.to_string(),
        usage: None,
        terminal_reason: None,
    }
}

#[test]
fn git_store_diff_reports_unique_commits_per_side() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    let store = GitSessionGraphStore::new(dir);

    let session_id = "0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_string();
    store
        .create_session(&session_id, Some("diff-test"), "coder", "main")
        .expect("create_session");

    // 1. Base commit on main.
    let base = store
        .commit_turn(turn_input(&session_id, vec![], "base"))
        .expect("base commit");
    let base_oid = base.commit_id.clone();

    // 2. One more commit on main (main-only).
    let main_only = store
        .commit_turn(turn_input(&session_id, vec![base_oid.clone()], "main-only"))
        .expect("main-only commit");
    let main_only_oid = main_only.commit_id.clone();

    // 3. Create feature branch from the base commit.
    store
        .branch(&session_id, "feature", Some(&base_oid))
        .expect("branch feature");

    // 4. Checkout feature and add one commit (feature-only).
    store
        .checkout(&session_id, "feature")
        .expect("checkout feature");
    let feature_only = store
        .commit_turn(turn_input(
            &session_id,
            vec![base_oid.clone()],
            "feature-only",
        ))
        .expect("feature-only commit");
    let feature_only_oid = feature_only.commit_id.clone();

    // 5. Diff main vs feature.
    let diff = store.diff(&session_id, "main", "feature").expect("diff");

    // common_ancestor_id is the base commit.
    assert_eq!(
        diff.common_ancestor_id.as_deref(),
        Some(base_oid.as_str()),
        "common ancestor should be the base commit",
    );

    // only_in_from (main side) contains exactly the main-only commit.
    assert_eq!(
        diff.only_in_from.len(),
        1,
        "only_in_from should have exactly 1 commit, got {}: {:?}",
        diff.only_in_from.len(),
        diff.only_in_from
            .iter()
            .map(|e| &e.commit_id)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        diff.only_in_from[0].commit_id, main_only_oid,
        "only_in_from should contain the main-only commit",
    );

    // only_in_to (feature side) contains exactly the feature-only commit.
    assert_eq!(
        diff.only_in_to.len(),
        1,
        "only_in_to should have exactly 1 commit, got {}: {:?}",
        diff.only_in_to.len(),
        diff.only_in_to
            .iter()
            .map(|e| &e.commit_id)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        diff.only_in_to[0].commit_id, feature_only_oid,
        "only_in_to should contain the feature-only commit",
    );

    // The base commit must not appear on either side (it is the common
    // ancestor and is hidden from both revwalks).
    let from_ids: Vec<&str> = diff
        .only_in_from
        .iter()
        .map(|e| e.commit_id.as_str())
        .collect();
    let to_ids: Vec<&str> = diff
        .only_in_to
        .iter()
        .map(|e| e.commit_id.as_str())
        .collect();
    assert!(
        !from_ids.contains(&base_oid.as_str()),
        "base commit must not appear in only_in_from",
    );
    assert!(
        !to_ids.contains(&base_oid.as_str()),
        "base commit must not appear in only_in_to",
    );

    // from/to commit ids are the resolved OIDs of the requested refs.
    assert_eq!(
        diff.from_commit_id, main_only_oid,
        "from should be main tip OID"
    );
    assert_eq!(
        diff.to_commit_id, feature_only_oid,
        "to should be feature tip OID"
    );
}
