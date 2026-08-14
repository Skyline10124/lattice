// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Git store session creation and open.
//!
//! Uses `SessionApi::git_create(dir)` backed by `GitSessionGraphStore` to create a
//! session with a label and active agent, then reopens the same session by ID
//! and verifies the reopened session has the same `session_id`, `label`, and
//! `active_agent_name`.

use lattice_api::{CreateSessionRequest, OpenSessionRequest, SessionApi};
use tempfile::TempDir;

fn test_store_key() -> lattice_session::ProjectStoreKey {
    lattice_session::ProjectStoreKey::from_canonical_path_bytes(b"/lattice-api-tests/project")
}

#[test]
fn git_store_create_and_reopen_preserves_identity() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    let api = SessionApi::git_create(dir.join("sessions.git")).expect("create store");

    let created = api
        .create_session(CreateSessionRequest {
            label: Some("git-test-session".into()),
            active_agent: "coder".into(),
            project_branch: "main".to_string(),
            project_store_key: test_store_key().to_string(),
        })
        .expect("create_session");

    let session_id = created.session_id.clone();
    assert!(!session_id.is_empty(), "session_id should be non-empty");
    assert_eq!(created.label.as_deref(), Some("git-test-session"));
    assert_eq!(created.active_agent_name, "coder");

    // Reopen the same session by ID using a fresh SessionApi handle over the
    // same Git-backed store directory.
    let reopened_api = SessionApi::git_open(dir.join("sessions.git")).expect("open store");
    let reopened = reopened_api
        .open_session(OpenSessionRequest {
            id: session_id.clone(),
        })
        .expect("open_session");

    assert_eq!(reopened.session_id, created.session_id);
    assert_eq!(reopened.label, created.label);
    assert_eq!(reopened.active_agent_name, created.active_agent_name);
}
