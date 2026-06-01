//! Tests for `commit_turn`, schema shape, and basic session/commit lifecycle.

use std::env;
use std::fs;
use std::path::PathBuf;

use rusqlite::{params, Connection};

use crate::graph::{
    derive_commit_message, GraphMessage, SessionGraphStore, TurnCommitInput, TurnCommitStatus,
};
use crate::sqlite_graph::SqliteSessionGraphStore;
use crate::test_helpers::{assistant_msg, tool_msg, user_msg};
use crate::SessionError;

fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir()
        .join("lattice-session-sqlite-graph-tests")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

#[test]
fn store_creates_v12_tables_on_open() {
    let dir = temp_dir("creates_tables");
    let store = SqliteSessionGraphStore::new(dir.clone());

    // Trigger lazy connection init (creates tables).
    drop(store.conn().expect("init"));

    // Verify the four graph tables exist via a raw connection.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    for table in &[
        "graph_sessions",
        "graph_refs",
        "graph_commits",
        "graph_messages",
    ] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get(0),
            )
            .expect("query should succeed");
        assert_eq!(count, 1, "table '{table}' should exist");
    }

    // Verify legacy tables do NOT exist.
    for table in &["objects", "commits", "refs", "meta"] {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get(0),
            )
            .expect("query should succeed");
        assert_eq!(count, 0, "legacy table '{table}' should NOT exist");
    }
}

#[test]
fn graph_refs_has_commit_id_column() {
    let dir = temp_dir("refs_has_commit_id");
    let store = SqliteSessionGraphStore::new(dir);
    drop(store.conn().expect("init"));
    let conn = Connection::open(store.db_path()).expect("raw conn");

    // PRAGMA table_info returns columns for graph_refs.
    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare("PRAGMA table_info(graph_refs)")
            .expect("pragma should succeed");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query_map should succeed");
        rows.filter_map(|r| r.ok()).collect()
    };

    assert!(
        columns.contains(&"commit_id".to_string()),
        "graph_refs must have commit_id column, found: {columns:?}"
    );
}

fn commit_input(
    session_id: &str,
    parent_ids: Vec<String>,
    messages: Vec<GraphMessage>,
    status: TurnCommitStatus,
) -> TurnCommitInput {
    TurnCommitInput {
        session_id: session_id.to_string(),
        parent_ids,
        messages: messages.clone(),
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status,
        commit_message: derive_commit_message(&messages),
        usage: None,
        terminal_reason: None,
    }
}

#[test]
fn graph_refs_does_not_have_session_id_as_branch_pointer_value() {
    let dir = temp_dir("refs_no_session_id_pointer");
    let store = SqliteSessionGraphStore::new(dir);
    drop(store.conn().expect("init"));
    let conn = Connection::open(store.db_path()).expect("raw conn");

    // The commit_id column exists (tested above). There should be no
    // separate "target_session_id" or similar column used as the branch
    // pointer value. The branch pointer IS commit_id.
    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare("PRAGMA table_info(graph_refs)")
            .expect("pragma should succeed");
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query_map should succeed");
        rows.filter_map(|r| r.ok()).collect()
    };

    // Ensure no column named "target_session_id" or "target" exists.
    assert!(
        !columns
            .iter()
            .any(|c| c == "target_session_id" || c == "target"),
        "graph_refs should not have a session-id-based pointer column, found: {columns:?}"
    );
}

#[test]
fn create_session_does_not_create_commit() {
    let dir = temp_dir("create_no_commit");
    let store = SqliteSessionGraphStore::new(dir);

    let session = store
        .create_session("sess-1", Some("test"), "coder")
        .expect("create_session should succeed");

    assert_eq!(session.session_id, "sess-1");
    assert_eq!(session.label, Some("test".to_string()));
    assert_eq!(session.active_agent_name, "coder");

    // graph_commits count must be 0 immediately after create.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let commit_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM graph_commits", [], |row| row.get(0))
        .expect("count query");
    assert_eq!(commit_count, 0, "create_session must not create any commit");
}

#[test]
fn open_session_returns_existing_session() {
    let dir = temp_dir("open_existing");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", Some("test"), "coder")
        .expect("create");

    let opened = store.open_session("sess-1").expect("open_session");
    assert_eq!(opened.session_id, "sess-1");
    assert_eq!(opened.label, Some("test".to_string()));
}

#[test]
fn open_session_returns_not_found_for_missing() {
    let dir = temp_dir("open_not_found");
    let store = SqliteSessionGraphStore::new(dir);

    let result = store.open_session("nonexistent");
    assert!(
        result.is_err(),
        "open_session should fail for missing session"
    );
}

#[test]
fn commit_turn_writes_messages_commit_and_branch_ref_atomically() {
    let dir = temp_dir("commit_atomic");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, "coder")
        .expect("create");

    let messages = vec![user_msg("Hello"), assistant_msg("Hi there!")];
    let input = commit_input("sess-1", vec![], messages, TurnCommitStatus::Completed);

    let commit = store.commit_turn(input).expect("commit_turn");

    // Commit ID is a 64-char hex string.
    assert_eq!(commit.commit_id.len(), 64);
    assert!(commit.commit_id.chars().all(|c| c.is_ascii_hexdigit()));

    // Message IDs are populated.
    assert_eq!(commit.message_ids.len(), 2);

    let conn = Connection::open(store.db_path()).expect("raw conn");

    // Verify message rows exist.
    let msg_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_messages WHERE session_id='sess-1'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(msg_count, 2, "should have 2 message rows");

    // Verify commit row exists.
    let commit_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(commit_count, 1, "should have 1 commit row");

    // Verify branch ref points to new commit.
    let ref_commit_id: Option<String> = conn
        .query_row(
            "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='main'",
            [],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(
        ref_commit_id,
        Some(commit.commit_id.clone()),
        "branch ref should point to new commit"
    );

    // Verify session head_commit_id points to new commit.
    let head_commit_id: Option<String> = conn
        .query_row(
            "SELECT head_commit_id FROM graph_sessions WHERE session_id='sess-1'",
            [],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(
        head_commit_id,
        Some(commit.commit_id.clone()),
        "session head should point to new commit"
    );
}

#[test]
fn commit_turn_rejects_stale_parent() {
    let dir = temp_dir("commit_stale_parent");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, "coder")
        .expect("create");

    // First commit succeeds.
    let messages1 = vec![user_msg("First")];
    let input1 = commit_input("sess-1", vec![], messages1, TurnCommitStatus::Completed);
    store.commit_turn(input1).expect("first commit");

    // Second commit with a stale parent (should be first commit's ID).
    let messages2 = vec![user_msg("Second")];
    let mut input2 = commit_input(
        "sess-1",
        vec!["stale-parent-id".to_string()],
        messages2,
        TurnCommitStatus::Completed,
    );
    // Override commit_message to be unique.
    input2.commit_message = "Second".to_string();

    let result = store.commit_turn(input2);
    assert!(result.is_err(), "stale parent should be rejected");

    // Verify commit count is still 1 (the failed write didn't persist).
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let commit_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(commit_count, 1, "failed commit should not persist");
}

#[test]
fn commit_turn_rejects_cross_session_secondary_parent() {
    let dir = temp_dir("commit_cross_session_secondary_parent");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("victim", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create victim");
    let victim_commit = store
        .commit_turn(commit_input(
            "victim",
            vec![],
            vec![user_msg("VICTIM_SECRET_TOKEN_12345")],
            TurnCommitStatus::Completed,
        ))
        .expect("victim commit");

    store
        .create_session("attacker", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create attacker");
    let attacker_commit = store
        .commit_turn(commit_input(
            "attacker",
            vec![],
            vec![user_msg("attacker base")],
            TurnCommitStatus::Completed,
        ))
        .expect("attacker commit");

    let result = store.commit_turn(commit_input(
        "attacker",
        vec![attacker_commit.commit_id, victim_commit.commit_id.clone()],
        vec![user_msg("attacker merge")],
        TurnCommitStatus::Completed,
    ));

    assert!(
        matches!(result, Err(SessionError::MissingCommit(id)) if id == victim_commit.commit_id),
        "cross-session secondary parent must be rejected before it can poison attacker history"
    );

    let transcript = store
        .head_transcript("attacker")
        .expect("attacker transcript");
    assert!(
        transcript
            .messages
            .iter()
            .all(|message| message.content != "VICTIM_SECRET_TOKEN_12345"),
        "attacker transcript must not include victim messages"
    );
}

#[test]
fn commit_turn_round_trips_tool_message() {
    let dir = temp_dir("commit_tool_msg");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, "coder")
        .expect("create");

    let messages = vec![
        user_msg("run ls"),
        assistant_msg("calling tool"),
        tool_msg("file1.txt\nfile2.txt", "call_123", "shell"),
    ];
    let input = commit_input("sess-1", vec![], messages, TurnCommitStatus::Completed);

    let commit = store.commit_turn(input).expect("commit_turn");
    assert_eq!(commit.message_ids.len(), 3);

    let conn = Connection::open(store.db_path()).expect("raw conn");

    // Verify tool message persisted with tool_call_id and name.
    let (role, tool_call_id, name): (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT role, tool_call_id, name FROM graph_messages WHERE role='tool' AND session_id='sess-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("tool message should exist");

    assert_eq!(role, "tool");
    assert_eq!(tool_call_id, Some("call_123".to_string()));
    assert_eq!(name, Some("shell".to_string()));
}

#[test]
fn commit_turn_accepts_completed_error_and_cancelled_statuses() {
    let dir = temp_dir("commit_all_statuses");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, "coder")
        .expect("create");

    for (i, status) in [
        TurnCommitStatus::Completed,
        TurnCommitStatus::Error,
        TurnCommitStatus::Cancelled,
    ]
    .iter()
    .enumerate()
    {
        let messages = vec![user_msg(&format!("Turn {i}"))];
        let parent_ids = if i == 0 {
            vec![]
        } else {
            // Use the previous commit as parent.
            vec![store
                .log("sess-1")
                .expect("log")
                .last()
                .unwrap()
                .commit_id
                .clone()]
        };
        let input = TurnCommitInput {
            session_id: "sess-1".to_string(),
            parent_ids,
            messages: messages.clone(),
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "test-profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            status: status.clone(),
            commit_message: format!("Turn {i}"),
            usage: None,
            terminal_reason: None,
        };

        let commit = store
            .commit_turn(input)
            .unwrap_or_else(|_| panic!("commit_turn should accept status {:?}", status));
        assert_eq!(commit.status, *status);
    }

    let conn = Connection::open(store.db_path()).expect("raw conn");
    let commit_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(commit_count, 3, "should have 3 commits for 3 statuses");
}

#[test]
fn commit_turn_first_commit_has_empty_parent_ids() {
    let dir = temp_dir("commit_first_empty_parents");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, "coder")
        .expect("create");

    let messages = vec![user_msg("First turn")];
    let input = commit_input("sess-1", vec![], messages, TurnCommitStatus::Completed);

    let commit = store.commit_turn(input).expect("commit_turn");
    assert!(
        commit.parent_ids.is_empty(),
        "first commit should have empty parent_ids"
    );
}

#[test]
fn commit_turn_second_commit_has_first_as_parent() {
    let dir = temp_dir("commit_chain");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, "coder")
        .expect("create");

    // First commit.
    let messages1 = vec![user_msg("First")];
    let input1 = commit_input("sess-1", vec![], messages1, TurnCommitStatus::Completed);
    let commit1 = store.commit_turn(input1).expect("first commit");

    // Second commit with first as parent.
    let messages2 = vec![user_msg("Second")];
    let input2 = commit_input(
        "sess-1",
        vec![commit1.commit_id.clone()],
        messages2,
        TurnCommitStatus::Completed,
    );
    let commit2 = store.commit_turn(input2).expect("second commit");

    assert_eq!(commit2.parent_ids, vec![commit1.commit_id]);
}
