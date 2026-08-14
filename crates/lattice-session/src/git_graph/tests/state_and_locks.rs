// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! ADR 0005 / GitHub #143 acceptance for the session store: strict open,
//! immutable `SessionState` objects, permanent project binding, and the
//! per-session execution lock.

use lattice_store::{Blocking, LockMode};

use super::*;
use crate::SessionError;
use crate::git_graph::GitSessionGraphStore;
use crate::git_graph::state;

/// SC1: an absent store is `NotInitialized`; opening never initializes.
#[test]
fn open_absent_store_fails_closed_without_creating_it() {
    let dir = temp_dir("open_absent");
    let path = dir.path().join("sessions.git");

    let err = GitSessionGraphStore::open(&path).expect_err("absent store must fail");
    assert!(
        matches!(
            err,
            SessionError::Store(lattice_store::StoreError::NotInitialized { .. })
        ),
        "expected NotInitialized, got {err:?}"
    );
    assert!(!path.exists(), "strict open must not create the store");
}

/// SC1: a store created for another domain is rejected by identity, not
/// silently reused.
#[test]
fn open_rejects_foreign_store_identity() {
    let dir = temp_dir("open_foreign");
    let path = dir.path().join("other.git");
    lattice_store::StoreRepo::create(
        &path,
        lattice_store::StoreMeta {
            store_kind: "workspace-history".to_string(),
            store_schema_version: 1,
            scope: lattice_store::StoreScope::Global,
        },
        |_| Ok(()),
    )
    .expect("create foreign store");

    let err = GitSessionGraphStore::open(&path).expect_err("foreign identity must be rejected");
    assert!(
        matches!(
            err,
            SessionError::Store(lattice_store::StoreError::Corruption { ref detail })
                if detail.contains("wrong store kind")
        ),
        "expected a store-kind corruption error, got {err:?}"
    );
}

/// SC2: every session fact lives in the store; no `.lattice/sessions/**`
/// sidecar is written anywhere under the store or its parent.
#[test]
fn session_state_has_no_sidecar_files() {
    let dir = temp_dir("no_sidecar");
    let store = GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create");
    setup_session_with_commits(&store, "sess-nosidecar", 2, None);

    assert!(!dir.path().join(".lattice").exists());
    assert!(!dir.path().join("sessions.git").join(".lattice").exists());
}

/// SC2: a Session is permanently bound to one path-derived key, and the
/// binding survives reopen.
#[test]
fn project_binding_is_persisted_and_survives_reopen() {
    let dir = temp_dir("binding_reopen");
    let path = dir.path().join("sessions.git");
    let key = test_project_key();
    {
        let store = GitSessionGraphStore::create(&path).expect("create");
        let session = store
            .create_session("sess-bound", None, "coder", "main", &key)
            .expect("create session");
        assert_eq!(session.project_store_key, key.to_string());
    }

    let reopened = GitSessionGraphStore::open(&path).expect("reopen");
    let session = reopened.open_session("sess-bound").expect("open session");
    assert_eq!(session.project_store_key, key.to_string());
    assert_eq!(
        reopened
            .status("sess-bound")
            .expect("status")
            .project_store_key,
        key.to_string()
    );
}

/// SC2: a state mutation writes a NEW immutable state commit whose parent is
/// the previous one — the ref moves, the old object stays readable.
#[test]
fn state_mutation_appends_an_immutable_commit() {
    let dir = temp_dir("state_chain");
    let store = GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create");
    store
        .create_session("sess-state", None, "coder", "main", &test_project_key())
        .expect("create session");

    let (first_oid, first_state) = state::read_state(store.kernel(), "sess-state").expect("read");
    assert_eq!(first_state.active_agent_name, "coder");

    store
        .set_active_agent("sess-state", "reviewer")
        .expect("set agent");

    let (second_oid, second_state) = state::read_state(store.kernel(), "sess-state").expect("read");
    assert_ne!(first_oid, second_oid, "state ref must advance");
    assert_eq!(second_state.active_agent_name, "reviewer");

    // The previous state object is immutable and still readable.
    let old_bytes = store
        .kernel()
        .read_tree_blob(first_oid, state::STATE_TREE_FILE)
        .expect("old state object still readable");
    let old: serde_json::Value = serde_json::from_slice(&old_bytes).expect("parse old state");
    assert_eq!(old["active_agent_name"], "coder");

    // …and it is the parent of the new one.
    let parents = store.kernel().commit_parents(second_oid).expect("parents");
    assert_eq!(parents, vec![first_oid]);
}

/// SC3: a commit publishes conversation head and session state in ONE
/// transaction; both advance together.
#[test]
fn commit_publishes_head_and_state_together() {
    let dir = temp_dir("commit_txn");
    let store = GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create");
    store
        .create_session("sess-txn", None, "coder", "main", &test_project_key())
        .expect("create session");
    let (state_before, _) = state::read_state(store.kernel(), "sess-txn").expect("read");

    let ids = {
        let messages = vec![user_msg("hello"), assistant_msg("hi")];
        let input = commit_input("sess-txn", vec![], messages, TurnCommitStatus::Completed);
        store.commit_turn(input).expect("commit")
    };

    let head_ref = state::head_ref_name("sess-txn", "main").expect("head ref");
    let head = store
        .kernel()
        .ref_target(&head_ref)
        .expect("read head")
        .expect("head exists");
    assert_eq!(head.to_string(), ids.commit_id);

    let (state_after, _) = state::read_state(store.kernel(), "sess-txn").expect("read");
    assert_ne!(
        state_before, state_after,
        "the state ref must advance in the same publication"
    );
}

/// SC5: `status` reports `SessionBusy` instead of blocking or misreporting
/// when another holder owns the session execution lock exclusively.
#[test]
fn status_reports_session_busy_under_an_exclusive_lock() {
    let dir = temp_dir("busy_status");
    let store = GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create");
    store
        .create_session("sess-busy", None, "coder", "main", &test_project_key())
        .expect("create session");

    // A separate process-level holder: the same-process flock is re-entrant,
    // so take the lock through a second store handle in a child thread and
    // observe from a foreign process view via a direct file lock.
    let lock_path = dir
        .path()
        .join("locks")
        .join("sessions")
        .join("sess-busy.lock");
    let holder = std::process::Command::new("flock")
        .arg("-x")
        .arg(&lock_path)
        .arg("sleep")
        .arg("5")
        .spawn();

    let Ok(mut holder) = holder else {
        // `flock(1)` is not installed; the lock contract is still covered by
        // the lattice-store kernel's own cross-process test.
        return;
    };
    // Give the child time to acquire.
    std::thread::sleep(std::time::Duration::from_millis(300));

    let err = store.status("sess-busy").expect_err("must report busy");
    let _ = holder.kill();
    let _ = holder.wait();

    assert!(
        matches!(err, SessionError::SessionBusy(ref id) if id == "sess-busy"),
        "expected SessionBusy, got {err:?}"
    );
}

/// SC5: after the exclusive holder releases, `status` succeeds again.
#[test]
fn status_succeeds_once_the_lock_is_released() {
    let dir = temp_dir("busy_release");
    let store = GitSessionGraphStore::create(dir.path().join("sessions.git")).expect("create");
    store
        .create_session("sess-free", None, "coder", "main", &test_project_key())
        .expect("create session");

    {
        let _guard = store
            .acquire_execution_lock("sess-free", LockMode::Exclusive, Blocking::Block)
            .expect("acquire");
        // Same-process re-entrancy: a nested acquisition must not deadlock.
        let _nested = store
            .acquire_execution_lock("sess-free", LockMode::Exclusive, Blocking::NonBlocking)
            .expect("re-entrant acquire");
    }

    let status = store.status("sess-free").expect("status after release");
    assert_eq!(status.session_id, "sess-free");
}
