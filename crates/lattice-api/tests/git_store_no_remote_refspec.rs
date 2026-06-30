// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! No remote push refspec for refs/lattice/**.
//!
//! The git repository has no remote push refspec that would push
//! `refs/lattice/**` to a remote. Session refs stay local. This test sets up
//! a real remote with a normal push refspec, creates session refs under
//! `refs/lattice/**`, and verifies none of the configured push refspecs
//! reference `refs/lattice`.

use std::fs;
use std::process::Command;

use lattice_session::test_helpers::{assistant_msg, user_msg};
use lattice_session::{
    GitSessionGraphStore, SessionGraphStore, TurnCommitInput, TurnCommitStatus,
    WorkspaceDisposition,
};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    fs::create_dir_all(dir).expect("create dir");
    git2::Repository::init(dir).expect("init repo");
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|_| panic!("git {:?} should run", args));
    assert!(
        output.status.success(),
        "git {:?} failed: stderr={}",
        args,
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[test]
fn git_store_no_remote_push_refspec_for_lattice_refs() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    // Set up a dummy remote with a normal push refspec (refs/heads/*). This
    // ensures the test is meaningful: there IS a remote and a push refspec,
    // but it must not include refs/lattice/**.
    let remote_url = "/tmp/lattice-dummy-remote";
    git(dir, &["remote", "add", "origin", remote_url]);
    git(
        dir,
        &[
            "config",
            "--add",
            "remote.origin.push",
            "refs/heads/*:refs/heads/*",
        ],
    );

    let store = GitSessionGraphStore::new(dir);

    let session_id = "abcdef0123456789abcdef0123456789".to_string();
    store
        .create_session(&session_id, Some("no-remote-test"), "coder", "main")
        .expect("create_session");

    // Commit a turn so refs/lattice/sessions/<id>/main actually exists.
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

    // Confirm the session ref actually exists under refs/lattice/**.
    let session_ref = format!("refs/lattice/sessions/{session_id}/main");
    let ref_value = git(dir, &["for-each-ref", "--format=%(refname)", &session_ref]);
    assert!(
        ref_value.trim() == session_ref,
        "session ref {session_ref} should exist after commit, got: {ref_value:?}",
    );

    // 1. `git remote -v` must not mention refs/lattice in any line.
    let remote_v = git(dir, &["remote", "-v"]);
    assert!(
        !remote_v.contains("refs/lattice"),
        "no remote line should reference refs/lattice, got: {remote_v}",
    );

    // 2. No configured push refspec for any remote references refs/lattice.
    //    Enumerate all remotes and check each one's push refspecs.
    let remotes = git(dir, &["remote"]);
    for remote in remotes.lines().map(|l| l.trim()).filter(|l| !l.is_empty()) {
        let push_key = format!("remote.{remote}.push");
        let push_refspecs = git(dir, &["config", "--get-all", &push_key]);
        for line in push_refspecs.lines() {
            assert!(
                !line.contains("refs/lattice"),
                "push refspec for remote '{remote}' must not reference refs/lattice: {line}",
            );
        }
    }

    // 3. Explicitly check remote.origin.push does not contain refs/lattice.
    let origin_push = git(dir, &["config", "--get-all", "remote.origin.push"]);
    assert!(
        !origin_push.contains("refs/lattice"),
        "remote.origin.push must not reference refs/lattice: {origin_push}",
    );

    // 4. Verify via git2 that no reference name under refs/lattice/** has a
    //    push refspec configured. Also confirm the commit is reachable only
    //    via the local session ref, not via any remote-tracking branch.
    let repo = git2::Repository::open(dir).expect("open repo");
    let oid = git2::Oid::from_str(&commit.commit_id).expect("parse oid");

    // No remote-tracking ref (refs/remotes/**) should point at this commit.
    let mut found_remote_tracking = false;
    for reference in repo.references().expect("enumerate references") {
        let r = reference.expect("read reference");
        if let Some(name) = r.name()
            && name.starts_with("refs/remotes/")
            && r.target() == Some(oid)
        {
            found_remote_tracking = true;
        }
    }
    assert!(
        !found_remote_tracking,
        "session commit must not be reachable via any remote-tracking ref",
    );
}
