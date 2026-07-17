// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn commit creates git history.
//!
//! Commits a turn via the Git store (`GitSessionGraphStore`). Verifies the
//! returned `TurnCommit` has a non-empty `commit_id` that is a valid 40-char
//! hex git OID, and that the commit object actually exists in the git object
//! database (looked up via gix).

use lattice_session::test_helpers::{assistant_msg, user_msg};
use lattice_session::{
    GitSessionGraphStore, SessionGraphStore, TurnCommitInput, TurnCommitStatus,
    WorkspaceDisposition,
};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    let _ = lattice_session::test_utils::init_empty_repo(dir);
}

#[test]
fn git_store_turn_commit_creates_git_history() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    let store = GitSessionGraphStore::new(dir);

    // Create a session to commit against.
    let session_id = "abcdef0123456789abcdef0123456789".to_string();
    store
        .create_session(&session_id, Some("history-test"), "coder", "main")
        .expect("create_session");

    // Build a minimal first turn (no parents).
    let messages = vec![user_msg("Hello, git history!"), assistant_msg("Hi there.")];
    let input = TurnCommitInput {
        session_id: session_id.clone(),
        parent_ids: vec![],
        messages,
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: "Turn 0".to_string(),
        usage: None,
        terminal_reason: None,
    };

    let commit = store.commit_turn(input).expect("commit_turn");

    // 1. commit_id is non-empty.
    assert!(!commit.commit_id.is_empty(), "commit_id must be non-empty");

    // 2. commit_id is a valid 40-char hex git OID.
    assert_eq!(
        commit.commit_id.len(),
        40,
        "commit_id must be 40 chars, got {}: {}",
        commit.commit_id.len(),
        commit.commit_id
    );
    assert!(
        commit.commit_id.chars().all(|c| c.is_ascii_hexdigit()),
        "commit_id must be hex: {}",
        commit.commit_id
    );

    // 3. The commit object exists in the git object database. We open the
    //    repository independently (not through the store) to confirm the
    //    object was actually persisted to .git/objects.
    let oid = gix::ObjectId::from_hex(commit.commit_id.as_bytes()).expect("parse oid");
    let repo = gix::open_opts(dir, gix::open::Options::isolated()).expect("open repo");
    let git_commit = repo
        .find_commit(oid)
        .expect("commit object must exist in git object database");

    // Sanity: the git commit's tree is non-empty (turn/ subtree present).
    let tree = git_commit.tree().expect("commit has a tree");
    assert!(
        tree.find_entry("turn").is_some(),
        "commit tree should contain a 'turn' entry"
    );

    // 4. The session branch ref points at the new commit, confirming the
    //    history is reachable (not a dangling object).
    let ref_name = format!("refs/lattice/sessions/{session_id}/main");
    let branch_ref = repo
        .find_reference(ref_name.as_str())
        .expect("session branch ref should exist");
    assert_eq!(
        branch_ref.target().try_id().map(|o| o.to_string()),
        Some(commit.commit_id.clone()),
        "session branch ref must point at the new commit"
    );
}
