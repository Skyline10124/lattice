// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Snapshot lost flag round-trips.
//!
//! After committing a turn, call `set_snapshot_lost(commit_id, true)` then
//! `get_snapshot_lost(commit_id)` returns `Some(true)`. Set it to false, get
//! returns `Some(false)`.

use std::fs;

use lattice_session::{
    GitSessionGraphStore, SessionGraphStore, TurnCommitInput, TurnCommitStatus,
    WorkspaceDisposition,
};
use lattice_session::test_helpers::{assistant_msg, user_msg};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    fs::create_dir_all(dir).expect("create dir");
    git2::Repository::init(dir).expect("init repo");
}

#[test]
fn git_store_snapshot_lost_flag_round_trips() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    let store = GitSessionGraphStore::new(dir);

    let session_id = "f1e2d3c4b5a69788796a5b4c3d2e1f00".to_string();
    store
        .create_session(&session_id, Some("snapshot-lost-test"), "coder", "main")
        .expect("create_session");

    // Commit a turn to get a real commit_id.
    let input = TurnCommitInput {
        session_id: session_id.clone(),
        parent_ids: vec![],
        messages: vec![user_msg("run"), assistant_msg("done")],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: "turn-0".to_string(),
        usage: None,
        terminal_reason: None,
    };
    let commit = store.commit_turn(input).expect("commit_turn");
    let commit_id = commit.commit_id.clone();

    // 1. Before any set call, get_snapshot_lost returns None (no ops state
    //    file exists yet). This documents the default and guards against the
    //    flag silently defaulting to a non-None value.
    assert_eq!(
        store.get_snapshot_lost(&commit_id).expect("get default"),
        None,
        "get_snapshot_lost should be None before any set call",
    );

    // 2. Set to true, then get returns Some(true).
    store
        .set_snapshot_lost(&commit_id, true)
        .expect("set true");
    assert_eq!(
        store.get_snapshot_lost(&commit_id).expect("get true"),
        Some(true),
        "get_snapshot_lost should return Some(true) after set_snapshot_lost(true)",
    );

    // 3. Set to false, then get returns Some(false).
    store
        .set_snapshot_lost(&commit_id, false)
        .expect("set false");
    assert_eq!(
        store.get_snapshot_lost(&commit_id).expect("get false"),
        Some(false),
        "get_snapshot_lost should return Some(false) after set_snapshot_lost(false)",
    );

    // 4. Set back to true to confirm the flag is mutable and not stuck.
    store
        .set_snapshot_lost(&commit_id, true)
        .expect("set true again");
    assert_eq!(
        store.get_snapshot_lost(&commit_id).expect("get true again"),
        Some(true),
        "get_snapshot_lost should return Some(true) after setting true again",
    );
}
