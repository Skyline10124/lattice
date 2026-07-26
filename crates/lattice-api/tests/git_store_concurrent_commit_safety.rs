// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Concurrent commit safety.
//!
//! Two concurrent `commit_prepared_turn` calls to the same branch with the
//! same expected parent do not silently overwrite each other. The second
//! commit returns a Conflict error because the ref moved between the CAS
//! check and the ref update.

use std::sync::Arc;
use std::thread;

use lattice_session::test_helpers::{assistant_msg, user_msg};
use lattice_session::{
    GitSessionGraphStore, PreparedTurnCommit, SessionError, SessionGraphStore, TurnCommitInput,
    TurnCommitStatus, WorkspaceDisposition, prepare_turn_commit,
};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    let _ = lattice_session::test_utils::init_empty_repo(dir);
}

fn turn_input(session_id: &str, parent_ids: Vec<String>, label: &str) -> TurnCommitInput {
    TurnCommitInput {
        session_id: session_id.to_string(),
        parent_ids,
        messages: vec![
            user_msg(&format!("{label} user")),
            assistant_msg(&format!("{label} assistant")),
        ],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: label.to_string(),
        usage: None,
        terminal_reason: None,
    }
}

fn prepare(session_id: &str, parent_ids: Vec<String>, label: &str) -> PreparedTurnCommit {
    let input = turn_input(session_id, parent_ids, label);
    prepare_turn_commit(&input, "2026-01-01T00:00:00Z")
}

#[test]
fn git_store_concurrent_commit_same_parent_second_conflicts() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path().to_path_buf();
    init_git_repo(&dir);

    let store = GitSessionGraphStore::new(&dir);
    let session_id = "c0ffee1234567890abcdef1234567890".to_string();
    store
        .create_session(&session_id, Some("concurrent-test"), "coder", "main")
        .expect("create_session");

    // 1. Commit the base turn A (no parents). Ref now points at A.
    let base = store
        .commit_turn(turn_input(&session_id, vec![], "base"))
        .expect("base commit");
    let base_oid = base.commit_id.clone();

    // 2. Prepare two turns B and C, both expecting parent A.
    let prepared_b = prepare(&session_id, vec![base_oid.clone()], "turn-b");
    let prepared_c = prepare(&session_id, vec![base_oid.clone()], "turn-c");

    // 3. Commit B first — should succeed and move the ref from A to B.
    let committed_b = store
        .commit_prepared_turn(prepared_b)
        .expect("commit B should succeed");
    let b_oid = committed_b.commit_id.clone();
    assert_ne!(b_oid, base_oid, "B must be a distinct commit");

    // 4. Commit C — the ref is now at B, but C expects parent A. The CAS
    //    check detects the ref moved and returns a Conflict error.
    let err = store
        .commit_prepared_turn(prepared_c)
        .expect_err("commit C should conflict");
    assert!(
        matches!(err, SessionError::Conflict(ref msg) if msg.contains("HEAD") || msg.contains("parent") || msg.contains("ref")),
        "expected Conflict error about HEAD/parent/ref mismatch, got: {err:?}",
    );

    // 5. The ref must still point at B (C did not silently overwrite it).
    let repo = gix::open_opts(&dir, gix::open::Options::isolated()).expect("open repo");
    let ref_name = format!("refs/lattice/sessions/{session_id}/main");
    let branch_ref = repo
        .find_reference(ref_name.as_str())
        .expect("main ref should exist");
    assert_eq!(
        branch_ref.target().try_id().map(|o| o.to_string()),
        Some(b_oid.clone()),
        "ref must still point at B after C's conflict; C must not have overwritten it",
    );

    // 6. C was never written. Verify the ref target is B and the log has
    //    exactly 2 entries (base + B), confirming C was rejected.
    let log = store.log(&session_id).expect("log");
    assert_eq!(
        log.len(),
        2,
        "log should have exactly 2 commits (base + B), got {} — C must not have been committed",
        log.len(),
    );
    assert_eq!(log[1].commit_id, b_oid);
}

#[test]
fn git_store_threaded_concurrent_commits_one_wins_one_conflicts() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path().to_path_buf();
    init_git_repo(&dir);

    let store = Arc::new(GitSessionGraphStore::new(&dir));
    let session_id = "dec0de1234567890abcdef1234567890".to_string();
    store
        .create_session(&session_id, Some("threaded-concurrent"), "coder", "main")
        .expect("create_session");

    // Base commit A.
    let base = store
        .commit_turn(turn_input(&session_id, vec![], "base"))
        .expect("base commit");
    let base_oid = base.commit_id.clone();

    // Prepare 4 turns all expecting parent A, then commit them from 4 threads.
    let prepared: Vec<PreparedTurnCommit> = (0..4)
        .map(|i| prepare(&session_id, vec![base_oid.clone()], &format!("race-{i}")))
        .collect();

    let store_clone = Arc::clone(&store);
    let handles: Vec<_> = prepared
        .into_iter()
        .map(move |p| {
            let s = Arc::clone(&store_clone);
            thread::spawn(move || s.commit_prepared_turn(p))
        })
        .collect();

    let mut oks = 0;
    let mut conflicts = 0;
    for h in handles {
        match h.join().expect("thread panicked") {
            Ok(_) => oks += 1,
            Err(SessionError::Conflict(_)) => conflicts += 1,
            Err(e) => panic!("unexpected error (not Conflict): {e:?}"),
        }
    }

    assert_eq!(
        oks, 1,
        "exactly one of the 4 concurrent commits should succeed, got {oks}",
    );
    assert_eq!(
        conflicts, 3,
        "the other 3 should return Conflict, got {conflicts}",
    );

    // The ref must point at exactly one of the winning commits, and the log
    // must have exactly 2 entries (base + winner).
    let log = store.log(&session_id).expect("log");
    assert_eq!(
        log.len(),
        2,
        "log should have exactly 2 commits (base + winner), got {}",
        log.len(),
    );
}
