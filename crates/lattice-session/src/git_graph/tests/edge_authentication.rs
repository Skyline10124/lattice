// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use gix::objs::tree::EntryKind;
use lattice_conversation::{TokenUsage, ToolResultOutcome};
use lattice_store::{Blocking, FlockGuard, LockMode, RefExpectation, TreeSpec};

use super::*;
use crate::SessionError;
use crate::git_graph::GitSessionGraphStore;
use crate::pending::{
    ClosedBinding, ClosureOrigin, MaterialAvailability, PendingSnapshot, PendingStage,
    StepClosedKind, StepClosingState, TurnClosingFacts, TurnFailure, TurnFinishReason, TurnOutcome,
    TurnUsageAggregate, UnavailableMaterialReason, WorkspaceHistoryCheckpoint,
    step_conversation_digest,
};

use super::pending_fixtures::{
    SESSION, TURN, assistant_text, attempt_key, invocation_id, one_binding_plan, session_id,
    turn_id, turn_started_facts,
};

fn closed_tip(store: &GitSessionGraphStore, lock: &FlockGuard) -> crate::PendingTurnTip {
    store
        .begin_pending_turn(turn_started_facts(None), lock)
        .unwrap();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::StepClosed {
                kind: StepClosedKind::FinishTurn {
                    final_message: assistant_text("done"),
                },
            },
            99,
            lock,
        )
        .unwrap()
}

fn candidate(tip: &crate::PendingTurnTip) -> crate::pending::TurnCommitCandidate {
    tip.snapshot
        .turn_commit_candidate_basis(
            TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop,
            },
            ClosureOrigin::Live,
        )
        .unwrap()
        .with_workspace_history_checkpoint(WorkspaceHistoryCheckpoint::Empty)
        .unwrap()
}

fn forge_active_snapshot(
    store: &GitSessionGraphStore,
    predecessor: &crate::PendingTurnTip,
    snapshot: &PendingSnapshot,
) {
    let bytes = crate::git_graph::pending::canonical_snapshot_bytes(snapshot).unwrap();
    let blob = store.kernel().write_blob(&bytes).unwrap();
    let tree = store
        .kernel()
        .write_tree(TreeSpec::new().upsert("pending.json", EntryKind::Blob, blob))
        .unwrap();
    let forged_oid = store
        .kernel()
        .write_commit_at(tree, &[predecessor.oid], "forged edge", 100)
        .unwrap();
    store
        .kernel()
        .cas_ref(
            &format!("refs/lattice/sessions/{SESSION}/active-turn"),
            forged_oid,
            RefExpectation::MustMatch(predecessor.oid),
            "forge unauthenticated edge",
        )
        .unwrap();
}

fn turn_closing_snapshot(
    predecessor: &crate::PendingTurnTip,
    candidate: crate::pending::TurnCommitCandidate,
) -> PendingSnapshot {
    PendingSnapshot {
        schema_version: crate::pending::PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: Some(predecessor.oid.to_string()),
        post_user_transcript: predecessor.snapshot.post_user_transcript.clone(),
        stage: PendingStage::TurnClosing(TurnClosingFacts {
            turn_id: turn_id(),
            candidate,
        }),
        recorded_at: None,
        root: None,
        attempt_usages: Vec::new(),
    }
}

fn assert_reopen_rejects_turn_closing_forgery(
    name: &str,
    mutate: impl FnOnce(&mut crate::pending::TurnCommitCandidate),
) {
    let fixture = temp_dir(name);
    let path = fixture.path().join("sessions.git");
    let store = GitSessionGraphStore::create(&path).unwrap();
    store
        .create_session(SESSION, None, "coder", "main", &test_project_key())
        .unwrap();
    let lock = store
        .lock_execution(SESSION, LockMode::Exclusive, Blocking::Block)
        .unwrap();
    let closed = closed_tip(&store, &lock);
    let mut forged = candidate(&closed);
    mutate(&mut forged);
    forge_active_snapshot(&store, &closed, &turn_closing_snapshot(&closed, forged));
    drop(lock);
    drop(store);

    let error = GitSessionGraphStore::open(path)
        .expect_err("strict reopen must authenticate every TurnClosing candidate field");
    assert!(matches!(error, SessionError::JournalCorruption { .. }));
}

#[test]
fn append_rejects_a_turn_closing_candidate_not_derived_from_its_predecessor() {
    let fixture = temp_dir("append-edge-authentication");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).unwrap();
    store
        .create_session(SESSION, None, "coder", "main", &test_project_key())
        .unwrap();
    let lock = store
        .lock_execution(SESSION, LockMode::Exclusive, Blocking::Block)
        .unwrap();
    let closed = closed_tip(&store, &lock);
    let mut forged = candidate(&closed);
    forged.provider.push_str("-forged");

    let error = store
        .advance_pending_turn(
            SESSION,
            PendingStage::TurnClosing(TurnClosingFacts {
                turn_id: turn_id(),
                candidate: forged,
            }),
            100,
            &lock,
        )
        .expect_err("append must authenticate the candidate against StepClosed");
    assert!(matches!(error, SessionError::Conflict(_)));
    assert_eq!(
        store.read_active_turn(SESSION).unwrap().unwrap().oid,
        closed.oid
    );
}

#[test]
fn strict_reopen_rejects_forged_turn_closing_authorities() {
    assert_reopen_rejects_turn_closing_forgery("forged-turn-outcome", |candidate| {
        candidate.outcome = TurnOutcome::Failed {
            reason: TurnFailure::ModelRequestFailed,
        };
    });
    assert_reopen_rejects_turn_closing_forgery("forged-turn-transcript", |candidate| {
        candidate.messages[1] = crate::test_helpers::assistant_msg("forged");
    });
    assert_reopen_rejects_turn_closing_forgery("forged-turn-profile-hash", |candidate| {
        candidate.profile_snapshot_hash.push_str("-forged");
    });
    assert_reopen_rejects_turn_closing_forgery("forged-turn-profile-json", |candidate| {
        candidate.profile_snapshot_json = "{\"forged\":true}".into();
    });
    assert_reopen_rejects_turn_closing_forgery("forged-turn-model", |candidate| {
        candidate.model.push_str("-forged");
    });
    assert_reopen_rejects_turn_closing_forgery("forged-turn-usage", |candidate| {
        candidate.usage_aggregate = Some(TurnUsageAggregate::Complete {
            usage: TokenUsage {
                prompt_tokens: 1,
                completion_tokens: 1,
                total_tokens: 2,
            },
        });
    });
    assert_reopen_rejects_turn_closing_forgery("forged-turn-timestamp", |candidate| {
        candidate.created_at = "1970-01-01T00:01:41+00:00".into();
    });
}

#[test]
fn strict_reopen_rejects_a_candidate_revision_that_changes_attempt_usage() {
    let fixture = temp_dir("forged-revision-usage");
    let path = fixture.path().join("sessions.git");
    let store = GitSessionGraphStore::create(&path).unwrap();
    store
        .create_session(SESSION, None, "coder", "main", &test_project_key())
        .unwrap();
    let lock = store
        .lock_execution(SESSION, LockMode::Exclusive, Blocking::Block)
        .unwrap();
    store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .unwrap();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 0),
            },
            99,
            &lock,
        )
        .unwrap();
    let original = one_binding_plan();
    let planned = store
        .advance_pending_turn(
            SESSION,
            PendingStage::BindingPlanned {
                plan: original.clone(),
                usage: Some(TokenUsage {
                    prompt_tokens: 1,
                    completion_tokens: 2,
                    total_tokens: 3,
                }),
            },
            100,
            &lock,
        )
        .unwrap();

    let mut revised = original;
    revised.bindings[0].invocation_id = invocation_id(2);
    revised.bindings[0].binding_digest = crate::pending::binding_digest(
        &revised.bindings[0].invocation_id,
        &revised.bindings[0].address,
        &revised.bindings[0].method,
        &revised.bindings[0].descriptor_identity,
        &revised.bindings[0].arguments_digest,
    );
    let forged = PendingSnapshot {
        schema_version: crate::pending::PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: Some(planned.oid.to_string()),
        post_user_transcript: vec![revised.bound_message().unwrap()],
        stage: PendingStage::BindingPlanned {
            plan: revised,
            usage: None,
        },
        recorded_at: None,
        root: None,
        attempt_usages: Vec::new(),
    };
    forge_active_snapshot(&store, &planned, &forged);
    drop(lock);
    drop(store);

    let error = GitSessionGraphStore::open(path)
        .expect_err("strict reopen must reject revision usage drift");
    assert!(matches!(error, SessionError::JournalCorruption { .. }));
}

fn assert_reopen_rejects_continue_model_forgery(
    name: &str,
    mutate: impl FnOnce(&mut StepClosedKind),
) {
    let fixture = temp_dir(name);
    let path = fixture.path().join("sessions.git");
    let store = GitSessionGraphStore::create(&path).unwrap();
    store
        .create_session(SESSION, None, "coder", "main", &test_project_key())
        .unwrap();
    let lock = store
        .lock_execution(SESSION, LockMode::Exclusive, Blocking::Block)
        .unwrap();
    store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .unwrap();
    let plan = one_binding_plan();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 0),
            },
            96,
            &lock,
        )
        .unwrap();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::BindingPlanned {
                plan: plan.clone(),
                usage: None,
            },
            97,
            &lock,
        )
        .unwrap();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::Bound { plan: plan.clone() },
            98,
            &lock,
        )
        .unwrap();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::StepClosing(StepClosingState {
                plan_key: plan.plan_key,
                bindings: plan.bindings.clone(),
                closure_cursor: 0,
                closures: Vec::new(),
                current_candidate: None,
            }),
            99,
            &lock,
        )
        .unwrap();
    let closure = ClosedBinding {
        binding: plan.bindings[0].clone(),
        outcome: ToolResultOutcome::NotExecuted,
        material: MaterialAvailability::Unavailable {
            reason: UnavailableMaterialReason::NotExecuted,
            digest: None,
            byte_length: None,
        },
    };
    let closed = store
        .advance_pending_turn(
            SESSION,
            PendingStage::StepClosing(StepClosingState {
                plan_key: plan.plan_key,
                bindings: plan.bindings.clone(),
                closure_cursor: 1,
                closures: vec![closure.clone()],
                current_candidate: None,
            }),
            100,
            &lock,
        )
        .unwrap();
    let mut kind = StepClosedKind::ContinueModel {
        model_step_ordinal: 0,
        ordered_tool_results: vec![closure.clone()],
        conversation_digest: step_conversation_digest(
            &plan.bound_message().unwrap(),
            std::slice::from_ref(&closure),
        ),
    };
    mutate(&mut kind);
    let forged = PendingSnapshot {
        schema_version: crate::pending::PENDING_SNAPSHOT_SCHEMA_VERSION,
        session_id: session_id(),
        turn_id: turn_id(),
        predecessor: Some(closed.oid.to_string()),
        post_user_transcript: closed.snapshot.post_user_transcript.clone(),
        stage: PendingStage::StepClosed { kind },
        recorded_at: None,
        root: None,
        attempt_usages: Vec::new(),
    };
    forge_active_snapshot(&store, &closed, &forged);
    drop(lock);
    drop(store);

    let error = GitSessionGraphStore::open(path)
        .expect_err("strict reopen must authenticate ContinueModel closure authority");
    assert!(matches!(error, SessionError::JournalCorruption { .. }));
}

#[test]
fn strict_reopen_rejects_forged_continue_model_closures_and_digest() {
    assert_reopen_rejects_continue_model_forgery("forged-continue-closure", |kind| {
        let StepClosedKind::ContinueModel {
            ordered_tool_results,
            conversation_digest,
            ..
        } = kind
        else {
            unreachable!()
        };
        ordered_tool_results[0].material = MaterialAvailability::Unavailable {
            reason: UnavailableMaterialReason::DraftPublicationFailed,
            digest: None,
            byte_length: None,
        };
        *conversation_digest = step_conversation_digest(
            &one_binding_plan().bound_message().unwrap(),
            ordered_tool_results,
        );
    });
    assert_reopen_rejects_continue_model_forgery("forged-continue-digest", |kind| {
        let StepClosedKind::ContinueModel {
            conversation_digest,
            ..
        } = kind
        else {
            unreachable!()
        };
        *conversation_digest = [9; 32];
    });
}

#[test]
fn saturated_usage_candidate_basis_is_identical_live_reopen_and_exact_retry() {
    let fixture = temp_dir("saturated-usage-exact-retry");
    let path = fixture.path().join("sessions.git");
    let store = GitSessionGraphStore::create(&path).unwrap();
    store
        .create_session(SESSION, None, "coder", "main", &test_project_key())
        .unwrap();
    let lock = store
        .lock_execution(SESSION, LockMode::Exclusive, Blocking::Block)
        .unwrap();
    store
        .begin_pending_turn(turn_started_facts(None), &lock)
        .unwrap();

    let saturated = TokenUsage {
        prompt_tokens: u64::MAX,
        completion_tokens: u64::MAX - 1,
        total_tokens: u64::MAX - 2,
    };
    let overflow = TokenUsage {
        prompt_tokens: 1,
        completion_tokens: 2,
        total_tokens: 3,
    };
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 0),
            },
            100,
            &lock,
        )
        .unwrap();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::ModelAttemptFailed {
                key: attempt_key(0, 0),
                retryable: true,
                usage: Some(saturated),
            },
            101,
            &lock,
        )
        .unwrap();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 1),
            },
            102,
            &lock,
        )
        .unwrap();
    store
        .advance_pending_turn(
            SESSION,
            PendingStage::MessageComplete {
                key: attempt_key(0, 1),
                message: assistant_text("done"),
                usage: Some(overflow),
            },
            103,
            &lock,
        )
        .unwrap();

    let stage = PendingStage::StepClosed {
        kind: StepClosedKind::FinishTurn {
            final_message: assistant_text("done"),
        },
    };
    lattice_store::fault::fail_ref_fsync(true);
    let error = store.advance_pending_turn(SESSION, stage.clone(), 104, &lock);
    assert!(matches!(
        error,
        Err(SessionError::Store(
            lattice_store::StoreError::Durability { .. }
        ))
    ));
    let published = store.read_active_turn(SESSION).unwrap().unwrap();
    let expected_attempts = vec![Some(saturated), Some(overflow)];
    assert_eq!(published.snapshot.attempt_usages, expected_attempts);
    assert_eq!(published.snapshot.turn_id.to_string(), TURN);
    let live_basis = published
        .snapshot
        .turn_commit_candidate_basis(
            TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop,
            },
            ClosureOrigin::Live,
        )
        .unwrap();
    assert_eq!(
        live_basis.usage_aggregate,
        Some(TurnUsageAggregate::Complete {
            usage: TokenUsage {
                prompt_tokens: u64::MAX,
                completion_tokens: u64::MAX,
                total_tokens: u64::MAX,
            },
        })
    );
    drop(lock);
    drop(store);

    let reopened = GitSessionGraphStore::open(&path).unwrap();
    let reopened_tip = reopened.read_active_turn(SESSION).unwrap().unwrap();
    let reopened_basis = reopened_tip
        .snapshot
        .turn_commit_candidate_basis(
            TurnOutcome::Completed {
                finish_reason: TurnFinishReason::Stop,
            },
            ClosureOrigin::Live,
        )
        .unwrap();
    assert_eq!(reopened_basis, live_basis);
    assert_eq!(reopened_tip.snapshot.attempt_usages, expected_attempts);

    let lock = reopened
        .lock_execution(SESSION, LockMode::Exclusive, Blocking::Block)
        .unwrap();
    let retried = reopened
        .advance_pending_turn(SESSION, stage, 104, &lock)
        .expect("exact retry confirms the published snapshot");
    assert_eq!(retried.oid, published.oid);
    assert_eq!(retried.snapshot.attempt_usages, expected_attempts);
    assert_eq!(
        retried
            .snapshot
            .turn_commit_candidate_basis(
                TurnOutcome::Completed {
                    finish_reason: TurnFinishReason::Stop,
                },
                ClosureOrigin::Live,
            )
            .unwrap(),
        live_basis
    );
    drop(lock);
    drop(reopened);

    let reopened_again = GitSessionGraphStore::open(path).unwrap();
    assert_eq!(
        reopened_again
            .read_active_turn(SESSION)
            .unwrap()
            .unwrap()
            .snapshot
            .attempt_usages,
        expected_attempts
    );
}
