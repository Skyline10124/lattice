// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Reset moves branch pointer.
//!
//! On a branch with 3 commits, reset to the first commit. The branch ref now
//! points to the first commit, HEAD reflects the reset, and the old commits
//! (2nd and 3rd) are still reachable by OID in the git object database.

use lattice_session::test_helpers::{assistant_msg, user_msg};
use lattice_session::{
    GitSessionGraphStore, GraphHead, SessionGraphStore, TurnCommitInput, TurnCommitStatus,
    WorkspaceDisposition,
};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    let _ = lattice_session::test_utils::init_empty_repo(dir);
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
fn git_store_reset_moves_branch_pointer() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    let store = GitSessionGraphStore::new(dir);

    let session_id = "a1b2c3d4e5f60718293a4b5c6d7e8f90".to_string();
    store
        .create_session(&session_id, Some("reset-test"), "coder", "main")
        .expect("create_session");

    // Commit 3 turns linearly on main.
    let mut commit_ids: Vec<String> = Vec::new();
    let mut parent_ids: Vec<String> = vec![];
    for i in 0..3 {
        let commit = store
            .commit_turn(turn_input(
                &session_id,
                parent_ids.clone(),
                &format!("turn-{i}"),
            ))
            .expect("commit turn");
        parent_ids = vec![commit.commit_id.clone()];
        commit_ids.push(commit.commit_id);
    }
    let first_oid = commit_ids[0].clone();
    let second_oid = commit_ids[1].clone();
    let third_oid = commit_ids[2].clone();

    // Sanity: before reset, main ref points at the third commit.
    let status_before = store.status(&session_id).expect("status before reset");
    assert_eq!(
        status_before.current_commit_id.as_deref(),
        Some(third_oid.as_str()),
        "main should point at third commit before reset",
    );
    assert_eq!(status_before.commit_count, 3);

    // Reset main to the first commit.
    let reset_result = store.reset(&session_id, &first_oid).expect("reset");
    assert_eq!(reset_result.branch, "main");
    assert_eq!(
        reset_result.old_commit_id.as_deref(),
        Some(third_oid.as_str())
    );
    assert_eq!(reset_result.new_commit_id, first_oid);

    // 1. The branch ref now points to the first commit.
    let repo = gix::open_opts(dir, gix::open::Options::isolated()).expect("open repo");
    let ref_name = format!("refs/lattice/sessions/{session_id}/main");
    let branch_ref = repo
        .find_reference(ref_name.as_str())
        .expect("main ref should exist");
    assert_eq!(
        branch_ref.target().try_id().map(|o| o.to_string()),
        Some(first_oid.clone()),
        "main branch ref must point at the first commit after reset",
    );

    // 2. HEAD reflects the reset (still on main branch, but now at first commit).
    let status_after = store.status(&session_id).expect("status after reset");
    assert!(
        matches!(&status_after.head, GraphHead::Branch { name } if name == "main"),
        "HEAD should still be on main branch after reset, got: {:?}",
        status_after.head,
    );
    assert_eq!(
        status_after.current_commit_id.as_deref(),
        Some(first_oid.as_str()),
        "HEAD should resolve to the first commit after reset",
    );
    assert_eq!(
        status_after.commit_count, 1,
        "reachable commit count from HEAD should be 1 after reset to first commit",
    );

    // 3. Old commits (2nd and 3rd) are still reachable by OID in the object
    //    database. Reset only moves the ref; it must not delete the commits.
    for old_oid in [&second_oid, &third_oid] {
        let oid = gix::ObjectId::from_hex(old_oid.as_bytes()).expect("parse old oid");
        let commit = repo
            .find_commit(oid)
            .unwrap_or_else(|_| panic!("old commit {old_oid} must still exist in object db"));
        // Confirm it is a real lattice turn commit by checking its tree.
        let tree = commit.tree().expect("old commit has a tree");
        assert!(
            tree.find_entry("turn").is_some(),
            "old commit {old_oid} should still have a turn/ tree entry",
        );
    }

    // 4. The old commits are also retrievable via get_turn_commit (store API).
    for old_oid in [&second_oid, &third_oid] {
        let found = store
            .get_turn_commit(&session_id, old_oid)
            .expect("get_turn_commit")
            .unwrap_or_else(|| panic!("get_turn_commit should find {old_oid}"));
        assert_eq!(found.commit_id, *old_oid);
        assert_eq!(found.session_id, session_id);
    }
}
