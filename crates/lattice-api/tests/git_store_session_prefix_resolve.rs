// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Session ID prefix resolution.
//!
//! Create a session with a 32-char hex ID. Query status with the first 4
//! chars. The API resolves the prefix and returns the full session's status.

use lattice_api::SessionApi;
use lattice_session::{GitSessionGraphStore, SessionGraphStore};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    let _ = lattice_session::test_utils::init_empty_repo(dir);
}

#[test]
fn git_store_session_id_prefix_resolution() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    // Create a session with a known 32-char hex ID directly via the store,
    // since SessionApi::create_session generates a random UUID.
    let full_id = "deadbeefcafebabe1234567890abcdef".to_string();
    assert_eq!(full_id.len(), 32);
    assert!(full_id.chars().all(|c| c.is_ascii_hexdigit()));

    let store = GitSessionGraphStore::new(dir);
    store
        .create_session(&full_id, Some("prefix-resolve-test"), "coder", "main")
        .expect("create_session via store");

    // Wrap the same store directory in SessionApi and query status with the
    // first 4 chars of the session ID.
    let api = SessionApi::git(dir);
    let prefix: String = full_id.chars().take(4).collect();
    assert_eq!(prefix.len(), 4);

    let status = api.status(&prefix).expect("status via prefix");

    // The resolved status must report the full session ID, not the prefix.
    assert_eq!(
        status.session_id, full_id,
        "status should report the full session ID after prefix resolution",
    );
    assert!(
        matches!(&status.head, lattice_session::GraphHead::Branch { name } if name == "main"),
        "HEAD should be on main branch, got: {:?}",
        status.head,
    );
    // No commits yet, so current_commit_id is None and commit_count is 0.
    assert_eq!(status.commit_count, 0);
    assert_eq!(status.current_commit_id, None);

    // Sanity: the full ID also works (exact match path).
    let status_full = api.status(&full_id).expect("status via full id");
    assert_eq!(status_full.session_id, full_id);

    // Sanity: a 3-char prefix is rejected (too short per resolve_id rules).
    let short_prefix: String = full_id.chars().take(3).collect();
    let err = api.status(&short_prefix).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("too short"),
        "3-char prefix should be rejected as too short, got: {msg}",
    );
}
