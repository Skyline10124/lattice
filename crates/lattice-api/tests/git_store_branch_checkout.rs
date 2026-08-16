// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Branch and checkout on git refs.
//!
//! Creates a feature branch from the first commit, checks out to feature
//! (HEAD moves to feature), checks out back to main (HEAD returns to main),
//! then detaches to a commit OID (HEAD is detached at that commit).

use lattice_session::test_helpers::{assistant_msg, user_msg};
use lattice_session::{
    GitSessionGraphStore, GraphHead, SessionGraphStore, TurnCommitInput, TurnCommitStatus,
    WorkspaceDisposition,
};
use tempfile::TempDir;

fn test_store_key() -> lattice_session::ProjectStoreKey {
    lattice_session::ProjectStoreKey::from_canonical_path_bytes(b"/lattice-api-tests/project")
}

fn turn_input(session_id: &str, parent_ids: Vec<String>, label: &str) -> TurnCommitInput {
    TurnCommitInput {
        workspace_history_oid: None,
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
fn git_store_branch_and_checkout_moves_head() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();

    let store = GitSessionGraphStore::create(dir.join("sessions.git")).expect("create store");

    let session_id = "1234567890abcdef1234567890abcdef".to_string();
    store
        .create_session(
            &session_id,
            Some("branch-checkout"),
            "coder",
            "main",
            &test_store_key(),
        )
        .expect("create_session");

    // Commit two turns linearly on main so we have a first commit to branch
    // from and a divergent tip.
    let first = store
        .commit_turn(turn_input(&session_id, vec![], "turn-0"))
        .expect("commit turn 0");
    let _second = store
        .commit_turn(turn_input(
            &session_id,
            vec![first.commit_id.clone()],
            "turn-1",
        ))
        .expect("commit turn 1");

    let first_oid = first.commit_id.clone();

    // 1. Create a feature branch from the first commit.
    let feature_ref = store
        .branch(&session_id, "feature", Some(&first_oid))
        .expect("branch feature");
    assert_eq!(feature_ref.name, "feature");
    assert_eq!(feature_ref.commit_id.as_deref(), Some(first_oid.as_str()));

    // 2. Checkout to feature. HEAD should move to the feature branch.
    let head = store
        .checkout(&session_id, "feature")
        .expect("checkout feature");
    assert!(
        matches!(&head, GraphHead::Branch { name } if name == "feature"),
        "HEAD should be on feature branch after checkout, got: {:?}",
        head,
    );

    // Confirm via status that HEAD resolves to the feature ref target
    // (the first commit), not main's tip (the second commit).
    let status_on_feature = store.status(&session_id).expect("status on feature");
    assert_eq!(
        status_on_feature.current_commit_id.as_deref(),
        Some(first_oid.as_str()),
        "HEAD on feature should point at first commit",
    );

    // 3. Checkout back to main. HEAD should return to the main branch.
    let head = store.checkout(&session_id, "main").expect("checkout main");
    assert!(
        matches!(&head, GraphHead::Branch { name } if name == "main"),
        "HEAD should be on main branch after checkout, got: {:?}",
        head,
    );

    let status_on_main = store.status(&session_id).expect("status on main");
    // main's tip is the second commit; verify HEAD is no longer at first_oid.
    assert_ne!(
        status_on_main.current_commit_id.as_deref(),
        Some(first_oid.as_str()),
        "HEAD on main should not point at the first commit",
    );
    assert!(
        status_on_main.current_commit_id.is_some(),
        "main HEAD should resolve to a commit",
    );

    // 4. Detach to the first commit OID. HEAD should be detached at that commit.
    let head = store
        .checkout(&session_id, &first_oid)
        .expect("checkout detach");
    assert!(
        matches!(&head, GraphHead::Detached { commit_id } if commit_id == &first_oid),
        "HEAD should be detached at first commit OID, got: {:?}",
        head,
    );

    let status_detached = store.status(&session_id).expect("status detached");
    assert_eq!(
        status_detached.current_commit_id.as_deref(),
        Some(first_oid.as_str()),
        "detached HEAD should point at the first commit",
    );
    assert!(
        matches!(&status_detached.head, GraphHead::Detached { commit_id } if *commit_id == first_oid),
        "status head should report detached at first commit, got: {:?}",
        status_detached.head,
    );
}
