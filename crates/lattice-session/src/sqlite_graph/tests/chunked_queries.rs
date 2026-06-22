// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Verify that batch IN queries are chunked correctly when the number of
//! bind parameters exceeds `SQLITE_BIND_CHUNK_SIZE` (900). Exercises
//! `head_transcript`, `log`, `commit_message_ids_batch`, and
//! `load_graph_messages_by_id` across chunk boundaries.

use std::env;
use std::fs;
use std::path::PathBuf;

use crate::graph::{SessionGraphStore, TurnCommitInput, TurnCommitStatus, WorkspaceDisposition};
use crate::sqlite_graph::SqliteSessionGraphStore;
use crate::test_helpers::{assistant_msg, tool_msg, user_msg};

fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir()
        .join("lattice-session-chunked-tests")
        .join(name);
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// Build a chain of N commits, each with 3 messages (user + assistant + tool).
/// Returns (store, last_commit_id).
fn build_long_session(dir: PathBuf, n: usize) -> (SqliteSessionGraphStore, String) {
    let store = SqliteSessionGraphStore::new(dir);
    store
        .create_session("long", None, "coder", "main")
        .expect("create");

    let mut parent_ids: Vec<String> = vec![];
    let mut last_commit_id = String::new();

    for i in 0..n {
        let messages = vec![
            user_msg(&format!("u{i}")),
            assistant_msg(&format!("a{i}")),
            tool_msg(&format!("t{i}"), &format!("call_{i}"), "shell"),
        ];
        let input = TurnCommitInput {
            session_id: "long".to_string(),
            parent_ids: parent_ids.clone(),
            messages,
            agent_name: "coder".to_string(),
            profile_snapshot_hash: "test-profile".to_string(),
            profile_snapshot_json: "{}".to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            status: TurnCommitStatus::Completed,
            disposition: WorkspaceDisposition::NotApplicable,
            lifecycle: None,
            commit_message: format!("turn {i}"),
            usage: None,
            terminal_reason: None,
        };
        let commit = store.commit_turn(input).expect("commit");
        last_commit_id = commit.commit_id.clone();
        parent_ids = vec![commit.commit_id];
    }

    (store, last_commit_id)
}

/// With 310 commits * 3 messages = 930 message_ids, which exceeds the 900
/// chunk size for `load_graph_messages_by_id` and the tool-count query in
/// `log_impl`.
#[test]
fn head_transcript_works_across_chunk_boundaries() {
    let n = 310;
    let dir = temp_dir("head_transcript_chunked");
    let (store, _) = build_long_session(dir, n);

    let transcript = store.head_transcript("long").expect("head_transcript");

    assert_eq!(
        transcript.messages.len(),
        n * 3,
        "should have {expected} messages from {n} commits",
        expected = n * 3
    );

    // Verify first and last messages to confirm ordering is preserved.
    assert_eq!(transcript.messages[0].content, "u0");
    assert_eq!(transcript.messages[1].content, "a0");
    assert_eq!(transcript.messages[2].content, "t0");
    assert_eq!(
        transcript.messages[n * 3 - 3].content,
        format!("u{}", n - 1)
    );
    assert_eq!(
        transcript.messages[n * 3 - 2].content,
        format!("a{}", n - 1)
    );
    assert_eq!(
        transcript.messages[n * 3 - 1].content,
        format!("t{}", n - 1)
    );
}

/// `log` with 930 message_ids exercises the chunked tool-count resolution.
#[test]
fn log_works_across_chunk_boundaries() {
    let n = 310;
    let dir = temp_dir("log_chunked");
    let (store, _) = build_long_session(dir, n);

    let log = store.log("long").expect("log");

    assert_eq!(log.len(), n, "should have {n} log entries");

    for (i, entry) in log.iter().enumerate() {
        assert_eq!(entry.message_count, 3, "commit {i} should have 3 messages");
        assert_eq!(
            entry.tool_message_count, 1,
            "commit {i} should have 1 tool message"
        );
    }
}
