//! Tests for `head_transcript` and usage/terminal-reason persistence.

use std::env;
use std::fs;
use std::path::PathBuf;

use crate::graph::{
    derive_commit_message, GraphHead, GraphMessage, SessionGraphStore, TurnCommitInput,
    TurnCommitStatus, TurnUsage,
};
use crate::sqlite_graph::SqliteSessionGraphStore;
use crate::test_helpers::{assistant_msg, tool_msg, user_msg};

fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir()
        .join("lattice-session-head-transcript-tests")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

fn commit_input_with_usage(
    session_id: &str,
    parent_ids: Vec<String>,
    messages: Vec<GraphMessage>,
    status: TurnCommitStatus,
    usage: Option<TurnUsage>,
    terminal_reason: Option<String>,
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
        usage,
        terminal_reason,
    }
}

#[test]
fn head_transcript_empty_session_has_no_parent_and_no_messages() {
    let dir = temp_dir("empty_session");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    let transcript = store.head_transcript("sess-1").expect("head_transcript");

    assert_eq!(transcript.session_id, "sess-1");
    assert!(
        transcript.parent_commit_id.is_none(),
        "empty session should have no parent commit"
    );
    assert!(
        transcript.messages.is_empty(),
        "empty session should have no messages"
    );
}

#[test]
fn head_transcript_reads_current_branch_history_in_commit_order() {
    let dir = temp_dir("history_order");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    // First commit: user + assistant.
    let messages1 = vec![user_msg("Turn 0"), assistant_msg("Response 0")];
    let input1 = commit_input_with_usage(
        "sess-1",
        vec![],
        messages1,
        TurnCommitStatus::Completed,
        None,
        None,
    );
    let commit1 = store.commit_turn(input1).expect("first commit");

    // Second commit: user + assistant + tool.
    let messages2 = vec![
        user_msg("Turn 1"),
        assistant_msg("Calling tool"),
        tool_msg("tool output", "call_1", "shell"),
    ];
    let input2 = commit_input_with_usage(
        "sess-1",
        vec![commit1.commit_id.clone()],
        messages2,
        TurnCommitStatus::Completed,
        None,
        None,
    );
    store.commit_turn(input2).expect("second commit");

    // Third commit: user + assistant.
    let messages3 = vec![user_msg("Turn 2"), assistant_msg("Response 2")];
    let input3 = commit_input_with_usage(
        "sess-1",
        vec![store
            .log("sess-1")
            .expect("log")
            .last()
            .unwrap()
            .commit_id
            .clone()],
        messages3,
        TurnCommitStatus::Completed,
        None,
        None,
    );
    store.commit_turn(input3).expect("third commit");

    // Read transcript.
    let transcript = store.head_transcript("sess-1").expect("head_transcript");

    // Should have all 7 messages from 3 commits in order.
    assert_eq!(
        transcript.messages.len(),
        7,
        "expected 7 messages from 3 commits"
    );

    // Verify chronological order: commit 0 messages, then commit 1, then commit 2.
    assert_eq!(transcript.messages[0].content, "Turn 0");
    assert_eq!(transcript.messages[1].content, "Response 0");
    assert_eq!(transcript.messages[2].content, "Turn 1");
    assert_eq!(transcript.messages[3].content, "Calling tool");
    assert_eq!(transcript.messages[4].content, "tool output");
    assert_eq!(transcript.messages[4].role, "tool");
    assert_eq!(transcript.messages[5].content, "Turn 2");
    assert_eq!(transcript.messages[6].content, "Response 2");

    // Parent should be the last commit's ID.
    assert!(transcript.parent_commit_id.is_some());
}

#[test]
fn head_transcript_respects_checkout_or_branch_head() {
    let dir = temp_dir("checkout_transcript");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    // First commit on main.
    let messages1 = vec![user_msg("Main turn"), assistant_msg("Main response")];
    let input1 = commit_input_with_usage(
        "sess-1",
        vec![],
        messages1,
        TurnCommitStatus::Completed,
        None,
        None,
    );
    let commit1 = store.commit_turn(input1).expect("first commit");

    // Create feature branch from first commit.
    store
        .branch("sess-1", "feature", Some(&commit1.commit_id))
        .expect("branch");

    // Checkout feature, commit one more turn.
    store.checkout("sess-1", "feature").expect("checkout");

    let messages2 = vec![user_msg("Feature turn"), assistant_msg("Feature response")];
    let input2 = commit_input_with_usage(
        "sess-1",
        vec![commit1.commit_id.clone()],
        messages2,
        TurnCommitStatus::Completed,
        None,
        None,
    );
    store.commit_turn(input2).expect("feature commit");

    // Transcript on feature branch should have 4 messages.
    let feature_transcript = store.head_transcript("sess-1").expect("head_transcript");
    assert_eq!(
        feature_transcript.messages.len(),
        4,
        "feature branch should have 4 messages"
    );

    // Reset main to first commit, checkout main.
    store.reset("sess-1", &commit1.commit_id).expect("reset");
    store.checkout("sess-1", "main").expect("checkout");

    // Transcript on main should have only 2 messages.
    let main_transcript = store.head_transcript("sess-1").expect("head_transcript");
    assert_eq!(
        main_transcript.messages.len(),
        2,
        "main branch should have 2 messages after reset"
    );
    assert_eq!(main_transcript.messages[0].content, "Main turn");
    assert_eq!(main_transcript.messages[1].content, "Main response");
}

#[test]
fn commit_turn_persists_usage_and_terminal_reason() {
    let dir = temp_dir("usage_persist");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    // Completed turn with usage.
    let messages1 = vec![user_msg("Hello"), assistant_msg("Hi!")];
    let usage1 = TurnUsage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
    };
    let input1 = commit_input_with_usage(
        "sess-1",
        vec![],
        messages1,
        TurnCommitStatus::Completed,
        Some(usage1.clone()),
        Some("stop".to_string()),
    );
    let commit1 = store.commit_turn(input1).expect("commit completed");

    // Error turn with usage.
    let messages2 = vec![user_msg("Fail"), assistant_msg("Error occurred")];
    let usage2 = TurnUsage {
        prompt_tokens: 200,
        completion_tokens: 0,
        total_tokens: 200,
    };
    let input2 = commit_input_with_usage(
        "sess-1",
        vec![commit1.commit_id.clone()],
        messages2,
        TurnCommitStatus::Error,
        Some(usage2.clone()),
        Some("error: provider timeout".to_string()),
    );
    let commit2 = store.commit_turn(input2).expect("commit error");

    // Cancelled turn without usage.
    let messages3 = vec![user_msg("Cancel me")];
    let input3 = commit_input_with_usage(
        "sess-1",
        vec![commit2.commit_id.clone()],
        messages3,
        TurnCommitStatus::Cancelled,
        None,
        Some("cancelled by user".to_string()),
    );
    let commit3 = store.commit_turn(input3).expect("commit cancelled");

    // Verify usage round-trips through log.
    let log = store.log("sess-1").expect("log");
    assert_eq!(log.len(), 3);

    // Completed turn.
    assert_eq!(log[0].usage, Some(usage1));
    assert_eq!(log[0].terminal_reason, Some("stop".to_string()));

    // Error turn.
    assert_eq!(log[1].usage, Some(usage2));
    assert_eq!(
        log[1].terminal_reason,
        Some("error: provider timeout".to_string())
    );

    // Cancelled turn.
    assert!(log[2].usage.is_none());
    assert_eq!(
        log[2].terminal_reason,
        Some("cancelled by user".to_string())
    );

    // Verify usage round-trips through commit output.
    assert_eq!(
        commit1.usage,
        Some(TurnUsage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150
        })
    );
    assert_eq!(commit1.terminal_reason, Some("stop".to_string()));
    assert_eq!(
        commit2.usage,
        Some(TurnUsage {
            prompt_tokens: 200,
            completion_tokens: 0,
            total_tokens: 200
        })
    );
    assert_eq!(
        commit2.terminal_reason,
        Some("error: provider timeout".to_string())
    );
    assert!(commit3.usage.is_none());
    assert_eq!(
        commit3.terminal_reason,
        Some("cancelled by user".to_string())
    );
}

#[test]
fn head_transcript_uses_detached_head_commit_id() {
    let dir = temp_dir("detached_transcript");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    // Commit first turn on main.
    let messages1 = vec![user_msg("Hello"), assistant_msg("Hi there!")];
    let input1 = commit_input_with_usage(
        "sess-1",
        vec![],
        messages1,
        TurnCommitStatus::Completed,
        None,
        None,
    );
    let commit1 = store.commit_turn(input1).expect("first commit");

    // Checkout the commit ID to enter detached HEAD.
    let head = store
        .checkout("sess-1", &commit1.commit_id)
        .expect("checkout to detached");
    assert!(
        matches!(head, GraphHead::Detached { .. }),
        "should be detached HEAD"
    );

    // head_transcript should still return the messages from the detached commit.
    let transcript = store.head_transcript("sess-1").expect("head_transcript");

    assert_eq!(
        transcript.parent_commit_id,
        Some(commit1.commit_id.clone()),
        "transcript parent should be the detached commit"
    );
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[0].content, "Hello");
    assert_eq!(transcript.messages[0].role, "user");
    assert_eq!(transcript.messages[1].content, "Hi there!");
    assert_eq!(transcript.messages[1].role, "assistant");
}

fn commit_input(
    session_id: &str,
    parent_ids: Vec<String>,
    messages: Vec<GraphMessage>,
) -> TurnCommitInput {
    commit_input_with_usage(
        session_id,
        parent_ids,
        messages,
        TurnCommitStatus::Completed,
        None,
        None,
    )
}

fn user_contents(messages: &[GraphMessage]) -> Vec<&str> {
    messages.iter().map(|m| m.content.as_str()).collect()
}

/// Build A → B on main, C on side branch, M merge(parents: [B, C]).
fn setup_merge_graph(store: &SqliteSessionGraphStore) -> (String, String, String, String) {
    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    let commit_a = store
        .commit_turn(commit_input(
            "sess-1",
            vec![],
            vec![user_msg("A-user"), assistant_msg("A-assist")],
        ))
        .expect("commit A");

    let commit_b = store
        .commit_turn(commit_input(
            "sess-1",
            vec![commit_a.commit_id.clone()],
            vec![user_msg("B-user"), assistant_msg("B-assist")],
        ))
        .expect("commit B");

    store
        .branch("sess-1", "side", Some(&commit_a.commit_id))
        .expect("branch side");
    store.checkout("sess-1", "side").expect("checkout side");

    let commit_c = store
        .commit_turn(commit_input(
            "sess-1",
            vec![commit_a.commit_id.clone()],
            vec![user_msg("C-user"), assistant_msg("C-assist")],
        ))
        .expect("commit C");

    store.checkout("sess-1", "main").expect("checkout main");

    let commit_m = store
        .commit_turn(commit_input(
            "sess-1",
            vec![commit_b.commit_id.clone(), commit_c.commit_id.clone()],
            vec![user_msg("M-user"), assistant_msg("M-assist")],
        ))
        .expect("commit merge");

    (
        commit_a.commit_id,
        commit_b.commit_id,
        commit_c.commit_id,
        commit_m.commit_id,
    )
}

#[test]
fn head_transcript_linear_history_unchanged_by_topological_traversal() {
    let dir = temp_dir("linear_regression");
    let store = SqliteSessionGraphStore::new(dir);

    store
        .create_session("sess-1", None, None, Some("sonnet"), Some("anthropic"))
        .expect("create");

    let commit1 = store
        .commit_turn(commit_input(
            "sess-1",
            vec![],
            vec![user_msg("Turn 0"), assistant_msg("Response 0")],
        ))
        .expect("first commit");

    let commit2 = store
        .commit_turn(commit_input(
            "sess-1",
            vec![commit1.commit_id.clone()],
            vec![user_msg("Turn 1"), assistant_msg("Response 1")],
        ))
        .expect("second commit");

    store
        .commit_turn(commit_input(
            "sess-1",
            vec![commit2.commit_id.clone()],
            vec![user_msg("Turn 2"), assistant_msg("Response 2")],
        ))
        .expect("third commit");

    let transcript = store.head_transcript("sess-1").expect("head_transcript");
    let contents = user_contents(&transcript.messages);

    assert_eq!(
        contents,
        vec![
            "Turn 0",
            "Response 0",
            "Turn 1",
            "Response 1",
            "Turn 2",
            "Response 2",
        ]
    );
}

#[test]
fn head_transcript_includes_second_parent_branch_on_merge() {
    let dir = temp_dir("merge_second_parent");
    let store = SqliteSessionGraphStore::new(dir);

    let (_a, _b, _c, merge_id) = setup_merge_graph(&store);

    let transcript = store.head_transcript("sess-1").expect("head_transcript");
    let contents = user_contents(&transcript.messages);

    assert_eq!(
        contents,
        vec![
            "A-user", "A-assist", "B-user", "B-assist", "C-user", "C-assist", "M-user", "M-assist",
        ],
        "merge transcript must include side-branch (second parent) messages"
    );
    assert_eq!(transcript.parent_commit_id, Some(merge_id));
}

#[test]
fn head_transcript_diamond_merge_does_not_duplicate_common_ancestor() {
    let dir = temp_dir("diamond_merge");
    let store = SqliteSessionGraphStore::new(dir);

    setup_merge_graph(&store);

    let transcript = store.head_transcript("sess-1").expect("head_transcript");

    assert_eq!(
        transcript.messages.len(),
        8,
        "diamond merge should emit each commit's messages once"
    );
    assert_eq!(
        transcript
            .messages
            .iter()
            .filter(|m| m.content == "A-user")
            .count(),
        1,
        "common ancestor A must appear exactly once"
    );
    assert_eq!(
        transcript
            .messages
            .iter()
            .filter(|m| m.content == "A-assist")
            .count(),
        1
    );
}
