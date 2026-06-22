// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Tests for Session project target branch persistence, migration, and
//! independence from Session graph branch operations.

use std::env;
use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;

use crate::SessionError;
use crate::graph::{
    SessionGraphStore, TurnCommitInput, TurnCommitStatus, WorkspaceDisposition,
    derive_commit_message,
};
use crate::sqlite_graph::SqliteSessionGraphStore;
use crate::test_helpers::user_msg;

fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir()
        .join("lattice-session-project-branch-tests")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Create an initial commit so that branch/checkout operations succeed.
fn make_initial_commit(store: &SqliteSessionGraphStore, session_id: &str) {
    let messages = vec![user_msg("initial turn")];
    let input = TurnCommitInput {
        session_id: session_id.to_string(),
        parent_ids: vec![],
        messages: messages.clone(),
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "sonnet".to_string(),
        provider: "anthropic".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: derive_commit_message(&messages),
        usage: None,
        terminal_reason: None,
    };
    store.commit_turn(input).expect("initial commit");
}

#[test]
fn create_persists_project_target_branch() {
    let dir = temp_dir("create_persists");
    let store = SqliteSessionGraphStore::new(&dir);

    let session = store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    assert_eq!(
        session.project_target_branch,
        Some("release/v1.0".to_string())
    );
}

#[test]
fn reopen_preserves_project_target_branch() {
    let dir = temp_dir("reopen_preserves");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    let reopened = store.open_session("sess-1").expect("reopen");
    assert_eq!(
        reopened.project_target_branch,
        Some("release/v1.0".to_string())
    );
}

#[test]
fn status_returns_project_target_branch() {
    let dir = temp_dir("status_returns");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    let status = store.status("sess-1").expect("status");
    assert_eq!(
        status.project_target_branch,
        Some("release/v1.0".to_string())
    );
}

#[test]
fn list_returns_project_target_branch() {
    let dir = temp_dir("list_returns");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    let list = store.list().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].project_target_branch,
        Some("release/v1.0".to_string())
    );
}

#[test]
fn reject_empty_project_target_branch() {
    let dir = temp_dir("reject_empty");
    let store = SqliteSessionGraphStore::new(&dir);

    let result = store.create_session("sess-1", None, "coder", "");
    assert!(
        matches!(result, Err(SessionError::InvalidBranchName)),
        "expected InvalidBranchName, got {result:?}"
    );
}

#[test]
fn reject_invalid_project_target_branch_syntax() {
    let dir = temp_dir("reject_invalid");
    let store = SqliteSessionGraphStore::new(&dir);

    // Starts with '-'
    let result = store.create_session("sess-1", None, "coder", "-invalid");
    assert!(
        matches!(result, Err(SessionError::InvalidBranchName)),
        "expected InvalidBranchName for '-invalid', got {result:?}"
    );

    // Contains '..'
    let result = store.create_session("sess-2", None, "coder", "foo..bar");
    assert!(
        matches!(result, Err(SessionError::InvalidBranchName)),
        "expected InvalidBranchName for 'foo..bar', got {result:?}"
    );

    // Starts with '/'
    let result = store.create_session("sess-3", None, "coder", "/leading-slash");
    assert!(
        matches!(result, Err(SessionError::InvalidBranchName)),
        "expected InvalidBranchName for '/leading-slash', got {result:?}"
    );
}

#[test]
fn legacy_schema_returns_none_project_target_branch() {
    let dir = temp_dir("legacy_schema");
    fs::create_dir_all(&dir).expect("create dir");
    let db_path = dir.join("session_v12.db");

    // Create a pre-Phase-36 database without the project_target_branch column.
    {
        let conn = Connection::open(&db_path).expect("open");
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS graph_sessions (
                session_id   TEXT PRIMARY KEY,
                label        TEXT,
                active_agent_name TEXT NOT NULL,
                head_ref     TEXT,
                head_commit_id TEXT,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS graph_refs (
                session_id TEXT NOT NULL,
                name       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                commit_id  TEXT,
                active_agent_name TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (session_id, kind, name),
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            CREATE TABLE IF NOT EXISTS graph_commits (
                commit_id            TEXT PRIMARY KEY,
                session_id           TEXT NOT NULL,
                parents_json         TEXT NOT NULL,
                message_ids_json     TEXT NOT NULL,
                agent_name           TEXT NOT NULL,
                profile_snapshot_hash TEXT NOT NULL,
                profile_snapshot_json TEXT NOT NULL,
                model                TEXT NOT NULL,
                provider             TEXT NOT NULL,
                status               TEXT NOT NULL,
                commit_message       TEXT NOT NULL,
                usage_json           TEXT,
                terminal_reason      TEXT,
                created_at           TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            CREATE TABLE IF NOT EXISTS graph_messages (
                message_id     TEXT PRIMARY KEY,
                session_id     TEXT NOT NULL,
                role           TEXT NOT NULL,
                content        TEXT NOT NULL,
                reasoning_content TEXT,
                tool_call_id   TEXT,
                name           TEXT,
                tool_calls_json TEXT NOT NULL DEFAULT '[]',
                created_at     TEXT NOT NULL,
                ordinal        INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            INSERT INTO graph_sessions (session_id, label, active_agent_name, head_ref, head_commit_id, created_at, updated_at)
                VALUES ('legacy-1', NULL, 'coder', 'main', NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
            ",
        )
        .expect("create legacy schema");
    }

    // Open through the current store — project_target_branch should be None.
    let store = SqliteSessionGraphStore::new(&dir);
    let session = store.open_session("legacy-1").expect("open legacy");
    assert!(
        session.project_target_branch.is_none(),
        "legacy rows must not invent a project target branch"
    );

    let status = store.status("legacy-1").expect("status legacy");
    assert!(status.project_target_branch.is_none());

    let list = store.list().expect("list legacy");
    assert_eq!(list.len(), 1);
    assert!(list[0].project_target_branch.is_none());
}

#[test]
fn checkout_does_not_alter_project_target_branch() {
    let dir = temp_dir("checkout_independence");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    // Need a commit before branching.
    make_initial_commit(&store, "sess-1");

    // Create a new branch and check out to it.
    store.branch("sess-1", "feature-x", None).expect("branch");
    store.checkout("sess-1", "feature-x").expect("checkout");

    // Project target branch must remain unchanged.
    let reopened = store.open_session("sess-1").expect("reopen");
    assert_eq!(
        reopened.project_target_branch,
        Some("release/v1.0".to_string())
    );

    let status = store.status("sess-1").expect("status");
    assert_eq!(
        status.project_target_branch,
        Some("release/v1.0".to_string())
    );
}

#[test]
fn branch_creation_does_not_alter_project_target_branch() {
    let dir = temp_dir("branch_independence");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    // Need a commit before branching.
    make_initial_commit(&store, "sess-1");

    // Create multiple branches.
    store.branch("sess-1", "dev", None).expect("branch dev");
    store
        .branch("sess-1", "hotfix", None)
        .expect("branch hotfix");

    let reopened = store.open_session("sess-1").expect("reopen");
    assert_eq!(
        reopened.project_target_branch,
        Some("release/v1.0".to_string())
    );
}

#[test]
fn repeated_open_status_list_agree_on_project_target_branch() {
    let dir = temp_dir("consistency");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    // Open multiple times.
    let o1 = store.open_session("sess-1").expect("open 1");
    let o2 = store.open_session("sess-1").expect("open 2");
    assert_eq!(o1.project_target_branch, o2.project_target_branch);

    // Status multiple times.
    let s1 = store.status("sess-1").expect("status 1");
    let s2 = store.status("sess-1").expect("status 2");
    assert_eq!(s1.project_target_branch, s2.project_target_branch);

    // List.
    let l = store.list().expect("list");
    assert_eq!(l[0].project_target_branch, o1.project_target_branch);
}

#[test]
fn reset_does_not_alter_project_target_branch() {
    let dir = temp_dir("reset_independence");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    // Create a commit so we have something to reset to.
    make_initial_commit(&store, "sess-1");

    // Reset main to the commit.
    store.reset("sess-1", "main").expect("reset");

    // Project target branch must remain unchanged.
    let reopened = store.open_session("sess-1").expect("reopen");
    assert_eq!(
        reopened.project_target_branch,
        Some("release/v1.0".to_string())
    );
}

// === Task 2: Migration and Session-branch independence proofs ===

#[test]
fn legacy_schema_stays_none_after_full_store_migration() {
    let dir = temp_dir("legacy_full_migration");
    fs::create_dir_all(&dir).expect("create dir");
    let db_path = dir.join("session_v12.db");

    // Create a pre-Phase-36 database without the project_target_branch column.
    {
        let conn = Connection::open(&db_path).expect("open");
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS graph_sessions (
                session_id   TEXT PRIMARY KEY,
                label        TEXT,
                active_agent_name TEXT NOT NULL,
                head_ref     TEXT,
                head_commit_id TEXT,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS graph_refs (
                session_id TEXT NOT NULL,
                name       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                commit_id  TEXT,
                active_agent_name TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (session_id, kind, name),
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            CREATE TABLE IF NOT EXISTS graph_commits (
                commit_id            TEXT PRIMARY KEY,
                session_id           TEXT NOT NULL,
                parents_json         TEXT NOT NULL,
                message_ids_json     TEXT NOT NULL,
                agent_name           TEXT NOT NULL,
                profile_snapshot_hash TEXT NOT NULL,
                profile_snapshot_json TEXT NOT NULL,
                model                TEXT NOT NULL,
                provider             TEXT NOT NULL,
                status               TEXT NOT NULL,
                commit_message       TEXT NOT NULL,
                usage_json           TEXT,
                terminal_reason      TEXT,
                created_at           TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            CREATE TABLE IF NOT EXISTS graph_messages (
                message_id     TEXT PRIMARY KEY,
                session_id     TEXT NOT NULL,
                role           TEXT NOT NULL,
                content        TEXT NOT NULL,
                reasoning_content TEXT,
                tool_call_id   TEXT,
                name           TEXT,
                tool_calls_json TEXT NOT NULL DEFAULT '[]',
                created_at     TEXT NOT NULL,
                ordinal        INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            INSERT INTO graph_sessions (session_id, label, active_agent_name, head_ref, head_commit_id, created_at, updated_at)
                VALUES ('legacy-migrated', NULL, 'coder', 'main', NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
            INSERT INTO graph_refs (session_id, name, kind, commit_id, active_agent_name, updated_at)
                VALUES ('legacy-migrated', 'main', 'branch', NULL, 'coder', '2026-01-01T00:00:00Z');
            ",
        )
        .expect("create legacy schema");
    }

    // Open through the current store (triggers migration), then read multiple times.
    let store = SqliteSessionGraphStore::new(&dir);

    // First open after migration.
    let s1 = store.open_session("legacy-migrated").expect("open 1");
    assert!(
        s1.project_target_branch.is_none(),
        "migrated legacy rows must remain None, not receive a guessed value"
    );

    // Status after migration.
    let status = store.status("legacy-migrated").expect("status");
    assert!(status.project_target_branch.is_none());

    // List after migration.
    let list = store.list().expect("list");
    let legacy_row = list
        .iter()
        .find(|s| s.session_id == "legacy-migrated")
        .unwrap();
    assert!(legacy_row.project_target_branch.is_none());

    // Reopen again — must still be None.
    let s2 = store.open_session("legacy-migrated").expect("open 2");
    assert!(s2.project_target_branch.is_none());
}

#[test]
fn set_active_agent_preserves_project_target_branch() {
    let dir = temp_dir("set_active_agent");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    // Need a commit for set_active_agent (which reads head_ref).
    make_initial_commit(&store, "sess-1");

    // Change the active agent.
    store
        .set_active_agent("sess-1", "reviewer")
        .expect("set active agent");

    // Project target branch must remain unchanged.
    let reopened = store.open_session("sess-1").expect("reopen");
    assert_eq!(
        reopened.project_target_branch,
        Some("release/v1.0".to_string())
    );
    assert_eq!(reopened.active_agent_name, "reviewer");
}

#[test]
fn multiple_sessions_have_independent_project_targets() {
    let dir = temp_dir("independent_targets");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-a", None, "coder", "release/v1.0")
        .expect("create a");
    store
        .create_session("sess-b", None, "coder", "develop")
        .expect("create b");

    let a = store.open_session("sess-a").expect("open a");
    let b = store.open_session("sess-b").expect("open b");

    assert_eq!(a.project_target_branch, Some("release/v1.0".to_string()));
    assert_eq!(b.project_target_branch, Some("develop".to_string()));

    // List should show both independently.
    let list = store.list().expect("list");
    let a_row = list.iter().find(|s| s.session_id == "sess-a").unwrap();
    let b_row = list.iter().find(|s| s.session_id == "sess-b").unwrap();
    assert_eq!(
        a_row.project_target_branch,
        Some("release/v1.0".to_string())
    );
    assert_eq!(b_row.project_target_branch, Some("develop".to_string()));
}

#[test]
fn head_ref_change_does_not_alter_project_target_branch() {
    let dir = temp_dir("head_ref_independence");
    let store = SqliteSessionGraphStore::new(&dir);

    store
        .create_session("sess-1", None, "coder", "release/v1.0")
        .expect("create");

    // Need a commit for branch/checkout.
    make_initial_commit(&store, "sess-1");

    // Create a new branch, check it out (changes head_ref), and verify.
    store.branch("sess-1", "experiment", None).expect("branch");
    store.checkout("sess-1", "experiment").expect("checkout");

    // The head_ref is now "experiment" but project target must stay "release/v1.0".
    let status = store.status("sess-1").expect("status");
    assert_eq!(
        status.head,
        crate::graph::GraphHead::Branch {
            name: "experiment".to_string()
        }
    );
    assert_eq!(
        status.project_target_branch,
        Some("release/v1.0".to_string()),
        "project target must be independent of head_ref"
    );

    // Also verify via raw SQL that the column value is stored correctly.
    let conn = Connection::open(store.db_path()).expect("open raw");
    let (head_ref, project_target): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT head_ref, project_target_branch FROM graph_sessions WHERE session_id='sess-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("raw query");
    assert_eq!(head_ref, Some("experiment".to_string()));
    assert_eq!(project_target, Some("release/v1.0".to_string()));
}

#[test]
fn legacy_session_set_active_agent_keeps_none() {
    let dir = temp_dir("legacy_set_agent");
    fs::create_dir_all(&dir).expect("create dir");
    let db_path = dir.join("session_v12.db");

    // Create legacy database.
    {
        let conn = Connection::open(&db_path).expect("open");
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS graph_sessions (
                session_id   TEXT PRIMARY KEY,
                label        TEXT,
                active_agent_name TEXT NOT NULL,
                head_ref     TEXT,
                head_commit_id TEXT,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS graph_refs (
                session_id TEXT NOT NULL,
                name       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                commit_id  TEXT,
                active_agent_name TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (session_id, kind, name),
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            CREATE TABLE IF NOT EXISTS graph_commits (
                commit_id            TEXT PRIMARY KEY,
                session_id           TEXT NOT NULL,
                parents_json         TEXT NOT NULL,
                message_ids_json     TEXT NOT NULL,
                agent_name           TEXT NOT NULL,
                profile_snapshot_hash TEXT NOT NULL,
                profile_snapshot_json TEXT NOT NULL,
                model                TEXT NOT NULL,
                provider             TEXT NOT NULL,
                status               TEXT NOT NULL,
                commit_message       TEXT NOT NULL,
                usage_json           TEXT,
                terminal_reason      TEXT,
                created_at           TEXT NOT NULL,
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            CREATE TABLE IF NOT EXISTS graph_messages (
                message_id     TEXT PRIMARY KEY,
                session_id     TEXT NOT NULL,
                role           TEXT NOT NULL,
                content        TEXT NOT NULL,
                reasoning_content TEXT,
                tool_call_id   TEXT,
                name           TEXT,
                tool_calls_json TEXT NOT NULL DEFAULT '[]',
                created_at     TEXT NOT NULL,
                ordinal        INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES graph_sessions(session_id)
            );
            INSERT INTO graph_sessions (session_id, label, active_agent_name, head_ref, head_commit_id, created_at, updated_at)
                VALUES ('legacy-agent', NULL, 'coder', 'main', NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
            INSERT INTO graph_refs (session_id, name, kind, commit_id, active_agent_name, updated_at)
                VALUES ('legacy-agent', 'main', 'branch', NULL, 'coder', '2026-01-01T00:00:00Z');
            ",
        )
        .expect("create legacy schema");
    }

    // Open through current store and perform set_active_agent.
    let store = SqliteSessionGraphStore::new(&dir);
    store
        .set_active_agent("legacy-agent", "new-agent")
        .expect("set active agent");

    // Project target must remain None — no fallback from head_ref.
    let session = store.open_session("legacy-agent").expect("open");
    assert!(
        session.project_target_branch.is_none(),
        "legacy session must remain None even after set_active_agent"
    );
}
