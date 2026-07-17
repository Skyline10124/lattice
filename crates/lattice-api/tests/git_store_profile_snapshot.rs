// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Get turn commit includes profile_snapshot_json.
//!
//! After committing a turn with `profile_snapshot_json` set to a non-empty
//! JSON string, `get_turn_commit` returns a `TurnCommit` whose
//! `profile_snapshot_json` field contains the full profile JSON (not an empty
//! string).

use std::fs;

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
fn git_store_get_turn_commit_includes_profile_snapshot_json() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    let store = GitSessionGraphStore::new(dir);

    let session_id = "11223344556677889900aabbccddeeff".to_string();
    store
        .create_session(&session_id, Some("profile-snapshot-test"), "coder", "main")
        .expect("create_session");

    // A non-trivial, non-empty profile JSON payload.
    let profile_json = serde_json::json!({
        "agent": "coder",
        "model": "claude-sonnet-4",
        "lancet": {
            "mode": "direct",
            "project_root": "/home/user/project",
            "network": { "allowed_domains": ["example.com"] }
        },
        "tools": ["edit", "shell", "web"]
    })
    .to_string();
    assert!(!profile_json.is_empty(), "test payload must be non-empty");

    let profile_hash = "deadbeefcafebabe".to_string();

    let input = TurnCommitInput {
        session_id: session_id.clone(),
        parent_ids: vec![],
        messages: vec![user_msg("run"), assistant_msg("done")],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: profile_hash.clone(),
        profile_snapshot_json: profile_json.clone(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: "turn-0".to_string(),
        usage: None,
        terminal_reason: None,
    };

    let committed = store.commit_turn(input).expect("commit_turn");
    let commit_id = committed.commit_id.clone();

    // The committed TurnCommit carries the profile hash; the full profile
    // JSON lives in the git tree (turn/profile.json) and is re-read by
    // get_turn_commit, so the committed payload may be empty here.
    assert_eq!(
        committed.profile_snapshot_hash, profile_hash,
        "committed TurnCommit should carry the profile hash",
    );

    // Fetch it back via get_turn_commit and verify the profile JSON is the
    // full payload, not an empty string.
    let fetched = store
        .get_turn_commit(&session_id, &commit_id)
        .expect("get_turn_commit")
        .expect("commit should be found");

    assert_eq!(fetched.commit_id, commit_id);
    assert_eq!(fetched.session_id, session_id);
    assert_ne!(
        fetched.profile_snapshot_json, "",
        "profile_snapshot_json must not be an empty string after get_turn_commit",
    );
    assert_eq!(
        fetched.profile_snapshot_json, profile_json,
        "get_turn_commit must return the full profile JSON, got: {}",
        fetched.profile_snapshot_json,
    );
    assert_eq!(
        fetched.profile_snapshot_hash, profile_hash,
        "get_turn_commit must return the profile hash",
    );

    // Extra: confirm the returned JSON round-trips back to the same value.
    let reparsed: serde_json::Value =
        serde_json::from_str(&fetched.profile_snapshot_json).expect("profile JSON is valid");
    let original: serde_json::Value =
        serde_json::from_str(&profile_json).expect("original profile JSON is valid");
    assert_eq!(
        reparsed, original,
        "round-tripped profile JSON must be semantically equal to the input",
    );
}
