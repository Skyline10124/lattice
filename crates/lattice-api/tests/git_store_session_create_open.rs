// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Git store session creation and open.
//!
//! Uses `SessionApi::git(dir)` backed by `GitSessionGraphStore` to create a
//! session with a label and active agent, then reopens the same session by ID
//! and verifies the reopened session has the same `session_id`, `label`, and
//! `active_agent_name`.

use lattice_api::{CreateSessionRequest, OpenSessionRequest, SessionApi};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    let _ = lattice_session::test_utils::init_empty_repo(dir);
}

#[test]
fn git_store_create_and_reopen_preserves_identity() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    let api = SessionApi::git(dir);

    let created = api
        .create_session(CreateSessionRequest {
            label: Some("git-test-session".into()),
            active_agent: "coder".into(),
            project_branch: "main".to_string(),
        })
        .expect("create_session");

    let session_id = created.session_id.clone();
    assert!(!session_id.is_empty(), "session_id should be non-empty");
    assert_eq!(created.label.as_deref(), Some("git-test-session"));
    assert_eq!(created.active_agent_name, "coder");

    // Reopen the same session by ID using a fresh SessionApi handle over the
    // same Git-backed store directory.
    let reopened_api = SessionApi::git(dir);
    let reopened = reopened_api
        .open_session(OpenSessionRequest {
            id: session_id.clone(),
        })
        .expect("open_session");

    assert_eq!(reopened.session_id, created.session_id);
    assert_eq!(reopened.label, created.label);
    assert_eq!(reopened.active_agent_name, created.active_agent_name);
}
