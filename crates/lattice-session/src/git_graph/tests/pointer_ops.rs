// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Branch, checkout, reset, diff, and candidate ref tests for the Git graph store.

use super::*;
use crate::git_graph::GitSessionGraphStore;
use crate::graph::{GraphHead, SessionGraphStore, TurnCommitStatus, TurnLifecycleEvidence, WorkspaceDisposition};
use crate::SessionError;

#[test]
fn test_branch_create() {
    let dir = temp_dir("branch_create");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-branch", 1, None);
    let head = ids[0].clone();

    let graph_ref = store.branch("sess-branch", "feature", None).expect("branch");
    assert_eq!(graph_ref.name, "feature");
    assert_eq!(graph_ref.commit_id, Some(head.clone()));

    let repo = git2::Repository::open(&dir).expect("open repo");
    let reference = repo
        .find_reference("refs/lattice/sessions/sess-branch/feature")
        .expect("find feature ref");
    assert_eq!(reference.target().map(|o| o.to_string()), Some(head));
}

#[test]
fn test_branch_from_commit() {
    let dir = temp_dir("branch_from_commit");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-from", 2, None);
    let first = ids[0].clone();

    let graph_ref = store
        .branch("sess-from", "old", Some(&first))
        .expect("branch from commit");
    assert_eq!(graph_ref.commit_id, Some(first));
}

#[test]
fn test_branch_duplicate_rejected() {
    let dir = temp_dir("branch_duplicate");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    setup_session_with_commits(&store, "sess-dup", 1, None);
    store.branch("sess-dup", "feature", None).expect("branch once");

    let err = store.branch("sess-dup", "feature", None).unwrap_err();
    assert!(matches!(err, SessionError::Conflict(_)));
}

#[test]
fn test_checkout_branch() {
    let dir = temp_dir("checkout_branch");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    setup_session_with_commits(&store, "sess-checkout", 1, None);
    store.branch("sess-checkout", "feature", None).expect("branch");

    let head = store.checkout("sess-checkout", "feature").expect("checkout");
    assert!(matches!(head, GraphHead::Branch { name } if name == "feature"));

    let status = store.status("sess-checkout").expect("status");
    assert!(matches!(status.head, GraphHead::Branch { name } if name == "feature"));
}

#[test]
fn test_checkout_detached() {
    let dir = temp_dir("checkout_detached");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-detached", 1, None);
    let oid = ids[0].clone();

    let head = store.checkout("sess-detached", &oid).expect("checkout detached");
    assert!(matches!(head, GraphHead::Detached { commit_id } if commit_id == oid));
}

#[test]
fn test_reset() {
    let dir = temp_dir("reset");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-reset", 3, None);
    let first = ids[0].clone();
    let third = ids[2].clone();

    let result = store.reset("sess-reset", &first).expect("reset");
    assert_eq!(result.old_commit_id, Some(third.clone()));
    assert_eq!(result.new_commit_id, first.clone());

    let repo = git2::Repository::open(&dir).expect("open repo");
    let reference = repo
        .find_reference("refs/lattice/sessions/sess-reset/main")
        .expect("find main");
    assert_eq!(reference.target().map(|o| o.to_string()), Some(first));

    // Old commit still reachable via get_turn_commit.
    let old = store.get_turn_commit("sess-reset", &third).expect("get");
    assert!(old.is_some());
}

#[test]
fn test_reset_detached_conflict() {
    let dir = temp_dir("reset_detached");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-reset-detached", 1, None);
    let oid = ids[0].clone();
    store.checkout("sess-reset-detached", &oid).expect("checkout detached");

    let err = store.reset("sess-reset-detached", &oid).unwrap_err();
    assert!(matches!(err, SessionError::Conflict(_)));
}

#[test]
fn test_diff_linear() {
    let dir = temp_dir("diff_linear");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-diff-linear", 3, None);
    let first = ids[0].clone();
    let third = ids[2].clone();

    let diff = store.diff("sess-diff-linear", &first, &third).expect("diff");
    assert_eq!(diff.from_commit_id, first);
    assert_eq!(diff.to_commit_id, third);
    assert_eq!(diff.only_in_from.len(), 0);
    assert_eq!(diff.only_in_to.len(), 2);
}

#[test]
fn test_diff_merge_base() {
    let dir = temp_dir("diff_merge_base");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-diff-merge", 2, None);
    let second = ids[1].clone();

    store.branch("sess-diff-merge", "feature", None).expect("branch");
    store.checkout("sess-diff-merge", "feature").expect("checkout feature");

    let feature_messages = vec![user_msg("Feature work")];
    let feature_input = commit_input(
        "sess-diff-merge",
        vec![second.clone()],
        feature_messages,
        TurnCommitStatus::Completed,
    );
    let feature_commit = store.commit_turn(feature_input).expect("commit feature");

    store.checkout("sess-diff-merge", "main").expect("checkout main");
    let main_messages = vec![user_msg("Main work")];
    let main_input = commit_input(
        "sess-diff-merge",
        vec![second.clone()],
        main_messages,
        TurnCommitStatus::Completed,
    );
    let main_commit = store.commit_turn(main_input).expect("commit main");

    let diff = store
        .diff("sess-diff-merge", &main_commit.commit_id, &feature_commit.commit_id)
        .expect("diff");
    assert!(diff.common_ancestor_id.is_some());
    assert_eq!(diff.only_in_from.len(), 1);
    assert_eq!(diff.only_in_to.len(), 1);
}

#[test]
fn test_diff_no_common_ancestor() {
    let dir = temp_dir("diff_no_common");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);

    store.create_session("sess-a", None, "coder", "main").expect("create a");
    let a_messages = vec![user_msg("A")];
    let a_input = commit_input("sess-a", vec![], a_messages, TurnCommitStatus::Completed);
    let a_commit = store.commit_turn(a_input).expect("commit a");

    store.create_session("sess-b", None, "coder", "main").expect("create b");
    let b_messages = vec![user_msg("B")];
    let b_input = commit_input("sess-b", vec![], b_messages, TurnCommitStatus::Completed);
    let b_commit = store.commit_turn(b_input).expect("commit b");

    let diff = store
        .diff("sess-a", &a_commit.commit_id, &b_commit.commit_id)
        .expect("diff");
    assert!(diff.common_ancestor_id.is_none());
    assert_eq!(diff.only_in_from.len(), 1);
    assert_eq!(diff.only_in_to.len(), 1);
}

#[test]
fn test_candidate_ref_created() {
    let dir = temp_dir("candidate_ref");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    store.create_session("sess-cand", None, "coder", "main").expect("create");

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
    let mut input = commit_input(
        "sess-cand",
        vec![],
        messages,
        TurnCommitStatus::Completed,
    );
    input.lifecycle = Some(lifecycle);
    input.disposition = WorkspaceDisposition::PendingPromotion;
    let commit = store.commit_turn(input).expect("commit");

    let repo = git2::Repository::open(&dir).expect("open repo");
    let git_commit = repo
        .find_commit(git2::Oid::from_str(&commit.commit_id).expect("parse"))
        .expect("find commit");
    let manifest_bytes = crate::git_graph::helpers::read_tree_blob(&repo, &git_commit, "turn/manifest.json")
        .expect("read manifest");
    let manifest: crate::git_graph::helpers::GitSessionManifest =
        serde_json::from_slice(&manifest_bytes).expect("parse manifest");
    let turn_id = manifest.turn_id;
    let cand_ref = format!("refs/lattice/candidates/sess-cand/{turn_id}");
    let reference = repo.find_reference(&cand_ref).expect("find candidate ref");
    assert_eq!(reference.target().map(|o| o.to_string()), Some(commit.commit_id));
}

#[test]
fn test_head_transcript_branched() {
    let dir = temp_dir("transcript_branched");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-trans-branch", 2, None);
    let second = ids[1].clone();

    store.branch("sess-trans-branch", "feature", None).expect("branch");
    store.checkout("sess-trans-branch", "feature").expect("checkout feature");

    let feature_messages = vec![user_msg("Feature only")];
    let feature_input = commit_input(
        "sess-trans-branch",
        vec![second],
        feature_messages,
        TurnCommitStatus::Completed,
    );
    store.commit_turn(feature_input).expect("commit feature");

    let transcript = store
        .head_transcript("sess-trans-branch")
        .expect("head_transcript");
    assert_eq!(transcript.messages.len(), 5);
}

#[test]
fn test_head_transcript_reset() {
    let dir = temp_dir("transcript_reset");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-trans-reset", 3, None);
    let first = ids[0].clone();

    store.reset("sess-trans-reset", &first).expect("reset");
    let transcript = store
        .head_transcript("sess-trans-reset")
        .expect("head_transcript");
    assert_eq!(transcript.messages.len(), 2);
}

#[test]
fn test_branch_reset_checkout_diff() {
    let dir = temp_dir("combined_topology");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    let ids = setup_session_with_commits(&store, "sess-combined", 2, None);
    let second = ids[1].clone();

    let _branch_ref = store.branch("sess-combined", "feature", None).expect("branch");
    store.checkout("sess-combined", "feature").expect("checkout");
    let feature_messages = vec![user_msg("Feature")];
    let feature_input = commit_input(
        "sess-combined",
        vec![second.clone()],
        feature_messages,
        TurnCommitStatus::Completed,
    );
    let feature_commit = store.commit_turn(feature_input).expect("commit feature");

    store.checkout("sess-combined", "main").expect("checkout main");
    let diff = store
        .diff("sess-combined", &second, &feature_commit.commit_id)
        .expect("diff");
    assert_eq!(diff.only_in_to.len(), 1);

    store.reset("sess-combined", &second).expect("reset");
    let status = store.status("sess-combined").expect("status");
    assert_eq!(status.current_commit_id, Some(second));
}
