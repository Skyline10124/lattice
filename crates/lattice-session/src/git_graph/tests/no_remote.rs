// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! No-remote-push-refspec verification for the Git graph store.

use std::process::Command;

use super::*;
use crate::git_graph::GitSessionGraphStore;
use crate::graph::{
    SessionGraphStore, TurnCommitStatus, TurnLifecycleEvidence, WorkspaceDisposition,
};

#[test]
fn test_no_remote_push_refspec() {
    let dir = temp_dir("no_remote_refspec");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    store
        .create_session("sess-remote", None, "coder", "main")
        .expect("create session");

    let lifecycle = TurnLifecycleEvidence {
        target_project_ref: "refs/heads/main".to_string(),
        baseline_oid: Some("0".repeat(40)),
        candidate_oid: Some("1".repeat(40)),
        candidate_ref: None,
        verification: None,
        snapshot_ref: None,
        resumes_turn_oid: None,
    };
    let messages = vec![user_msg("Candidate turn")];
    let mut input = commit_input("sess-remote", vec![], messages, TurnCommitStatus::Completed);
    input.lifecycle = Some(lifecycle);
    input.disposition = WorkspaceDisposition::PendingPromotion;
    store.commit_turn(input).expect("commit");

    let output = Command::new("git")
        .args(["remote", "-v"])
        .current_dir(&dir)
        .output()
        .expect("git remote -v");
    assert!(output.status.success(), "git remote -v should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("refs/lattice"),
        "no refs/lattice push refspec should exist, got: {stdout}"
    );

    let config_output = Command::new("git")
        .args(["config", "--get-all", "remote.origin.push"])
        .current_dir(&dir)
        .output()
        .expect("git config");
    let config_stdout = String::from_utf8_lossy(&config_output.stdout);
    assert!(
        !config_stdout.contains("refs/lattice"),
        "no remote.origin.push should reference refs/lattice: {config_stdout}"
    );
}
