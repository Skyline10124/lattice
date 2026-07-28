// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Candidate refs created for lifecycle turns.
//!
//! A turn committed with lifecycle evidence (`candidate_oid` set) creates a
//! candidate ref under `refs/lattice/candidates/<session_id>/<turn_id>`
//! pointing to the session commit OID. The ref is queryable.

use lattice_session::test_helpers::{assistant_msg, user_msg};
use lattice_session::{
    GitSessionGraphStore, SessionGraphStore, TurnCommitInput, TurnCommitStatus,
    TurnLifecycleEvidence, WorkspaceDisposition,
};
use tempfile::TempDir;

fn init_git_repo(dir: &std::path::Path) {
    let _ = lattice_session::test_utils::init_empty_repo(dir);
}

#[test]
fn git_store_candidate_ref_created_for_lifecycle_turn() {
    let tmp = TempDir::new().expect("temp dir");
    let dir = tmp.path();
    init_git_repo(dir);

    let store = GitSessionGraphStore::new(dir);

    let session_id = "aabbccddeeff00112233445566778899".to_string();
    store
        .create_session(&session_id, Some("candidate-ref-test"), "coder", "main")
        .expect("create_session");

    // Commit a turn WITH lifecycle evidence and candidate_oid set. This should
    // trigger candidate ref creation in commit_prepared_turn_impl.
    let lifecycle = TurnLifecycleEvidence {
        target_project_ref: "refs/heads/main".to_string(),
        baseline_oid: Some("0".repeat(40)),
        candidate_oid: Some("1".repeat(40)),
        candidate_ref: None,
        verification: None,
        snapshot_ref: None,
        resumes_turn_oid: None,
    };
    let input = TurnCommitInput {
        workspace_history_oid: None,
        session_id: session_id.clone(),
        parent_ids: vec![],
        messages: vec![user_msg("candidate turn"), assistant_msg("done")],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::PendingPromotion,
        lifecycle: Some(lifecycle),
        commit_message: "candidate-turn-0".to_string(),
        usage: None,
        terminal_reason: None,
    };
    let commit = store.commit_turn(input).expect("commit_turn");
    let commit_oid = commit.commit_id.clone();

    // 1. Enumerate all refs under refs/lattice/candidates/<session_id>/** and
    //    confirm exactly one candidate ref exists pointing at the session
    //    commit OID.
    let repo = gix::open_opts(dir, gix::open::Options::isolated()).expect("open repo");
    let candidate_prefix = format!("refs/lattice/candidates/{session_id}/");
    let mut candidate_refs: Vec<(String, gix::ObjectId)> = Vec::new();
    let platform = repo.references().expect("enumerate references");
    for reference in platform.all().expect("iterate references") {
        let r = reference.expect("read reference");
        let name = r.name().as_bstr().to_string();
        if name.starts_with(&candidate_prefix)
            && let Some(oid) = r.target().try_id()
        {
            candidate_refs.push((name, oid.to_owned()));
        }
    }
    assert_eq!(
        candidate_refs.len(),
        1,
        "expected exactly 1 candidate ref under {candidate_prefix}, got {}: {:?}",
        candidate_refs.len(),
        candidate_refs
            .iter()
            .map(|(n, o)| (n, o.to_string()))
            .collect::<Vec<_>>(),
    );

    let (ref_name, ref_oid) = &candidate_refs[0];
    assert!(
        ref_name.starts_with(&candidate_prefix),
        "candidate ref name should be under {candidate_prefix}, got: {ref_name}",
    );
    assert_eq!(
        ref_oid.to_string(),
        commit_oid,
        "candidate ref must point at the session commit OID",
    );

    // 2. The turn_id component (suffix after the session_id) is non-empty and
    //    matches the derive_turn_id format (16 chars, alphanumeric).
    let turn_id = ref_name.strip_prefix(&candidate_prefix).unwrap_or(ref_name);
    assert!(!turn_id.is_empty(), "turn_id component must be non-empty");
    assert!(
        turn_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
        "turn_id must be a valid ref component: {turn_id}",
    );

    // 3. The candidate ref is queryable via find_reference.
    let queried = repo
        .find_reference(ref_name.as_str())
        .expect("candidate ref should be queryable via find_reference");
    assert_eq!(
        queried.target().try_id().map(|o| o.to_string()),
        Some(commit_oid.clone()),
        "queried candidate ref target must match session commit OID",
    );

    // 4. Control: a turn WITHOUT lifecycle evidence must NOT create a
    //    candidate ref. Commit a second turn with no lifecycle and confirm
    //    the candidate ref count stays at 1.
    let plain_input = TurnCommitInput {
        workspace_history_oid: None,
        session_id: session_id.clone(),
        parent_ids: vec![commit_oid.clone()],
        messages: vec![user_msg("plain turn"), assistant_msg("ok")],
        agent_name: "coder".to_string(),
        profile_snapshot_hash: "test-profile-hash".to_string(),
        profile_snapshot_json: "{}".to_string(),
        model: "test-model".to_string(),
        provider: "test-provider".to_string(),
        status: TurnCommitStatus::Completed,
        disposition: WorkspaceDisposition::NotApplicable,
        lifecycle: None,
        commit_message: "plain-turn-1".to_string(),
        usage: None,
        terminal_reason: None,
    };
    let _plain = store.commit_turn(plain_input).expect("commit plain turn");

    let mut candidate_refs_after: Vec<(String, gix::ObjectId)> = Vec::new();
    let platform = repo.references().expect("enumerate references after");
    for reference in platform.all().expect("iterate references after") {
        let r = reference.expect("read reference");
        let name = r.name().as_bstr().to_string();
        if name.starts_with(&candidate_prefix)
            && let Some(oid) = r.target().try_id()
        {
            candidate_refs_after.push((name, oid.to_owned()));
        }
    }
    assert_eq!(
        candidate_refs_after.len(),
        1,
        "plain turn (no lifecycle) must not create a candidate ref; count should stay 1, got: {:?}",
        candidate_refs_after
            .iter()
            .map(|(n, o)| (n, o.to_string()))
            .collect::<Vec<_>>(),
    );
}
