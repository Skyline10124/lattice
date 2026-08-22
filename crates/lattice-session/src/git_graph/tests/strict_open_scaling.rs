// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! L-GH139A-11: ordinary fresh reopen must not parse unrelated Session history.

use super::*;
use crate::git_graph::{GitSessionGraphStore, last_strict_open_work};

fn reopen(path: &std::path::Path) {
    drop(GitSessionGraphStore::open(path).expect("strict reopen"));
}

#[test]
fn strict_open_validation_work_does_not_grow_with_unrelated_session_history() {
    let dir = temp_dir("strict-open-scale");
    let path = dir.path().join("sessions.git");
    let store = GitSessionGraphStore::create(&path).expect("create");
    setup_session_with_commits(&store, "target", 2, None);
    drop(store);

    reopen(&path);
    let after_one = last_strict_open_work();
    assert!(
        after_one.sessions_seen >= 1,
        "expected to observe the target session, got {after_one:?}"
    );

    let store = GitSessionGraphStore::open(&path).expect("reopen to accumulate");
    for i in 0..8 {
        setup_session_with_commits(&store, &format!("unrelated-{i}"), 3, None);
    }
    drop(store);

    reopen(&path);
    let after_many = last_strict_open_work();

    assert!(
        after_many.sessions_seen > after_one.sessions_seen,
        "unrelated sessions must still be visible to enumeration: one={after_one:?} many={after_many:?}"
    );
    assert_eq!(
        after_one.state_objects_parsed, 0,
        "target session must be covered by its projection: {after_one:?}"
    );
    assert_eq!(
        after_one.sessions_projection_hits, after_one.sessions_seen,
        "target session must hit its projection: {after_one:?}"
    );
    assert_eq!(
        after_many.state_objects_parsed, after_one.state_objects_parsed,
        "state object parsing must not grow with unrelated sessions: one={after_one:?} many={after_many:?}"
    );
    assert_eq!(
        after_many.branch_commits_checked, after_one.branch_commits_checked,
        "branch commit checks must not grow with unrelated sessions: one={after_one:?} many={after_many:?}"
    );
    assert_eq!(
        after_many.turn_pins_parsed, after_one.turn_pins_parsed,
        "turn-pin parsing must not grow with unrelated history: one={after_one:?} many={after_many:?}"
    );
    assert_eq!(
        after_many.active_turns_parsed, after_one.active_turns_parsed,
        "active-turn parsing must not grow with unrelated history: one={after_one:?} many={after_many:?}"
    );
    assert_eq!(
        after_many.sessions_projection_hits, after_many.sessions_seen,
        "every accumulated session must hit its projection: {after_many:?}"
    );
    assert_eq!(after_many.state_objects_parsed, 0);
    assert_eq!(after_many.branch_commits_checked, 0);
}

#[test]
fn strict_open_still_rejects_tampered_unrelated_session() {
    let dir = temp_dir("strict-open-tamper");
    let path = dir.path().join("sessions.git");
    let store = GitSessionGraphStore::create(&path).expect("create");
    setup_session_with_commits(&store, "healthy", 1, None);
    setup_session_with_commits(&store, "tampered", 1, None);
    let state_ref = crate::git_graph::state::state_ref_name("tampered").unwrap();
    let state_oid = store
        .kernel()
        .ref_target(&state_ref)
        .unwrap()
        .expect("state ref");
    let missing = gix::ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
    store
        .kernel()
        .cas_ref(
            &state_ref,
            missing,
            lattice_store::RefExpectation::MustMatch(state_oid),
            "test tamper unrelated state",
        )
        .unwrap();
    drop(store);

    let err = GitSessionGraphStore::open(&path).expect_err("strict open must fail closed");
    assert!(
        matches!(
            err,
            crate::SessionError::Store(lattice_store::StoreError::Corruption { .. })
        ),
        "got {err:?}"
    );
}
