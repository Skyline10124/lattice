// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Transcript reconstruction tests for the Git graph store.

use super::*;
use crate::git_graph::GitSessionGraphStore;

#[test]
fn test_head_transcript_linear() {
    let dir = temp_dir("transcript_linear");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");
    let commit_ids = setup_session_with_commits(&store, "sess-transcript", 2, None);

    let transcript = store
        .head_transcript("sess-transcript")
        .expect("head_transcript");
    assert_eq!(transcript.session_id, "sess-transcript");
    assert_eq!(transcript.parent_commit_id, Some(commit_ids[1].clone()));
    assert_eq!(transcript.messages.len(), 4);
    assert_eq!(
        crate::test_helpers::text(&transcript.messages[0]),
        Some("Turn 0")
    );
    assert_eq!(
        crate::test_helpers::text(&transcript.messages[1]),
        Some("Response 0")
    );
    assert_eq!(
        crate::test_helpers::text(&transcript.messages[2]),
        Some("Turn 1")
    );
    assert_eq!(
        crate::test_helpers::text(&transcript.messages[3]),
        Some("Response 1")
    );
}

#[test]
fn test_head_transcript_empty_session() {
    let dir = temp_dir("transcript_empty");
    let store =
        GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create store");
    store
        .create_session(
            "sess-empty-transcript",
            None,
            "coder",
            "main",
            &test_project_key(),
        )
        .expect("create");

    let transcript = store
        .head_transcript("sess-empty-transcript")
        .expect("head_transcript");
    assert_eq!(transcript.session_id, "sess-empty-transcript");
    assert!(transcript.parent_commit_id.is_none());
    assert!(transcript.messages.is_empty());
}
