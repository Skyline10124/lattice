// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Head transcript reads ordered messages.
//!
//! Commits 3 turns on a branch (2 messages per turn), then calls
//! `head_transcript` and verifies all 6 messages are returned in commit order
//! — first turn's messages first, last turn's messages last.

use lattice_conversation::MessageRole;
use lattice_session::test_helpers::{assistant_msg, user_msg};
use lattice_session::{GitSessionGraphStore, SessionGraphStore, TurnCommitInput};
use tempfile::TempDir;

fn test_store_key() -> lattice_session::ProjectStoreKey {
    lattice_session::ProjectStoreKey::from_canonical_path_bytes(b"/lattice-api-tests/project")
}

fn turn_input(
    session_id: &str,
    parent_ids: Vec<String>,
    user_text: &str,
    assistant_text: &str,
) -> TurnCommitInput {
    TurnCommitInput {
        turn_id: lancet_protocol::TurnId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
            .expect("valid test TurnId"),
        workspace_history_checkpoint: lattice_session::WorkspaceHistoryCheckpoint::Empty,
        session_id: session_id.to_string(),
        parent_ids,
        messages: vec![user_msg(user_text), assistant_msg(assistant_text)],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        outcome: lattice_session::TurnOutcome::Completed {
            finish_reason: lattice_session::TurnFinishReason::Stop,
        },
        origin: lattice_session::ClosureOrigin::Live,
        commit_message: "turn".to_string(),
        usage: None,
    }
}

#[test]
fn git_store_head_transcript_reads_ordered_messages() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();

    let store = GitSessionGraphStore::create(dir.join("sessions.git")).expect("create store");

    let session_id = "fedcba9876543210fedcba9876543210".to_string();
    store
        .create_session(
            &session_id,
            Some("transcript-order"),
            "coder",
            "main",
            &test_store_key(),
        )
        .expect("create_session");

    // Commit 3 turns linearly on the default branch, each with 2 messages.
    let mut parent_ids: Vec<String> = vec![];
    let expected_texts = [
        ("turn-0-user", "turn-0-assistant"),
        ("turn-1-user", "turn-1-assistant"),
        ("turn-2-user", "turn-2-assistant"),
    ];
    for (user_text, assistant_text) in expected_texts.iter() {
        let commit = store
            .test_commit_turn(turn_input(
                &session_id,
                parent_ids.clone(),
                user_text,
                assistant_text,
            ))
            .expect("commit_turn");
        parent_ids = vec![commit.commit_id];
    }

    let transcript = store.head_transcript(&session_id).expect("head_transcript");

    assert_eq!(transcript.session_id, session_id);
    assert!(
        transcript.parent_commit_id.is_some(),
        "parent_commit_id should be set after commits"
    );

    let messages = transcript.messages;
    assert_eq!(
        messages.len(),
        6,
        "expected 6 messages (3 turns x 2), got {}",
        messages.len()
    );

    // Verify ordering: first turn's messages first, last turn's messages last.
    let contents: Vec<&str> = messages
        .iter()
        .map(|message| lattice_session::test_helpers::text(message).expect("text message"))
        .collect();
    assert_eq!(
        contents,
        vec![
            "turn-0-user",
            "turn-0-assistant",
            "turn-1-user",
            "turn-1-assistant",
            "turn-2-user",
            "turn-2-assistant",
        ],
        "messages must appear in commit order (turn 0 first, turn 2 last), got: {:?}",
        contents,
    );

    // Also verify roles alternate user/assistant per turn.
    let roles: Vec<_> = messages.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        vec![
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Assistant,
        ],
        "roles must alternate per message in commit order, got: {:?}",
        roles,
    );
}
