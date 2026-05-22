//! Tests for pointer ops: status/list/log/branch/checkout/reset/diff.

use std::env;
use std::fs;
use std::path::PathBuf;

use rusqlite::Connection;

use crate::graph::{
    derive_commit_message, GraphHead, GraphMessage, SessionGraphStore, TurnCommit,
    TurnCommitInput, TurnCommitStatus,
};
use crate::sqlite_graph::SqliteSessionGraphStore;
use crate::test_helpers::{assistant_msg, tool_msg, user_msg};
use crate::SessionError;

fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir()
        .join("lattice-session-pointer-ops-tests")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    dir
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
        profile_snapshot_id: Some("test-profile".to_string()),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status,
        commit_message: derive_commit_message(&messages),
        usage: None,
        terminal_reason: None,
    }
}

/// Helper: create a session and commit N turns, returning commit IDs.
fn setup_session_with_commits(
    store: &SqliteSessionGraphStore,
    session_id: &str,
    n: usize,
    label: Option<&str>,
) -> Vec<TurnCommit> {
    store
        .create_session(session_id, label, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    let mut commits: Vec<TurnCommit> = Vec::new();
    for i in 0..n {
        let parent_ids = if i == 0 {
            vec![]
        } else {
            vec![commits.last().unwrap().commit_id.clone()]
        };
        let messages = vec![
            user_msg(&format!("{session_id} turn {i}")),
            assistant_msg(&format!("{session_id} response {i}")),
        ];
        let input = TurnCommitInput {
            session_id: session_id.to_string(),
            parent_ids,
            messages: messages.clone(),
            profile_snapshot_id: Some("test-profile".to_string()),
            model: "sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: TurnCommitStatus::Completed,
            commit_message: format!("Turn {i}"),
            usage: None,
            terminal_reason: None,
        };
        let commit = store.commit_turn(input).expect("commit_turn");
        commits.push(commit);
    }
    commits
}

#[test]
fn status_reports_pointer_state() {
    let dir = temp_dir("status_pointer");
    let store = SqliteSessionGraphStore::new(dir);

    let commits = setup_session_with_commits(&store, "sess-1", 3, Some("test session"));

    let status = store.status("sess-1").expect("status");

    // Session ID matches.
    assert_eq!(status.session_id, "sess-1");

    // HEAD is on the "main" branch.
    match &status.head {
        GraphHead::Branch { name } => assert_eq!(name, "main"),
        other => panic!("expected Branch HEAD, got {other:?}"),
    }

    // Current commit ID is the last commit.
    assert_eq!(
        status.current_commit_id,
        Some(commits.last().unwrap().commit_id.clone())
    );

    // Branch refs include at least "main".
    assert!(
        status.branches.iter().any(|r| r.name == "main"),
        "should have main branch ref, got {:?}",
        status.branches
    );

    // Commit count is 3.
    assert_eq!(status.commit_count, 3);
}

#[test]
fn list_sessions_reports_head_commit_and_commit_count() {
    let dir = temp_dir("list_sessions");
    let store = SqliteSessionGraphStore::new(dir);

    let commits_a = setup_session_with_commits(&store, "sess-a", 2, Some("Session A"));
    setup_session_with_commits(&store, "sess-b", 1, Some("Session B"));

    let list = store.list().expect("list");

    // Should have at least 2 sessions.
    assert!(
        list.len() >= 2,
        "expected at least 2 sessions, got {}",
        list.len()
    );

    let a = list
        .iter()
        .find(|s| s.session_id == "sess-a")
        .expect("sess-a");
    assert_eq!(a.label, Some("Session A".to_string()));
    assert_eq!(a.current_branch, Some("main".to_string()));
    assert_eq!(
        a.head_commit_id,
        Some(commits_a.last().unwrap().commit_id.clone())
    );
    assert_eq!(a.commit_count, 2);

    let b = list
        .iter()
        .find(|s| s.session_id == "sess-b")
        .expect("sess-b");
    assert_eq!(b.label, Some("Session B".to_string()));
    assert_eq!(b.commit_count, 1);
}

#[test]
fn log_is_one_entry_per_turn_commit() {
    let dir = temp_dir("log_entries");
    let store = SqliteSessionGraphStore::new(dir);

    // Create session with tool messages too.
    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    // First turn: user + assistant.
    let messages1 = vec![user_msg("Hello"), assistant_msg("Hi!")];
    let input1 = commit_input("sess-1", vec![], messages1, TurnCommitStatus::Completed);
    let commit1 = store.commit_turn(input1).expect("first commit");

    // Second turn: user + assistant + tool message.
    let messages2 = vec![
        user_msg("run ls"),
        assistant_msg("calling tool"),
        tool_msg("file1.txt", "call_1", "shell"),
    ];
    let input2 = commit_input(
        "sess-1",
        vec![commit1.commit_id.clone()],
        messages2,
        TurnCommitStatus::Completed,
    );
    let commit2 = store.commit_turn(input2).expect("second commit");

    let log = store.log("sess-1").expect("log");

    // Two committed turns -> two log entries.
    assert_eq!(log.len(), 2, "expected 2 log entries, got {}", log.len());

    // First entry.
    let entry1 = &log[0];
    assert_eq!(entry1.commit_id, commit1.commit_id);
    assert!(entry1.parent_ids.is_empty());
    assert_eq!(entry1.status, TurnCommitStatus::Completed);
    assert_eq!(entry1.model, "test-model");
    assert_eq!(entry1.provider, "test-provider");
    assert_eq!(entry1.message_count, 2);
    assert_eq!(entry1.tool_message_count, 0);

    // Second entry.
    let entry2 = &log[1];
    assert_eq!(entry2.commit_id, commit2.commit_id);
    assert_eq!(entry2.parent_ids, vec![commit1.commit_id]);
    assert_eq!(entry2.status, TurnCommitStatus::Completed);
    assert_eq!(entry2.message_count, 3);
    assert_eq!(entry2.tool_message_count, 1);
}

#[test]
fn branch_checkout_and_reset_do_not_create_commits() {
    let dir = temp_dir("pointer_no_commit");
    let store = SqliteSessionGraphStore::new(dir);

    let commits = setup_session_with_commits(&store, "sess-1", 2, None);
    let first_commit_id = commits[0].commit_id.clone();

    // Record commit count before pointer operations.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let count_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
            [],
            |row| row.get(0),
        )
        .expect("count");

    // Create a branch from first commit.
    store
        .branch("sess-1", "feature", Some(&first_commit_id))
        .expect("branch");

    // Checkout the feature branch.
    store.checkout("sess-1", "feature").expect("checkout");

    // Reset main to first commit.
    store.reset("sess-1", &first_commit_id).expect("reset");

    // Checkout back to main.
    store.checkout("sess-1", "main").expect("checkout");

    // Count must be unchanged.
    let count_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_commits WHERE session_id='sess-1'",
            [],
            |row| row.get(0),
        )
        .expect("count");

    assert_eq!(
        count_before, count_after,
        "pointer operations must not create commits"
    );
}

#[test]
fn branch_refs_point_to_commit_ids() {
    let dir = temp_dir("branch_commit_ids");
    let store = SqliteSessionGraphStore::new(dir);

    let commits = setup_session_with_commits(&store, "sess-1", 2, None);
    let first_commit_id = commits[0].commit_id.clone();

    // Create a branch from first commit.
    let branch_ref = store
        .branch("sess-1", "feature", Some(&first_commit_id))
        .expect("branch");

    // The branch ref must point to a commit ID.
    assert_eq!(
        branch_ref.commit_id,
        Some(first_commit_id.clone()),
        "branch ref must store commit ID, not session ID"
    );

    // Verify in the database directly.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let stored_commit_id: Option<String> = conn
        .query_row(
            "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='feature'",
            [],
            |row| row.get(0),
        )
        .expect("query");

    assert_eq!(stored_commit_id, Some(first_commit_id));
}

#[test]
fn reset_returns_old_head_and_preserves_commits() {
    let dir = temp_dir("reset_preserves");
    let store = SqliteSessionGraphStore::new(dir);

    let commits = setup_session_with_commits(&store, "sess-1", 3, None);
    let first_id = commits[0].commit_id.clone();
    let third_id = commits[2].commit_id.clone();

    // Reset to first commit.
    let result = store.reset("sess-1", &first_id).expect("reset");

    // Old HEAD was the third commit.
    assert_eq!(result.old_commit_id, Some(third_id.clone()));
    // New HEAD is the first commit.
    assert_eq!(result.new_commit_id, first_id);
    // Branch is main.
    assert_eq!(result.branch, "main");

    // All commits are still accessible.
    let log = store.log("sess-1").expect("log");
    assert_eq!(log.len(), 3, "all 3 commits must remain after reset");
    assert!(log.iter().any(|e| e.commit_id == third_id));
}

#[test]
fn diff_reports_unique_commit_paths() {
    let dir = temp_dir("diff_paths");
    let store = SqliteSessionGraphStore::new(dir);

    // Create session with 2 commits on main.
    let commits = setup_session_with_commits(&store, "sess-1", 2, None);
    let base_id = commits[0].commit_id.clone();
    let main_head_id = commits[1].commit_id.clone();

    // Create a feature branch from the base commit.
    store
        .branch("sess-1", "feature", Some(&base_id))
        .expect("branch");

    // Checkout feature, commit one more turn on it.
    store.checkout("sess-1", "feature").expect("checkout");
    let feature_messages = vec![user_msg("Feature work"), assistant_msg("Done")];
    let feature_input = TurnCommitInput {
        session_id: "sess-1".to_string(),
        parent_ids: vec![base_id.clone()],
        messages: feature_messages.clone(),
        profile_snapshot_id: Some("test-profile".to_string()),
        model: "sonnet".to_string(),
        provider: "anthropic".to_string(),
        status: TurnCommitStatus::Completed,
        commit_message: derive_commit_message(&feature_messages),
        usage: None,
        terminal_reason: None,
    };
    let feature_commit = store.commit_turn(feature_input).expect("commit feature");

    // Diff between main head and feature head.
    let diff = store
        .diff("sess-1", &main_head_id, &feature_commit.commit_id)
        .expect("diff");

    // Common ancestor should be the base commit.
    assert_eq!(
        diff.common_ancestor_id,
        Some(base_id.clone()),
        "common ancestor should be the base commit"
    );

    // Commits only in "from" (main): the second commit on main.
    assert_eq!(diff.only_in_from.len(), 1);
    assert_eq!(diff.only_in_from[0].commit_id, main_head_id);

    // Commits only in "to" (feature): the feature commit.
    assert_eq!(diff.only_in_to.len(), 1);
    assert_eq!(diff.only_in_to[0].commit_id, feature_commit.commit_id);

    // Verify entries have status and commit_message.
    assert_eq!(diff.only_in_from[0].status, TurnCommitStatus::Completed);
    assert!(!diff.only_in_from[0].commit_message.is_empty());
    assert_eq!(diff.only_in_to[0].status, TurnCommitStatus::Completed);
    assert!(!diff.only_in_to[0].commit_message.is_empty());
}

#[test]
fn checkout_existing_empty_branch_succeeds() {
    let dir = temp_dir("checkout_empty_branch");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    // main branch was created by create_session with NULL commit_id.
    // Checkout should succeed even though the branch has no commits.
    let head = store.checkout("sess-1", "main").expect("checkout main");
    assert!(
        matches!(head, GraphHead::Branch { name } if name == "main"),
        "should return Branch with name 'main'"
    );

    // Status should still show branch main with no commit.
    let status = store.status("sess-1").expect("status");
    assert!(
        matches!(status.head, GraphHead::Branch { name } if name == "main"),
        "status HEAD should be Branch main"
    );
    assert!(
        status.current_commit_id.is_none(),
        "current_commit_id should be None for empty branch"
    );
}

#[test]
fn reset_rejects_detached_head() {
    let dir = temp_dir("reset_detached");
    let store = SqliteSessionGraphStore::new(dir);

    let commits = setup_session_with_commits(&store, "sess-1", 2, None);
    let first_id = commits[0].commit_id.clone();
    let second_id = commits[1].commit_id.clone();

    // Checkout first commit to enter detached HEAD.
    store
        .checkout("sess-1", &first_id)
        .expect("checkout detached");

    // Reset should be rejected while detached.
    let result = store.reset("sess-1", &second_id);
    assert!(result.is_err(), "reset should fail while detached");
    match result {
        Err(SessionError::Conflict(msg)) => {
            assert!(
                msg.contains("detached"),
                "error should mention detached HEAD: {msg}"
            );
        }
        other => panic!("expected Conflict error, got {other:?}"),
    }

    // Verify main branch was NOT moved — still points to second_id.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let main_commit: Option<String> = conn
        .query_row(
            "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='main'",
            [],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(
        main_commit,
        Some(second_id.clone()),
        "main branch should still point to second commit"
    );
}

#[test]
fn commit_turn_while_detached_does_not_move_main_branch() {
    let dir = temp_dir("commit_detached");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    // Commit A on main.
    let messages_a = vec![user_msg("A"), assistant_msg("Response A")];
    let input_a = commit_input("sess-1", vec![], messages_a, TurnCommitStatus::Completed);
    let commit_a = store.commit_turn(input_a).expect("commit A");

    // Checkout A to enter detached HEAD.
    store
        .checkout("sess-1", &commit_a.commit_id)
        .expect("checkout detached");

    // Commit B with parent A (while detached).
    let messages_b = vec![user_msg("B"), assistant_msg("Response B")];
    let input_b = commit_input(
        "sess-1",
        vec![commit_a.commit_id.clone()],
        messages_b,
        TurnCommitStatus::Completed,
    );
    let commit_b = store.commit_turn(input_b).expect("commit B while detached");

    // Status should show detached at B.
    let status = store.status("sess-1").expect("status");
    assert!(
        matches!(&status.head, GraphHead::Detached { commit_id } if *commit_id == commit_b.commit_id),
        "HEAD should be detached at commit B, got {:?}",
        status.head
    );

    // main branch should still point to A.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let main_commit: Option<String> = conn
        .query_row(
            "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='main'",
            [],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(
        main_commit,
        Some(commit_a.commit_id.clone()),
        "main branch should still point to commit A"
    );

    // head_transcript should include messages from both A and B.
    let transcript = store.head_transcript("sess-1").expect("head_transcript");
    assert_eq!(transcript.messages.len(), 4, "should have A+B messages");
    assert_eq!(transcript.messages[0].content, "A");
    assert_eq!(transcript.messages[1].content, "Response A");
    assert_eq!(transcript.messages[2].content, "B");
    assert_eq!(transcript.messages[3].content, "Response B");
    assert_eq!(
        transcript.parent_commit_id,
        Some(commit_b.commit_id),
        "transcript parent should be commit B"
    );
}

#[test]
fn branch_from_detached_head_uses_detached_commit() {
    let dir = temp_dir("branch_from_detached");
    let store = SqliteSessionGraphStore::new(dir);

    let commits = setup_session_with_commits(&store, "sess-1", 2, None);
    let first_id = commits[0].commit_id.clone();

    // Checkout first commit to enter detached HEAD.
    store
        .checkout("sess-1", &first_id)
        .expect("checkout detached");

    // Create branch from detached HEAD (from=None should use current HEAD).
    let branch_ref = store
        .branch("sess-1", "detached-copy", None)
        .expect("branch from detached");

    assert_eq!(
        branch_ref.commit_id,
        Some(first_id.clone()),
        "branch should point to the detached commit"
    );
}

#[test]
fn commit_turn_allows_repeated_identical_user_message_across_turns() {
    let dir = temp_dir("repeated_messages");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    // First turn with user "hello" and assistant "hi".
    let messages1 = vec![user_msg("hello"), assistant_msg("hi")];
    let input1 = commit_input("sess-1", vec![], messages1, TurnCommitStatus::Completed);
    let commit1 = store.commit_turn(input1).expect("first commit");

    // Second turn with identical user "hello" and assistant "hi".
    let messages2 = vec![user_msg("hello"), assistant_msg("hi")];
    let input2 = commit_input(
        "sess-1",
        vec![commit1.commit_id.clone()],
        messages2,
        TurnCommitStatus::Completed,
    );
    let commit2 = store.commit_turn(input2).expect("second commit");

    // Both commits should succeed.
    assert_ne!(
        commit1.commit_id, commit2.commit_id,
        "commit IDs should differ"
    );

    // Message IDs should differ between the two commits.
    assert_ne!(
        commit1.message_ids, commit2.message_ids,
        "message IDs should differ across turns even for identical content"
    );

    // Verify all messages exist in the database.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let msg_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM graph_messages WHERE session_id='sess-1'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(msg_count, 4, "should have 4 message rows total");
}

/// When `branch()` updates the currently checked-out branch, `graph_sessions.head_commit_id`
/// must be kept in sync. Otherwise `status()` returns a stale `current_commit_id`.
#[test]
fn branch_update_current_branch_syncs_session_head_commit_id() {
    let dir = temp_dir("branch_syncs_head");
    let store = SqliteSessionGraphStore::new(dir);

    // 1. Create session and commit A, then commit B on main.
    let commits = setup_session_with_commits(&store, "sess-1", 2, None);
    let commit_a = commits[0].commit_id.clone();
    let commit_b = commits[1].commit_id.clone();

    // Sanity: status shows HEAD on main at commit B.
    let status = store.status("sess-1").expect("status");
    assert!(
        matches!(&status.head, GraphHead::Branch { name } if name == "main"),
        "HEAD should be on main"
    );
    assert_eq!(status.current_commit_id, Some(commit_b.clone()));

    // 2. Call branch() to move main back to commit A.
    store
        .branch("sess-1", "main", Some(&commit_a))
        .expect("branch update");

    // 3. Status must now report commit A as current.
    let status = store.status("sess-1").expect("status after branch");
    assert!(
        matches!(&status.head, GraphHead::Branch { name } if name == "main"),
        "HEAD should still be on main"
    );
    assert_eq!(
        status.current_commit_id,
        Some(commit_a.clone()),
        "status.current_commit_id must be commit A after branch moved main"
    );

    // 4. Verify main branch ref points to commit A.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let main_commit: Option<String> = conn
        .query_row(
            "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='main'",
            [],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(main_commit, Some(commit_a.clone()));

    // 5. head_transcript must also reflect the rewind (via branch ref read).
    let transcript = store.head_transcript("sess-1").expect("head_transcript");
    assert_eq!(
        transcript.messages.len(),
        2,
        "only commit A's messages visible"
    );
    assert_eq!(
        transcript.parent_commit_id,
        Some(commit_a),
        "transcript parent should be commit A"
    );
}

/// Updating a non-current branch via `branch()` must NOT move the session HEAD.
#[test]
fn branch_update_non_current_branch_does_not_move_head() {
    let dir = temp_dir("branch_non_current");
    let store = SqliteSessionGraphStore::new(dir);

    // 1. Create session, commit A, branch 'side' from A, commit B on main.
    let commits = setup_session_with_commits(&store, "sess-1", 1, None);
    let commit_a = commits[0].commit_id.clone();

    store
        .branch("sess-1", "side", Some(&commit_a))
        .expect("create side branch");

    // Commit B on main.
    let messages_b = vec![user_msg("B"), assistant_msg("Response B")];
    let input_b = commit_input(
        "sess-1",
        vec![commit_a.clone()],
        messages_b,
        TurnCommitStatus::Completed,
    );
    let commit_b = store.commit_turn(input_b).expect("commit B");

    // HEAD is still on main at B.
    let status = store.status("sess-1").expect("status");
    assert_eq!(status.current_commit_id, Some(commit_b.commit_id.clone()));
    let commit_b_id = commit_b.commit_id;

    // 2. Update 'side' branch to point to B.
    store
        .branch("sess-1", "side", Some(&commit_b_id))
        .expect("update side branch");

    // 3. HEAD must still be on main at B — not moved by side branch update.
    let status = store.status("sess-1").expect("status after side update");
    assert!(
        matches!(&status.head, GraphHead::Branch { name } if name == "main"),
        "HEAD should still be on main"
    );
    assert_eq!(
        status.current_commit_id,
        Some(commit_b_id.clone()),
        "HEAD commit should still be B"
    );

    // 4. 'side' branch ref should point to B.
    let conn = Connection::open(store.db_path()).expect("raw conn");
    let side_commit: Option<String> = conn
        .query_row(
            "SELECT commit_id FROM graph_refs WHERE session_id='sess-1' AND kind='branch' AND name='side'",
            [],
            |row| row.get(0),
        )
        .expect("query");
    assert_eq!(side_commit, Some(commit_b_id));
}
