// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn commit tests for the Git graph store.

use super::*;
use crate::git_graph::GitSessionGraphStore;
use crate::graph::TurnCommitStatus;

#[test]
fn test_commit_turn_tree_structure() {
    let dir = temp_dir("tree_structure");
    setup_git_repo(dir.path());
    let store = GitSessionGraphStore::new(dir.path());
    store
        .create_session("sess-tree", None, "coder", "main")
        .expect("create");

    let messages = vec![user_msg("Hello")];
    let input = commit_input("sess-tree", vec![], messages, TurnCommitStatus::Completed);
    let commit = store.commit_turn(input).expect("commit");
    assert_eq!(commit.commit_id.len(), 40);

    let repo = gix::open_opts(dir.path(), gix::open::Options::isolated()).expect("open repo");
    let oid = gix::ObjectId::from_hex(commit.commit_id.as_bytes()).expect("parse oid");
    let git_commit = repo.find_commit(oid).expect("find commit");
    let tree = git_commit.tree().expect("tree");
    let turn = tree
        .lookup_entry_by_path("turn")
        .expect("lookup turn")
        .expect("turn entry");
    let turn_tree = turn
        .object()
        .expect("turn object")
        .try_into_tree()
        .expect("turn tree");
    for name in [
        "manifest.json",
        "messages.jsonl",
        "agent.json",
        "profile.json",
        "audit.json",
    ] {
        assert!(
            turn_tree.find_entry(name).is_some(),
            "missing {name} in turn tree"
        );
    }
}

#[test]
fn test_commit_turn_linear() {
    let dir = temp_dir("commit_linear");
    setup_git_repo(dir.path());
    let store = GitSessionGraphStore::new(dir.path());
    store
        .create_session("sess-linear", None, "coder", "main")
        .expect("create");

    let messages = vec![user_msg("Hello")];
    let input = commit_input("sess-linear", vec![], messages, TurnCommitStatus::Completed);
    let commit = store.commit_turn(input).expect("commit");
    assert_eq!(commit.commit_id.len(), 40);
    assert!(commit.parent_ids.is_empty());
    assert_eq!(commit.message_ids.len(), 1);
    assert!(commit.message_ids[0].starts_with(&commit.commit_id));
}

#[test]
fn test_commit_turn_with_parent() {
    let dir = temp_dir("commit_with_parent");
    setup_git_repo(dir.path());
    let store = GitSessionGraphStore::new(dir.path());
    let commit_ids = setup_session_with_commits(&store, "sess-parent", 1, None);
    let first_id = commit_ids[0].clone();

    let messages = vec![user_msg("Second")];
    let input = commit_input(
        "sess-parent",
        vec![first_id.clone()],
        messages,
        TurnCommitStatus::Completed,
    );
    let commit = store.commit_turn(input).expect("commit");
    assert_eq!(commit.parent_ids, vec![first_id]);
}

#[test]
fn test_get_turn_commit() {
    let dir = temp_dir("get_turn_commit");
    setup_git_repo(dir.path());
    let store = GitSessionGraphStore::new(dir.path());
    let commit_ids = setup_session_with_commits(&store, "sess-get", 1, None);
    let first_id = commit_ids[0].clone();

    let found = store
        .get_turn_commit("sess-get", &first_id)
        .expect("get")
        .expect("exists");
    assert_eq!(found.commit_id, first_id);
    assert_eq!(found.session_id, "sess-get");
}

#[test]
fn test_get_turn_commit_not_found() {
    let dir = temp_dir("get_not_found");
    setup_git_repo(dir.path());
    let store = GitSessionGraphStore::new(dir.path());
    store
        .create_session("sess-not-found", None, "coder", "main")
        .expect("create");

    let found = store
        .get_turn_commit("sess-not-found", "0000000000000000000000000000000000000000")
        .expect("get");
    assert!(found.is_none());
}
