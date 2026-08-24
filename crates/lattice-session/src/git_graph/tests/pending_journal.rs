// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! PendingTurn journal persistence over the store kernel (GitHub #144):
//! CAS chain, predecessor grafting, draft publication/classification, and
//! the one final ref transaction.

use lattice_store::{Blocking, LockMode, RefExpectation};

use crate::SessionError;
use crate::git_graph::GitSessionGraphStore;
use crate::git_graph::tests::{Fixture, temp_dir};
use crate::graph::SessionGraphStore;
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
            (),
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
            (),
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
            (),
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
    assert_eq!(committed.turn_id, turn_id());

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
    assert_eq!(
        crate::test_helpers::text(&transcript.messages[2]),
        Some("please continue")
    );
    assert_eq!(
        crate::test_helpers::text(&transcript.messages[3]),
        Some("done")
    );
    // Log sees two conversation turns, no journal noise, and preserves the
    // explicit durable identity of the finalized Turn.
    let log = store.log(&session).expect("log");
    assert_eq!(log.len(), 2);
    assert_eq!(log.last().expect("finalized log row").turn_id, turn_id());

    // Public counts include conversation Turns only, never journal snapshots.
    assert_eq!(
        store
            .status_locked(&session, &lock)
            .expect("status")
            .commit_count,
        2
    );
    drop(lock);
    let summary = store
        .list()
        .expect("list")
        .into_iter()
        .find(|summary| summary.session_id == session)
        .expect("session summary");
    assert_eq!(summary.commit_count, 2);
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("relock");
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

// ─── Review-round depth coverage (F5/F6/F1) ─────────────────────────────

/// Write one journal-shaped commit with an arbitrary payload and parent set,
/// bypassing every publication contract. Corruption fixtures only.
fn forge_journal_commit(
    store: &GitSessionGraphStore,
    payload: &serde_json::Value,
    parents: &[gix::ObjectId],
) -> gix::ObjectId {
    let bytes = crate::graph::canonical_json_bytes(payload);
    let blob = store.kernel().write_blob(&bytes).expect("blob");
    let tree = store
        .kernel()
        .write_tree(lattice_store::TreeSpec::new().upsert(
            "pending.json",
            gix::objs::tree::EntryKind::Blob,
            blob,
        ))
        .expect("tree");
    store
        .kernel()
        .write_commit_at(tree, parents, "forged journal commit", 200)
        .expect("commit")
}

fn force_active_turn(store: &GitSessionGraphStore, session: &str, oid: gix::ObjectId) {
    let ref_name = format!("refs/lattice/sessions/{session}/active-turn");
    let current = store.kernel().ref_target(&ref_name).expect("ref");
    let expect = match current {
        Some(existing) => RefExpectation::MustMatch(existing),
        None => RefExpectation::MustNotExist,
    };
    store
        .kernel()
        .cas_ref(&ref_name, oid, expect, "test forge active-turn")
        .expect("forced");
}

#[test]
fn strict_open_validates_the_active_turn_snapshot_in_depth() {
    // F5.1: strict open must run the full domain invariants of the
    // `active-turn` object, not just parse it and compare the session.
    let (fixture, store, session) = setup();
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let started = store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .expect("begin");
    drop(lock);

    // A payload that parses and names the right session but violates the
    // snapshot contract: a `TurnStarted` stage carrying a predecessor.
    let mut payload = serde_json::to_value(&started.snapshot).expect("payload");
    payload["predecessor"] = serde_json::Value::String("0".repeat(40));
    let forged = forge_journal_commit(&store, &payload, &[]);
    force_active_turn(&store, &session, forged);
    drop(store);

    let err = GitSessionGraphStore::open(fixture.path().join("sessions.git"))
        .expect_err("strict open must reject the invalid journal object");
    assert!(
        matches!(err, SessionError::JournalCorruption { .. }),
        "got {err:?}"
    );
}

#[test]
fn strict_open_validates_the_turn_pin_identity() {
    // F5.2: a `turns/<TurnId>` pin must point at the conversation commit of
    // *that* Turn of *this* session, not merely at some commit carrying a
    // manifest.
    let (fixture, store, session) = setup();
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .expect("begin");
    store
        .advance_pending_turn(
            &session,
            PendingStage::StepClosed {
                kind: crate::pending::StepClosedKind::FinishTurn {
                    final_message: assistant_text("done"),
                },
            },
            99,
            &lock,
        )
        .expect("closed");
    store
        .advance_pending_turn(
            &session,
            PendingStage::TurnClosing(turn_closing(completed_candidate(None))),
            100,
            &lock,
        )
        .expect("closing");
    let committed = store
        .finalize_turn(&session, &turn_id(), &lock)
        .expect("finalize");
    drop(lock);

    // A second pin under a different TurnId aimed at the same commit.
    let other = "00000000-0000-4000-8000-0000000000ff";
    let pin = format!("refs/lattice/sessions/{session}/turns/{other}");
    store
        .kernel()
        .cas_ref(
            &pin,
            crate::git_graph::helpers::parse_oid(&committed.commit_id).expect("oid"),
            RefExpectation::MustNotExist,
            "test forge turn pin",
        )
        .expect("forged pin");
    drop(store);

    let err = GitSessionGraphStore::open(fixture.path().join("sessions.git"))
        .expect_err("strict open must reject a mislabelled turn pin");
    assert!(
        matches!(err, SessionError::JournalCorruption { .. }),
        "got {err:?}"
    );
}

#[test]
fn journal_read_rejects_a_predecessor_that_diverges_from_the_git_parent() {
    // F6: `pending.json`'s `predecessor` and the commit's git parent are two
    // recordings of the same fact. A chain where they disagree is typed
    // corruption, not a readable journal.
    let (_f, store, session) = setup();
    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let started = store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .expect("begin");
    let attempt = store
        .advance_pending_turn(
            &session,
            PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 0),
            },
            100,
            &lock,
        )
        .expect("attempt");
    drop(lock);

    // Same payload (predecessor = the attempt snapshot), but the git parent
    // is grafted back onto the chain root.
    let payload = serde_json::json!({
        "schema_version": 1,
        "session_id": SESSION,
        "turn_id": TURN,
        "predecessor": attempt.oid.to_string(),
        "stage": {
            "stage": "message_complete",
            "key": {
                "turn_id": TURN,
                "model_step_ordinal": 0,
                "attempt_ordinal": 0,
            },
            "message": serde_json::to_value(assistant_text("done")).expect("message"),
            "usage": serde_json::Value::Null,
        },
    });
    let forged = forge_journal_commit(&store, &payload, &[started.oid]);
    force_active_turn(&store, &session, forged);

    let err = store
        .read_active_turn(&session)
        .expect_err("divergent predecessor must fail closed");
    assert!(
        matches!(err, SessionError::JournalCorruption { .. }),
        "got {err:?}"
    );
}

#[test]
fn turn_started_root_rejects_a_graft_onto_a_foreign_parent() {
    // F6, root case: the chain root's git parent is the frozen conversation
    // tip. A root grafted onto anything else is corruption, and a root with
    // no frozen tip must have no parent at all.
    let (_f, store, session) = setup();
    let first = store
        .commit_turn(super::commit_input(
            &session,
            vec![],
            vec![
                crate::test_helpers::user_msg("first"),
                crate::test_helpers::assistant_msg("reply"),
            ],
            (),
        ))
        .expect("first turn");

    // Facts freeze the real tip, but the commit is grafted onto nothing.
    let payload = serde_json::to_value(crate::pending::contract::PendingSnapshot {
        schema_version: 1,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: None,
        stage: PendingStage::TurnStarted(turn_started_facts(Some(first.commit_id.clone()))),
        recorded_at: None,
        root: None,
    })
    .expect("payload");
    let forged = forge_journal_commit(&store, &payload, &[]);
    force_active_turn(&store, &session, forged);

    let err = store
        .read_active_turn(&session)
        .expect_err("ungrafted root must fail closed");
    assert!(
        matches!(err, SessionError::JournalCorruption { .. }),
        "got {err:?}"
    );
}

#[test]
fn journal_finalized_turns_keep_no_legacy_outcome_authority() {
    // Typed outcome/origin/checkpoint are the only terminal facts in schema v4.
    use crate::pending::outcome::{
        TurnCancellation, TurnFailure, TurnFinishReason, TurnInterruption, TurnOutcome,
    };

    let outcomes = [
        TurnOutcome::Completed {
            finish_reason: TurnFinishReason::Stop,
        },
        TurnOutcome::Failed {
            reason: TurnFailure::ModelRequestFailed,
        },
        TurnOutcome::Cancelled {
            reason: TurnCancellation::UserCancelled,
        },
        TurnOutcome::Interrupted {
            reason: TurnInterruption::ProcessInterruptedBeforeModelAttempt,
        },
    ];

    for outcome in outcomes {
        let (_f, store, session) = setup();
        let lock = store
            .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
            .expect("lock");
        store
            .begin_pending_turn(turn_started_facts(None), &lock)
            .expect("begin");
        let mut candidate = completed_candidate(None);
        candidate.outcome = outcome.clone();
        store
            .advance_pending_turn(
                &session,
                PendingStage::StepClosed {
                    kind: crate::pending::StepClosedKind::FinishTurn {
                        final_message: assistant_text("done"),
                    },
                },
                99,
                &lock,
            )
            .expect("closed");
        store
            .advance_pending_turn(
                &session,
                PendingStage::TurnClosing(turn_closing(candidate)),
                100,
                &lock,
            )
            .expect("closing");
        let committed = store
            .finalize_turn(&session, &turn_id(), &lock)
            .expect("finalize");

        // The typed outcome is durable in the journal `Committed` object.
        let commit_oid = crate::git_graph::helpers::parse_oid(&committed.commit_id).expect("oid");
        let journal_oid = store.kernel().commit_parents(commit_oid).expect("parents")[0];
        let journal = store
            .read_pending_snapshot_at(journal_oid)
            .expect("committed journal object");
        match journal.snapshot.stage {
            PendingStage::Committed(ref facts) => {
                assert_eq!(facts.candidate.outcome, outcome, "typed authority");
            }
            ref other => panic!("expected Committed, got {}", other.stage_name()),
        }

        let manifest_bytes = store
            .kernel()
            .read_tree_blob(commit_oid, "turn/manifest.json")
            .expect("manifest");
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest_bytes).expect("manifest json");
        assert_eq!(manifest["outcome"], serde_json::to_value(&outcome).unwrap());
        assert_eq!(manifest["origin"], "live");
        assert!(manifest.get("workspace_history_checkpoint").is_some());
        for retired in [
            "execution_status",
            "disposition",
            "terminal_reason",
            "lifecycle",
        ] {
            assert!(
                manifest.get(retired).is_none(),
                "retired field {retired}: {manifest:#}"
            );
        }
        assert!(
            store
                .kernel()
                .read_tree_blob(commit_oid, "turn/audit.json")
                .is_err(),
            "retired lifecycle audit blob must not exist"
        );
    }
}

#[test]
fn binding_candidate_revision_cas_publishes_a_full_replacement_plan() {
    let (_fixture, store, session) = setup();
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
    let original = one_binding_plan();
    let planned = store
        .advance_pending_turn(
            &session,
            PendingStage::BindingPlanned {
                plan: original.clone(),
                usage: None,
            },
            101,
            &lock,
        )
        .expect("planned");

    let mut revised = original.clone();
    let binding = &mut revised.bindings[0];
    binding.invocation_id = invocation_id(2);
    binding.binding_digest = crate::pending::binding_digest(
        &binding.invocation_id,
        &binding.address,
        &binding.method,
        &binding.descriptor_identity,
        &binding.arguments_digest,
    );
    lattice_store::fault::fail_ref_fsync(true);
    let err = store.revise_binding_candidates(&session, planned.oid, revised.clone(), 102, &lock);
    assert!(matches!(
        err,
        Err(SessionError::Store(
            lattice_store::StoreError::Durability { .. }
        ))
    ));
    let revised_tip = store
        .revise_binding_candidates(&session, planned.oid, revised.clone(), 102, &lock)
        .expect("exact retry confirms the published revision");

    assert_eq!(
        revised_tip.snapshot.predecessor.as_deref(),
        Some(planned.oid.to_string().as_str())
    );
    assert_eq!(
        revised_tip.snapshot.stage,
        PendingStage::BindingPlanned {
            plan: revised,
            usage: None,
        }
    );
    assert!(matches!(
        store.revise_binding_candidates(&session, planned.oid, original, 103, &lock),
        Err(SessionError::Conflict(_))
    ));
}

#[test]
fn binding_candidate_revision_rejects_any_old_uuid_reuse() {
    let (_fixture, store, session) = setup();
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

    let mut original = one_binding_plan();
    let arguments = serde_json::json!({"path": "second"});
    let address = lancet_protocol::ToolCallAddress {
        message: lancet_protocol::ModelMessageAddress {
            session_id: session_id(),
            turn_id: turn_id(),
            model_step_ordinal: 0,
        },
        part_ordinal: 1,
    };
    let arguments_digest = lattice_conversation::arguments_digest(&arguments);
    original
        .message
        .parts
        .push(tool_call_part("observe.workspace", arguments));
    original.bindings.push(crate::pending::PlannedBinding {
        invocation_id: invocation_id(2),
        binding_digest: crate::pending::binding_digest(
            &invocation_id(2),
            &address,
            "observe.workspace",
            &"d".repeat(64),
            &arguments_digest,
        ),
        address,
        method: "observe.workspace".to_string(),
        descriptor_identity: "d".repeat(64),
        arguments_digest,
    });
    original.validate().expect("two-binding plan");
    let planned = store
        .advance_pending_turn(
            &session,
            PendingStage::BindingPlanned {
                plan: original.clone(),
                usage: None,
            },
            101,
            &lock,
        )
        .expect("planned");

    let mut permuted = original.clone();
    let old_ids = [
        permuted.bindings[0].invocation_id,
        permuted.bindings[1].invocation_id,
    ];
    permuted.bindings[0].invocation_id = old_ids[1];
    permuted.bindings[1].invocation_id = old_ids[0];
    for binding in &mut permuted.bindings {
        binding.binding_digest = crate::pending::binding_digest(
            &binding.invocation_id,
            &binding.address,
            &binding.method,
            &binding.descriptor_identity,
            &binding.arguments_digest,
        );
    }
    assert!(matches!(
        store.revise_binding_candidates(&session, planned.oid, permuted, 102, &lock),
        Err(SessionError::Conflict(_))
    ));
}

#[test]
fn candidate_revision_executor_verifies_the_request_against_the_pinned_tip() {
    let (_fixture, store, session) = setup();
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
    let original = one_binding_plan();
    let planned = store
        .advance_pending_turn(
            &session,
            PendingStage::BindingPlanned {
                plan: original.clone(),
                usage: None,
            },
            101,
            &lock,
        )
        .expect("planned");

    let request = crate::pending::CandidateRevisionRequest {
        session_id: session_id(),
        turn_id: turn_id(),
        plan: original.clone(),
        colliding_candidates: vec![original.bindings[0].invocation_id],
    };

    // A request describing a different durable plan never writes.
    let mut foreign = request.clone();
    foreign.plan.bindings[0].descriptor_identity = "e".repeat(64);
    assert!(matches!(
        store.execute_candidate_revision(&foreign, &planned, 102, &lock, || invocation_id(9)),
        Err(SessionError::Conflict(_))
    ));

    // A generator that returns an existing candidate never writes.
    assert!(matches!(
        store.execute_candidate_revision(&request, &planned, 102, &lock, || original.bindings[0]
            .invocation_id),
        Err(SessionError::Conflict(_))
    ));
    assert_eq!(
        store
            .read_active_turn(&session)
            .expect("read")
            .expect("active")
            .oid,
        planned.oid,
        "a rejected revision leaves the durable BindingPlanned untouched"
    );

    // The accepted revision replaces the candidate and its derived digest
    // while every structural field stays byte-identical.
    let revised = store
        .execute_candidate_revision(&request, &planned, 102, &lock, || invocation_id(9))
        .expect("revision");
    let PendingStage::BindingPlanned { plan, .. } = &revised.snapshot.stage else {
        panic!("still BindingPlanned");
    };
    assert_eq!(plan.bindings[0].invocation_id, invocation_id(9));
    assert_eq!(plan.bindings[0].address, original.bindings[0].address);
    assert_eq!(plan.bindings[0].method, original.bindings[0].method);
    assert_eq!(
        plan.bindings[0].arguments_digest,
        original.bindings[0].arguments_digest
    );
    assert_ne!(
        plan.bindings[0].binding_digest,
        original.bindings[0].binding_digest
    );
    assert_eq!(plan.message, original.message);
}

#[test]
fn candidate_revision_recovers_a_published_but_undurable_revision_without_reusing_uuids() {
    // A CAS-published revision whose durability barrier failed must be
    // recoverable by a caller whose UUID source is a normal random generator:
    // recovery recognizes the published revision structurally, so it never
    // depends on re-minting the same candidates.
    let (_fixture, store, session) = setup();
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
    let original = one_binding_plan();
    let planned = store
        .advance_pending_turn(
            &session,
            PendingStage::BindingPlanned {
                plan: original.clone(),
                usage: None,
            },
            101,
            &lock,
        )
        .expect("planned");

    let request = crate::pending::CandidateRevisionRequest {
        session_id: session_id(),
        turn_id: turn_id(),
        plan: original.clone(),
        colliding_candidates: vec![original.bindings[0].invocation_id],
    };

    lattice_store::fault::fail_ref_fsync(true);
    assert!(matches!(
        store.execute_candidate_revision(&request, &planned, 102, &lock, || invocation_id(2)),
        Err(SessionError::Store(
            lattice_store::StoreError::Durability { .. }
        ))
    ));

    // A different generator: recovery must still return the published revision.
    let mut never_used = 0;
    let recovered = store
        .execute_candidate_revision(&request, &planned, 102, &lock, || {
            never_used += 1;
            invocation_id(7)
        })
        .expect("recovered the published revision");
    assert_eq!(never_used, 0, "recovery minted no candidate");
    let PendingStage::BindingPlanned { plan, .. } = &recovered.snapshot.stage else {
        panic!("still BindingPlanned");
    };
    assert_eq!(plan.bindings[0].invocation_id, invocation_id(2));
    assert_eq!(
        recovered.snapshot.predecessor.as_deref(),
        Some(planned.oid.to_string().as_str())
    );
}

#[test]
fn the_journal_outcome_contract_never_grows_legacy_outcome_fields() {
    // F1 compile/shape guard: the pinned candidate — the journal's outcome
    // authority — must never carry the fields #139-B deletes. A future
    // change that reintroduces one fails here instead of silently creating a
    // second outcome authority.
    let candidate = completed_candidate(None);
    let value = serde_json::to_value(&candidate).expect("candidate json");
    let object = value.as_object().expect("object");
    for forbidden in ["disposition", "lifecycle", "terminal_reason", "status"] {
        assert!(
            !object.contains_key(forbidden),
            "TurnCommitCandidate must not carry '{forbidden}': {value:#}"
        );
    }
    assert!(object.contains_key("outcome") && object.contains_key("origin"));
}
