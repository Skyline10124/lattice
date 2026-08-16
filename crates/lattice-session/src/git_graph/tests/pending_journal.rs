// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! PendingTurn journal persistence over the store kernel (GitHub #144):
//! CAS chain, predecessor grafting, draft publication/classification, and
//! the one final ref transaction.

use lattice_store::{Blocking, LockMode, RefExpectation};

use crate::SessionError;
use crate::git_graph::GitSessionGraphStore;
use crate::git_graph::tests::{Fixture, temp_dir};
use crate::graph::{SessionGraphStore, TurnCommitStatus};
use crate::pending::contract::PendingStage;
use crate::pending::draft::ResultDraft;
use crate::pending::{DraftClassification, DraftPublication};

use super::pending_fixtures::*;

fn setup() -> (Fixture, GitSessionGraphStore, String) {
    let fixture = temp_dir("pending-journal");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    store
        .create_session(SESSION, None, "coder", "main", &super::test_project_key())
        .expect("session");
    (fixture, store, SESSION.to_string())
}

fn head_oid(store: &GitSessionGraphStore) -> Option<String> {
    store
        .kernel()
        .ref_target(&format!("refs/lattice/sessions/{SESSION}/heads/main"))
        .expect("head")
        .map(|oid| oid.to_string())
}

#[test]
fn turn_started_publishes_active_turn_grafted_on_the_conversation_tip() {
    let (_f, store, session) = setup();
    // One committed conversation turn to graft onto.
    let first = store
        .commit_turn(super::commit_input(
            &session,
            vec![],
            vec![
                crate::test_helpers::user_msg("first"),
                crate::test_helpers::assistant_msg("reply"),
            ],
            TurnCommitStatus::Completed,
        ))
        .expect("first turn");
    let tip = head_oid(&store);
    assert_eq!(tip.as_deref(), Some(first.commit_id.as_str()));

    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let started = store
        .begin_pending_turn(turn_started_facts(tip.clone()), &lock)
        .expect("begin");

    let ref_name = format!("refs/lattice/sessions/{session}/active-turn");
    assert_eq!(
        store.kernel().ref_target(&ref_name).expect("ref"),
        Some(started.oid)
    );
    // The journal chain root is grafted onto the frozen conversation tip.
    assert_eq!(
        store.kernel().commit_parents(started.oid).expect("parents"),
        vec![
            store
                .kernel()
                .ref_target(&format!("refs/lattice/sessions/{session}/heads/main"))
                .expect("head")
                .expect("present")
        ]
    );
    let read = store
        .read_active_turn(&session)
        .expect("read")
        .expect("some");
    assert_eq!(read, started);
    assert!(read.snapshot.predecessor.is_none());
    assert!(read.snapshot.recorded_at.is_some(), "recorded_at filled");
}

#[test]
fn turn_started_rejects_a_moved_tip_and_a_second_active_turn() {
    let (_f, store, session) = setup();
    let first = store
        .commit_turn(super::commit_input(
            &session,
            vec![],
            vec![
                crate::test_helpers::user_msg("first"),
                crate::test_helpers::assistant_msg("reply"),
            ],
            TurnCommitStatus::Completed,
        ))
        .expect("first turn");
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");

    // Frozen tip must equal the actual head.
    let stale = turn_started_facts(Some(first.commit_id.clone()));
    store.begin_pending_turn(stale, &lock).expect("valid begin");
    // A second active Turn on the same session fails closed.
    let second = store.begin_pending_turn(turn_started_facts(None), &lock);
    assert!(matches!(second, Err(SessionError::Conflict(_))));

    // A frozen tip that never was the head is a conflict.
    let (_f2, store2, session2) = setup();
    let lock2 = store2
        .lock_execution(&session2, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let lying = turn_started_facts(Some("0123456789012345678901234567890123456789".to_string()));
    assert!(matches!(
        store2.begin_pending_turn(lying, &lock2),
        Err(SessionError::Conflict(_))
    ));
}

#[test]
fn advance_validates_transitions_and_rejects_committed_via_active_turn() {
    let (_f, store, session) = setup();
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let started = store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .expect("begin");

    let started_stage = PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    };
    let tip1 = store
        .advance_pending_turn(&session, started_stage, 100, &lock)
        .expect("advance");
    assert_eq!(
        tip1.snapshot.predecessor.as_deref(),
        Some(started.oid.to_string().as_str())
    );

    // Illegal transition (retry after a non-started attempt) is a conflict.
    let bad = store.advance_pending_turn(
        &session,
        PendingStage::ModelAttemptStarted {
            key: attempt_key(0, 1),
        },
        101,
        &lock,
    );
    assert!(matches!(bad, Err(SessionError::Conflict(_))));

    // A stage of another Turn is invalid input.
    let mut foreign = attempt_key(0, 0);
    foreign.turn_id =
        lancet_protocol::TurnId::parse("00000000-0000-4000-8000-00000000000f").expect("other turn");
    assert!(matches!(
        store.advance_pending_turn(
            &session,
            PendingStage::ModelAttemptStarted { key: foreign },
            102,
            &lock
        ),
        Err(SessionError::InvalidInput(_))
    ));

    // Committed is never publishable through active-turn.
    let committed = PendingStage::Committed(crate::pending::CommittedFacts {
        turn_id: turn_id(),
        candidate: completed_candidate(None),
    });
    assert!(matches!(
        store.advance_pending_turn(&session, committed, 103, &lock),
        Err(SessionError::Conflict(_))
    ));
}

#[test]
fn corrupt_active_target_is_typed_corruption() {
    let (_f, store, session) = setup();
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .expect("begin");
    drop(lock);

    // Tamper: point active-turn at a commit with no pending.json.
    let blob = store.kernel().write_blob(b"not a tree").expect("blob");
    let commit = store
        .kernel()
        .write_commit_at(blob, &[], "garbage", 1)
        .expect("commit");
    let ref_name = format!("refs/lattice/sessions/{session}/active-turn");
    let current = store
        .kernel()
        .ref_target(&ref_name)
        .expect("ref")
        .expect("present");
    store
        .kernel()
        .cas_ref(
            &ref_name,
            commit,
            RefExpectation::MustMatch(current),
            "tamper",
        )
        .expect("tamper");

    // Reading the tampered store's active turn is typed corruption.
    let err = store.read_active_turn(&session);
    assert!(
        matches!(
            err,
            Err(SessionError::JournalCorruption { .. }) | Err(SessionError::Store(_))
        ),
        "got {err:?}"
    );

    // Strict reopen also fails closed on the tampered ref.
    let reopened = GitSessionGraphStore::open(store.dir());
    assert!(reopened.is_err(), "domain invariant validation rejects it");
}

#[test]
fn draft_publication_is_durable_then_uncertain_then_reopen_classifies() {
    let (_f, store, session) = setup();
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let plan = one_binding_plan();
    let candidate = ResultDraft::new(
        plan.bindings[0].invocation_id,
        plan.bindings[0].binding_digest,
        turn_id(),
        plan.bindings[0].address.message,
        0,
        "{\"entries\":[]}".to_string(),
    )
    .expect("draft");

    // Before publication: provably absent.
    assert_eq!(
        store.classify_draft(&candidate).expect("classify"),
        DraftClassification::CleanAbsent
    );

    let published = store.publish_draft(&candidate, &lock).expect("publish");
    let DraftPublication::Durable { draft_oid } = &published else {
        panic!("expected durable publication, got {published:?}");
    };
    assert_eq!(
        store.classify_draft(&candidate).expect("classify"),
        DraftClassification::Durable
    );
    // The durable oid is the content address of the canonical draft bytes:
    // the classified candidate reads back byte-identical.
    let oid = crate::git_graph::helpers::parse_oid(draft_oid).expect("oid");
    assert_eq!(
        store.kernel().read_blob(oid).expect("read"),
        crate::git_graph::pending::canonical_draft_bytes(&candidate).expect("canonical")
    );
    let _ = session;

    // An injected object-fsync failure classifies the publication as
    // Uncertain carrying the exact candidate, never as clean absence. Live
    // classification is the publish return value: an unfysnced object that
    // is still present in this process proves nothing, and `classify_draft`
    // is the reopen-side API — a fresh process sees only objects that
    // survived the crash.
    lattice_store::fault::fail_object_fsync(true);
    let other = ResultDraft::new(
        plan.bindings[0].invocation_id,
        plan.bindings[0].binding_digest,
        turn_id(),
        plan.bindings[0].address.message,
        0,
        "{\"entries\":[1]}".to_string(),
    )
    .expect("draft");
    let uncertain = store.publish_draft(&other, &lock).expect("classified");
    assert!(matches!(uncertain, DraftPublication::Uncertain { .. }));
}

#[test]
fn finalize_publishes_head_pin_state_and_deletes_active_turn_atomically() {
    let (_f, store, session) = setup();
    let first = store
        .commit_turn(super::commit_input(
            &session,
            vec![],
            vec![
                crate::test_helpers::user_msg("first"),
                crate::test_helpers::assistant_msg("reply"),
            ],
            TurnCommitStatus::Completed,
        ))
        .expect("first turn");
    let head = head_oid(&store);
    assert_eq!(head.as_deref(), Some(first.commit_id.as_str()));

    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    store
        .begin_pending_turn(turn_started_facts(head.clone()), &lock)
        .expect("begin");
    store
        .advance_pending_turn(
            &session,
            PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 0),
            },
            100,
            &lock,
        )
        .expect("attempt");
    store
        .advance_pending_turn(
            &session,
            PendingStage::MessageComplete {
                key: attempt_key(0, 0),
                message: assistant_text("done"),
                usage: None,
            },
            101,
            &lock,
        )
        .expect("complete");
    store
        .advance_pending_turn(
            &session,
            PendingStage::StepClosed {
                kind: crate::pending::StepClosedKind::FinishTurn {
                    final_message: assistant_text("done"),
                },
            },
            102,
            &lock,
        )
        .expect("closed");
    let closing_tip = store
        .advance_pending_turn(
            &session,
            // The frozen tip is journal authority; the candidate carries only side parents.
            PendingStage::TurnClosing(turn_closing(completed_candidate(None))),
            103,
            &lock,
        )
        .expect("closing");

    let committed = store
        .finalize_turn(&session, &turn_id(), &lock)
        .expect("finalize");
    let commit_oid = committed.commit_id.clone();

    // Head advanced, pin created, active-turn deleted.
    assert_eq!(head_oid(&store).as_deref(), Some(commit_oid.as_str()));
    let pin = format!("refs/lattice/sessions/{session}/turns/{TURN}");
    assert_eq!(
        store.kernel().ref_target(&pin).expect("pin"),
        crate::git_graph::helpers::parse_oid(&commit_oid).ok()
    );
    assert!(store.read_active_turn(&session).expect("read").is_none());

    // The pinned TurnCommit's parent is the Committed journal object whose
    // parent is the TurnClosing tip: the journal chain stays reachable.
    let turn_oid = crate::git_graph::helpers::parse_oid(&commit_oid).expect("oid");
    let parents = store.kernel().commit_parents(turn_oid).expect("parents");
    assert_eq!(parents.len(), 1);
    let committed_parents = store
        .kernel()
        .commit_parents(parents[0])
        .expect("committed parents");
    assert_eq!(committed_parents, vec![closing_tip.oid]);

    // Transcript reconstruction skips journal commits and keeps both turns.
    let transcript = store.head_transcript(&session).expect("transcript");
    assert_eq!(transcript.messages.len(), 4);
    assert_eq!(transcript.messages[2].content, "please continue");
    assert_eq!(transcript.messages[3].content, "done");
    // Log sees two conversation turns, no journal noise.
    assert_eq!(store.log(&session).expect("log").len(), 2);

    // Finalize is idempotent: the exact pinned TurnCommit comes back.
    let again = store
        .finalize_turn(&session, &turn_id(), &lock)
        .expect("idempotent");
    assert_eq!(again.commit_id, commit_oid);
}

#[test]
fn finalize_requires_a_turn_closing_tip_and_the_matching_turn() {
    let (_f, store, session) = setup();
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .expect("begin");
    assert!(matches!(
        store.finalize_turn(&session, &turn_id(), &lock),
        Err(SessionError::InvalidInput(_))
    ));

    // A different Turn id is a conflict.
    let other = lancet_protocol::TurnId::parse("00000000-0000-4000-8000-00000000000f").expect("t");
    assert!(matches!(
        store.finalize_turn(&session, &other, &lock),
        Err(SessionError::Conflict(_))
    ));
}

#[test]
fn finalize_conflict_publishes_nothing() {
    let (_f, store, session) = setup();
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .expect("begin");
    store
        .advance_pending_turn(
            &session,
            PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 0),
            },
            100,
            &lock,
        )
        .expect("attempt");
    store
        .advance_pending_turn(
            &session,
            PendingStage::MessageComplete {
                key: attempt_key(0, 0),
                message: assistant_text("done"),
                usage: None,
            },
            101,
            &lock,
        )
        .expect("complete");
    store
        .advance_pending_turn(
            &session,
            PendingStage::StepClosed {
                kind: crate::pending::StepClosedKind::FinishTurn {
                    final_message: assistant_text("done"),
                },
            },
            102,
            &lock,
        )
        .expect("closed");
    store
        .advance_pending_turn(
            &session,
            PendingStage::TurnClosing(turn_closing(completed_candidate(None))),
            103,
            &lock,
        )
        .expect("closing");

    // Move the conversation head behind the journal's back: the branch was
    // empty at TurnStarted (frozen tip None), and a foreign commit appears
    // on it before finalization. The frozen-tip check must fail before any
    // ref moves.
    let other_blob = store.kernel().write_blob(b"moved").expect("blob");
    let moved = store
        .kernel()
        .write_commit_at(other_blob, &[], "moved head", 1)
        .expect("moved commit");
    let head_ref = format!("refs/lattice/sessions/{session}/heads/main");
    store
        .kernel()
        .cas_ref(
            &head_ref,
            moved,
            RefExpectation::MustNotExist,
            "tamper head",
        )
        .expect("moved");

    let err = store.finalize_turn(&session, &turn_id(), &lock);
    assert!(matches!(err, Err(SessionError::Conflict(_))), "got {err:?}");
    // Nothing published: active-turn still present, no pin, head untouched.
    assert!(store.read_active_turn(&session).expect("read").is_some());
    assert!(store.read_turn_pin(&session, TURN).expect("pin").is_none());
    assert_eq!(
        store.kernel().ref_target(&head_ref).expect("head"),
        Some(moved)
    );
}
