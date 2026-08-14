// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! No-remote-push-refspec verification for the Git graph store.

use super::*;
use crate::git_graph::GitSessionGraphStore;
use crate::graph::{
    SessionGraphStore, TurnCommitStatus, TurnLifecycleEvidence, WorkspaceDisposition,
};

#[test]
fn test_no_remote_push_refspec() {
    let dir = temp_dir("no_remote_refspec");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");
    store
        .create_session("sess-remote", None, "coder", "main", &test_project_key())
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

    // Inspect the repository config via gix — there must be no remote and no
    // push refspec that mentions refs/lattice.
    let repo = gix::open_opts(
        dir.path().join("sessions.git"),
        gix::open::Options::isolated(),
    )
    .expect("open repo");
    let remotes: Vec<_> = repo.remote_names().into_iter().collect();
    assert!(
        remotes.is_empty(),
        "store must not configure remotes, got: {remotes:?}"
    );

    // Also scan raw config for any push refspec mentioning refs/lattice.
    let config_path = dir.path().join(".git").join("config");
    if config_path.is_file() {
        let text = std::fs::read_to_string(&config_path).expect("read git config");
        assert!(
            !text.contains("refs/lattice"),
            "git config must not reference refs/lattice, got: {text}"
        );
    }
}
