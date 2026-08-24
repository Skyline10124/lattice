// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session metadata tests for the Git graph store.

use super::*;
use crate::SessionError;
use crate::git_graph::GitSessionGraphStore;

#[test]
fn test_create_session() {
    let dir = temp_dir("create_session");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");

    let session = store
        .create_session(
            "sess-create",
            Some("label"),
            "coder",
            "main",
            &test_project_key(),
        )
        .expect("create session");
    assert_eq!(session.session_id, "sess-create");
    assert_eq!(session.label, Some("label".to_string()));
    assert_eq!(session.active_agent_name, "coder");
    assert_eq!(session.project_target_branch, Some("main".to_string()));

    // #143: no sidecar files; the SessionState is an immutable commit behind
    // the session's state ref.
    assert_eq!(session.project_store_key, test_project_key().to_string());
    assert!(!dir.path().join(".lattice").exists());
}

#[test]
fn test_open_session() {
    let dir = temp_dir("open_session");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");
    store
        .create_session("sess-open", None, "coder", "main", &test_project_key())
        .expect("create");

    let session = store.open_session("sess-open").expect("open");
    assert_eq!(session.session_id, "sess-open");
    assert_eq!(session.active_agent_name, "coder");
}

#[test]
fn test_open_session_not_found() {
    let dir = temp_dir("open_not_found");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");

    let err = store.open_session("sess-missing").unwrap_err();
    assert!(matches!(err, SessionError::NotFound(_)));
}

#[test]
fn test_set_active_agent() {
    let dir = temp_dir("set_active_agent");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");
    store
        .create_session("sess-agent", None, "coder", "main", &test_project_key())
        .expect("create");

    let session = store
        .set_active_agent("sess-agent", "reviewer")
        .expect("set agent");
    assert_eq!(session.active_agent_name, "reviewer");

    let reopened = store.open_session("sess-agent").expect("reopen");
    assert_eq!(reopened.active_agent_name, "reviewer");
}

#[test]
fn test_list_sessions() {
    let dir = temp_dir("list_sessions");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");
    store
        .create_session("sess-a", None, "coder", "main", &test_project_key())
        .expect("create a");
    store
        .create_session("sess-b", Some("B"), "coder", "dev", &test_project_key())
        .expect("create b");

    let list = store.list().expect("list");
    assert_eq!(list.len(), 2);
    let ids: Vec<_> = list.iter().map(|s| s.session_id.as_str()).collect();
    assert!(ids.contains(&"sess-a"));
    assert!(ids.contains(&"sess-b"));
}

#[test]
fn test_status_empty_session() {
    let dir = temp_dir("status_empty");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");
    store
        .create_session("sess-empty", None, "coder", "main", &test_project_key())
        .expect("create");

    let status = store.status("sess-empty").expect("status");
    assert_eq!(status.session_id, "sess-empty");
    assert_eq!(status.commit_count, 0);
    assert!(status.branches.is_empty());
}

#[test]
fn create_builds_a_fresh_kernel_store() {
    // `create` is the sole initialization path (#143): it builds the store on
    // the kernel and returns a usable handle. Strict-open fail-closed coverage
    // lives in `state_and_locks.rs::open_absent_store_fails_closed_without_creating_it`.
    let dir = temp_dir("fresh_kernel_store");
    // Intentionally do NOT call setup_git_repo — dir does not exist yet.
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");

    store
        .create_session("sess-auto-init", None, "coder", "main", &test_project_key())
        .expect("create session on fresh kernel store");

    // The session is usable: a follow-up open succeeds and a commit lands.
    let session = store.open_session("sess-auto-init").expect("open");
    assert_eq!(session.session_id, "sess-auto-init");

    let messages = vec![
        crate::test_helpers::user_msg("hello"),
        crate::test_helpers::assistant_msg("hi"),
    ];
    let input = super::commit_input("sess-auto-init", Vec::new(), messages, ());
    let commit = store.test_commit_turn(input).expect("commit turn");
    assert!(!commit.commit_id.is_empty());
}
