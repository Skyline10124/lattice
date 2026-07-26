// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! No remote push refspec for refs/lattice/**.
//!
//! The git repository has no remote push refspec that would push
//! `refs/lattice/**` to a remote. Session refs stay local. This test creates
//! session refs under `refs/lattice/**` and verifies the repository config
//! has no remote and no push refspec that references `refs/lattice`.

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
fn git_store_no_remote_push_refspec_for_lattice_refs() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    let store = GitSessionGraphStore::new(dir);

    let session_id = "abcdef0123456789abcdef0123456789".to_string();
    store
        .create_session(&session_id, Some("no-remote-test"), "coder", "main")
        .expect("create_session");

    // Commit a turn so refs/lattice/sessions/<id>/main actually exists.
    let input = TurnCommitInput {
        workspace_history_oid: None,
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

    let repo = gix::open_opts(dir, gix::open::Options::isolated()).expect("open repo");

    // Confirm the session ref actually exists under refs/lattice/**.
    let session_ref = format!("refs/lattice/sessions/{session_id}/main");
    let r = repo
        .find_reference(session_ref.as_str())
        .expect("session ref should exist after commit");
    assert_eq!(
        r.target().try_id().map(|o| o.to_string()),
        Some(commit.commit_id.clone()),
        "session ref must point at the commit",
    );

    // 1. No remotes configured by the store.
    let remotes: Vec<_> = repo.remote_names().into_iter().collect();
    assert!(
        remotes.is_empty(),
        "store must not configure remotes, got: {remotes:?}"
    );

    // 2. Raw config must not mention refs/lattice.
    let config_path = dir.join(".git").join("config");
    if config_path.is_file() {
        let text = std::fs::read_to_string(&config_path).expect("read config");
        assert!(
            !text.contains("refs/lattice"),
            "git config must not reference refs/lattice, got: {text}"
        );
    }

    // 3. No remote-tracking ref (refs/remotes/**) should point at this commit.
    let oid = gix::ObjectId::from_hex(commit.commit_id.as_bytes()).expect("parse oid");
    let platform = repo.references().expect("enumerate references");
    let mut found_remote_tracking = false;
    for reference in platform.all().expect("iterate references") {
        let r = reference.expect("read reference");
        let name = r.name().as_bstr().to_string();
        if name.starts_with("refs/remotes/")
            && r.target().try_id().map(|id| id.to_owned()) == Some(oid)
        {
            found_remote_tracking = true;
        }
    }
    assert!(
        !found_remote_tracking,
        "session commit must not be reachable via any remote-tracking ref",
    );
}
