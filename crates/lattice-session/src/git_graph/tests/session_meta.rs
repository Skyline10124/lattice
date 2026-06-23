// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session metadata tests for the Git graph store.

use super::*;
use crate::git_graph::GitSessionGraphStore;
use crate::SessionError;

#[test]
fn test_create_session() {
    let dir = temp_dir("create_session");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);

    let session = store
        .create_session("sess-create", Some("label"), "coder", "main")
        .expect("create session");
    assert_eq!(session.session_id, "sess-create");
    assert_eq!(session.label, Some("label".to_string()));
    assert_eq!(session.active_agent_name, "coder");
    assert_eq!(session.project_target_branch, Some("main".to_string()));

    let meta_path = dir
        .join(".lattice")
        .join("sessions")
        .join("sess-create")
        .join("meta.json");
    assert!(meta_path.is_file());
}

#[test]
fn test_open_session() {
    let dir = temp_dir("open_session");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    store
        .create_session("sess-open", None, "coder", "main")
        .expect("create");

    let session = store.open_session("sess-open").expect("open");
    assert_eq!(session.session_id, "sess-open");
    assert_eq!(session.active_agent_name, "coder");
}

#[test]
fn test_open_session_not_found() {
    let dir = temp_dir("open_not_found");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);

    let err = store.open_session("sess-missing").unwrap_err();
    assert!(matches!(err, SessionError::NotFound(_)));
}

#[test]
fn test_set_active_agent() {
    let dir = temp_dir("set_active_agent");
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    store
        .create_session("sess-agent", None, "coder", "main")
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
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    store
        .create_session("sess-a", None, "coder", "main")
        .expect("create a");
    store
        .create_session("sess-b", Some("B"), "coder", "dev")
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
    setup_git_repo(&dir);
    let store = GitSessionGraphStore::new(&dir);
    store
        .create_session("sess-empty", None, "coder", "main")
        .expect("create");

    let status = store.status("sess-empty").expect("status");
    assert_eq!(status.session_id, "sess-empty");
    assert_eq!(status.commit_count, 0);
    assert!(status.branches.is_empty());
}
