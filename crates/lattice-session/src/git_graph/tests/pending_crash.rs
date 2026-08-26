// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Crash-window fixtures for the PendingTurn journal (GitHub #144): every
//! durable stage recovers through the pure reducer, injected durability
//! faults leave the journal at the last published tip, and executing a
//! reconciliation plan against the store commits the recovered Turn with
//! the journal chain reachable from its permanent pin.
//!
//! Fault injection demonstrates call ordering and classification; it is not
//! a claim of real power-loss consistency (ADR 0005 §18). "Crash" is
//! simulated by dropping to a fresh `GitSessionGraphStore` handle on the
//! same repository (strict reopen, no cached refs).

use lattice_store::{Blocking, LockMode};

use crate::SessionError;
use crate::git_graph::GitSessionGraphStore;
use crate::git_graph::tests::{Fixture, temp_dir};
use crate::graph::SessionGraphStore;
use crate::pending::PendingSnapshot;
use crate::pending::ResultDraft;
use crate::pending::contract::{PendingStage, TurnClosingFacts};
use crate::pending::facts::{
    ExecutionOutcome, InvocationFactsSnapshot, InvocationLifecycleFact, InvocationRecordFact,
    PlanFacts, ResultMaterial, UnavailableMaterialReason,
};
use crate::pending::outcome::{ClosureOrigin, TurnInterruption, TurnOutcome};
use crate::pending::reconcile::{ReconciliationAction, ReconciliationPlan, reconcile};
use crate::pending::step::{MaterialAvailability, StepClosingState};

use super::pending_fixtures::*;

/// Drive the journal through one legal stage sequence and hand back the
/// locked store for whichever stage the caller asked to crash at.
struct JournalDriver<'a> {
    store: &'a GitSessionGraphStore,
    session: &'a str,
    lock: lattice_store::FlockGuard,
}

impl<'a> JournalDriver<'a> {
    fn new(store: &'a GitSessionGraphStore, session: &'a str) -> Self {
        store
            .create_session(session, None, "coder", "main", &super::test_project_key())
            .expect("session");
        let lock = store
            .lock_execution(session, LockMode::Exclusive, Blocking::Block)
            .expect("lock");
        Self {
            store,
            session,
            lock,
        }
    }

    fn begin(&self) -> crate::git_graph::pending::PendingTurnTip {
        self.store
            .begin_pending_turn(turn_started_facts(None), &self.lock)
            .expect("begin")
    }

    fn advance(&self, stage: PendingStage) -> crate::git_graph::pending::PendingTurnTip {
        let tip = self
            .store
            .read_active_turn(self.session)
            .expect("tip")
            .expect("active turn");
        let recorded_at = tip.snapshot.recorded_at.unwrap_or(0);
        let stage = match stage {
            PendingStage::TurnClosing(facts) => {
                let candidate = tip
                    .snapshot
                    .turn_commit_candidate_basis(
                        facts.candidate.outcome.clone(),
                        facts.candidate.origin,
                    )
                    .expect("derive exact closing basis")
                    .with_workspace_history_checkpoint(
                        facts.candidate.workspace_history_checkpoint.clone(),
                    )
                    .expect("attach checkpoint");
                PendingStage::TurnClosing(TurnClosingFacts {
                    turn_id: tip.snapshot.turn_id,
                    candidate,
                })
            }
            other => other,
        };
        self.store
            .advance_pending_turn(self.session, stage, recorded_at, &self.lock)
            .expect("advance")
    }
}

fn drive_to_empty_step_closing(
    store: &GitSessionGraphStore,
    session: &str,
) -> (lattice_store::FlockGuard, crate::pending::BindingPlan) {
    let driver = JournalDriver::new(store, session);
    driver.begin();
    let plan = one_binding_plan();
    driver.advance(PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    });
    driver.advance(PendingStage::BindingPlanned {
        plan: plan.clone(),
        usage: None,
    });
    driver.advance(PendingStage::Bound { plan: plan.clone() });
    driver.advance(PendingStage::StepClosing(StepClosingState {
        plan_key: plan.plan_key,
        bindings: plan.bindings.clone(),
        closure_cursor: 0,
        closures: Vec::new(),
        current_candidate: None,
    }));
    (driver.lock, plan)
}

fn candidate_draft(plan: &crate::pending::BindingPlan) -> ResultDraft {
    let binding = &plan.bindings[0];
    ResultDraft::new(
        binding.invocation_id,
        binding.binding_digest,
        turn_id(),
        binding.address.message,
        0,
        "{\"rows\":[]}".to_string(),
    )
    .expect("draft")
}

/// Reopen semantics: a fresh handle validates the same repository strict,
/// with no cached refs.
fn reopen(fixture: &Fixture) -> GitSessionGraphStore {
    GitSessionGraphStore::open(fixture.path().join("sessions.git")).expect("strict reopen")
}

/// Reconcile and require a closure decision. A crash fixture that reduced
/// to a candidate revision would mean the Turn is not closing at all,
/// which no fixture here describes.
fn reconcile_closure(
    snapshot: &PendingSnapshot,
    facts: &InvocationFactsSnapshot,
    what: &str,
) -> ReconciliationPlan {
    reconcile(snapshot, facts)
        .unwrap_or_else(|e| panic!("{what}: {e:?}"))
        .closure()
        .unwrap_or_else(|| panic!("{what}: expected a closure, got a candidate revision"))
        .clone()
}

/// Execute a plan's journal actions against the store; facts-port actions
/// apply to the synthetic facts snapshot (closure-only, never dispatch).
fn execute_plan(
    store: &GitSessionGraphStore,
    session: &str,
    lock: &lattice_store::FlockGuard,
    plan: &[ReconciliationAction],
    facts: &mut InvocationFactsSnapshot,
) -> Result<(), SessionError> {
    for action in plan {
        let tip = store
            .read_active_turn(session)?
            .ok_or_else(|| SessionError::Conflict("no active turn left".into()))?;
        let recorded_at = tip.snapshot.recorded_at.unwrap_or(0);
        match action {
            ReconciliationAction::ReserveExactPlan { plan_key, bindings } => {
                facts.plan = Some(PlanFacts {
                    plan_key: *plan_key,
                    bindings: bindings.clone(),
                });
                for binding in bindings {
                    facts.invocations.entry(binding.invocation_id).or_insert(
                        InvocationRecordFact {
                            owner: *plan_key,
                            lifecycle: InvocationLifecycleFact::Reserved,
                        },
                    );
                }
            }
            ReconciliationAction::TerminalizeInvocation {
                invocation,
                outcome,
                material,
                ..
            } => {
                let owner = facts.plan.as_ref().expect("plan set").plan_key;
                facts.invocations.insert(
                    *invocation,
                    InvocationRecordFact {
                        owner,
                        lifecycle: InvocationLifecycleFact::Terminal {
                            outcome: outcome.clone(),
                            material: material.clone(),
                        },
                    },
                );
            }
            ReconciliationAction::PublishBound { plan } => {
                let _ = store.advance_pending_turn(
                    session,
                    PendingStage::Bound { plan: plan.clone() },
                    recorded_at,
                    lock,
                )?;
            }
            ReconciliationAction::AdvanceStepClosing { state } => {
                let _ = store.advance_pending_turn(
                    session,
                    PendingStage::StepClosing(state.clone()),
                    recorded_at,
                    lock,
                )?;
            }
            ReconciliationAction::PublishStepClosed { kind } => {
                let _ = store.advance_pending_turn(
                    session,
                    PendingStage::StepClosed { kind: kind.clone() },
                    recorded_at,
                    lock,
                )?;
            }
            ReconciliationAction::PublishTurnClosing { candidate } => {
                let candidate = candidate.clone().with_workspace_history_checkpoint(
                    crate::pending::WorkspaceHistoryCheckpoint::Empty,
                )?;
                let _ = store.advance_pending_turn(
                    session,
                    PendingStage::TurnClosing(TurnClosingFacts {
                        turn_id: tip.snapshot.turn_id,
                        candidate,
                    }),
                    recorded_at,
                    lock,
                )?;
            }
            ReconciliationAction::FinalizeTurn => {
                let _ = store.finalize_turn(session, &tip.snapshot.turn_id, lock)?;
            }
        };
    }
    Ok(())
}

#[test]
fn every_stage_crash_window_recovers_one_deterministic_closure() {
    // One repository per stage so the matrix exercises independent crashes.
    for case in stage_cases() {
        let fixture = temp_dir("pending-crash-matrix");
        let store =
            GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
        let session = SESSION.to_string();
        let driver = JournalDriver::new(&store, &session);
        driver.begin();
        for stage in case.stages_to_crash_at {
            driver.advance(stage);
        }
        drop(driver);

        // Crash: reopen with a fresh handle and reconcile.
        let reopened = reopen(&fixture);
        let tip = reopened
            .read_active_turn(&session)
            .expect("read")
            .expect("active turn");
        let plan = reconcile_closure(&tip.snapshot, &case.facts, "one closure");
        assert_eq!(plan.outcome, case.expected_outcome, "stage {}", case.name);
        assert_eq!(plan.origin, ClosureOrigin::Reconciled);
        // Deterministic: the same inputs produce the same plan.
        let again = reconcile_closure(&tip.snapshot, &case.facts, "repeat");
        assert_eq!(plan, again);
    }
}

struct StageCase {
    name: &'static str,
    stages_to_crash_at: Vec<PendingStage>,
    facts: InvocationFactsSnapshot,
    expected_outcome: TurnOutcome,
}

fn stage_cases() -> Vec<StageCase> {
    let plan_one = one_binding_plan();
    let interrupted = |reason| TurnOutcome::Interrupted { reason };
    vec![
        StageCase {
            name: "turn_started",
            stages_to_crash_at: vec![],
            facts: InvocationFactsSnapshot::empty(),
            expected_outcome: interrupted(TurnInterruption::ProcessInterruptedBeforeModelAttempt),
        },
        StageCase {
            name: "model_attempt_started",
            stages_to_crash_at: vec![PendingStage::ModelAttemptStarted {
                key: attempt_key(0, 0),
            }],
            facts: InvocationFactsSnapshot::empty(),
            expected_outcome: interrupted(TurnInterruption::ProcessInterruptedDuringModelAttempt),
        },
        StageCase {
            name: "model_attempt_failed_retryable",
            stages_to_crash_at: vec![
                PendingStage::ModelAttemptStarted {
                    key: attempt_key(0, 0),
                },
                PendingStage::ModelAttemptFailed {
                    key: attempt_key(0, 0),
                    retryable: true,
                    usage: None,
                },
            ],
            facts: InvocationFactsSnapshot::empty(),
            expected_outcome: interrupted(TurnInterruption::ProcessInterruptedDuringModelRetry),
        },
        StageCase {
            name: "model_attempt_interrupted",
            stages_to_crash_at: vec![
                PendingStage::ModelAttemptStarted {
                    key: attempt_key(0, 0),
                },
                PendingStage::ModelAttemptInterrupted {
                    key: attempt_key(0, 0),
                    reason: lattice_conversation::AttemptInterruption::StreamError,
                    usage: None,
                },
            ],
            facts: InvocationFactsSnapshot::empty(),
            expected_outcome: interrupted(TurnInterruption::ModelAttempt(
                lattice_conversation::AttemptInterruption::StreamError,
            )),
        },
        StageCase {
            name: "model_attempt_rejected",
            stages_to_crash_at: vec![
                PendingStage::ModelAttemptStarted {
                    key: attempt_key(0, 0),
                },
                PendingStage::ModelAttemptRejected {
                    key: attempt_key(0, 0),
                    rejection: lattice_conversation::AttemptRejection::InvalidToolCall {
                        validation: lattice_conversation::ToolCallValidation::MalformedArguments {
                            call_ordinal: 0,
                        },
                    },
                    usage: None,
                },
            ],
            facts: InvocationFactsSnapshot::empty(),
            expected_outcome: TurnOutcome::Failed {
                reason: crate::pending::TurnFailure::InvalidToolCall {
                    validation: lattice_conversation::ToolCallValidation::MalformedArguments {
                        call_ordinal: 0,
                    },
                },
            },
        },
        StageCase {
            name: "message_complete",
            stages_to_crash_at: vec![
                PendingStage::ModelAttemptStarted {
                    key: attempt_key(0, 0),
                },
                PendingStage::MessageComplete {
                    key: attempt_key(0, 0),
                    message: assistant_text("done"),
                    usage: None,
                },
            ],
            facts: InvocationFactsSnapshot::empty(),
            expected_outcome: TurnOutcome::Completed {
                finish_reason: crate::pending::TurnFinishReason::Stop,
            },
        },
        StageCase {
            name: "binding_planned_absent_plan",
            stages_to_crash_at: vec![
                PendingStage::ModelAttemptStarted {
                    key: attempt_key(0, 0),
                },
                PendingStage::BindingPlanned {
                    plan: plan_one.clone(),
                    usage: None,
                },
            ],
            facts: InvocationFactsSnapshot::empty(),
            expected_outcome: interrupted(TurnInterruption::ProcessInterruptedDuringToolBinding),
        },
        StageCase {
            name: "bound_reserved_no_dispatch",
            stages_to_crash_at: vec![
                PendingStage::ModelAttemptStarted {
                    key: attempt_key(0, 0),
                },
                PendingStage::BindingPlanned {
                    plan: plan_one.clone(),
                    usage: None,
                },
                PendingStage::Bound {
                    plan: plan_one.clone(),
                },
            ],
            facts: reserved_facts(&plan_one),
            expected_outcome: interrupted(TurnInterruption::ProcessInterruptedDuringToolBinding),
        },
        StageCase {
            name: "bound_dispatch_started_no_terminal",
            stages_to_crash_at: vec![
                PendingStage::ModelAttemptStarted {
                    key: attempt_key(0, 0),
                },
                PendingStage::BindingPlanned {
                    plan: plan_one.clone(),
                    usage: None,
                },
                PendingStage::Bound {
                    plan: plan_one.clone(),
                },
            ],
            facts: dispatch_started_facts(&plan_one),
            expected_outcome: interrupted(TurnInterruption::InvocationMayHaveOccurred {
                invocation: plan_one.bindings[0].invocation_id,
            }),
        },
        StageCase {
            name: "turn_closing",
            stages_to_crash_at: vec![
                PendingStage::ModelAttemptStarted {
                    key: attempt_key(0, 0),
                },
                PendingStage::MessageComplete {
                    key: attempt_key(0, 0),
                    message: assistant_text("done"),
                    usage: None,
                },
                PendingStage::StepClosed {
                    kind: crate::pending::StepClosedKind::FinishTurn {
                        final_message: assistant_text("done"),
                    },
                },
                PendingStage::TurnClosing(turn_closing(completed_candidate(None))),
            ],
            facts: InvocationFactsSnapshot::empty(),
            expected_outcome: TurnOutcome::Completed {
                finish_reason: crate::pending::TurnFinishReason::Stop,
            },
        },
    ]
}

fn reserved_facts(plan: &crate::pending::BindingPlan) -> InvocationFactsSnapshot {
    let mut facts = InvocationFactsSnapshot::empty();
    facts.plan = Some(PlanFacts {
        plan_key: plan.plan_key,
        bindings: plan.bindings.clone(),
    });
    for binding in &plan.bindings {
        facts.invocations.insert(
            binding.invocation_id,
            InvocationRecordFact {
                owner: plan.plan_key,
                lifecycle: InvocationLifecycleFact::Reserved,
            },
        );
    }
    facts
}

fn dispatch_started_facts(plan: &crate::pending::BindingPlan) -> InvocationFactsSnapshot {
    let mut facts = reserved_facts(plan);
    facts.invocations.insert(
        plan.bindings[0].invocation_id,
        InvocationRecordFact {
            owner: plan.plan_key,
            lifecycle: InvocationLifecycleFact::DispatchStarted,
        },
    );
    facts
}

#[test]
fn crash_during_advance_keeps_the_previous_tip_and_retry_is_finite() {
    let fixture = temp_dir("pending-crash-advance");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let driver = JournalDriver::new(&store, &session);
    let begun = driver.begin();
    drop(driver);

    let lock = store
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    // Crash at the object durability barrier of the advance: the CAS never
    // runs, so the journal still points at the last published tip.
    lattice_store::fault::fail_object_fsync(true);
    let err = store.advance_pending_turn(
        &session,
        PendingStage::ModelAttemptStarted {
            key: attempt_key(0, 0),
        },
        100,
        &lock,
    );
    assert!(
        matches!(
            err,
            Err(SessionError::Store(
                lattice_store::StoreError::Durability { .. }
            ))
        ),
        "got {err:?}"
    );
    let tip = store
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    assert_eq!(tip.oid, begun.oid, "previous tip remains published");

    // Reopen after the crash: the plan is the TurnStarted closure.
    let reopened = reopen(&fixture);
    let tip = reopened
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    let plan = reconcile_closure(&tip.snapshot, &InvocationFactsSnapshot::empty(), "plan");
    assert!(matches!(
        plan.outcome,
        TurnOutcome::Interrupted {
            reason: TurnInterruption::ProcessInterruptedBeforeModelAttempt
        }
    ));
}

#[test]
fn step_closing_candidate_pin_object_barrier_failure_does_not_publish_the_candidate() {
    // ADR 0005 §12: the exact ResultDraft candidate is pinned in a durable
    // StepClosing snapshot before any draft write. An object-barrier failure
    // during that pin must not publish the candidate; the previous empty
    // StepClosing tip remains, and classify_draft stays CleanAbsent.
    let fixture = temp_dir("pending-crash-pin-object");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let (lock, plan) = drive_to_empty_step_closing(&store, &session);
    let before = store
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    let draft = candidate_draft(&plan);

    lattice_store::fault::fail_object_fsync(true);
    let err = store.advance_pending_turn(
        &session,
        PendingStage::StepClosing(StepClosingState {
            plan_key: plan.plan_key,

            bindings: plan.bindings.clone(),
            closure_cursor: 0,
            closures: Vec::new(),
            current_candidate: Some(draft.clone()),
        }),
        104,
        &lock,
    );
    assert!(
        matches!(
            err,
            Err(SessionError::Store(
                lattice_store::StoreError::Durability { .. }
            ))
        ),
        "got {err:?}"
    );

    let tip = store
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    assert_eq!(tip.oid, before.oid, "the pin CAS must not have run");
    let PendingStage::StepClosing(state) = &tip.snapshot.stage else {
        panic!("expected StepClosing, got {:?}", tip.snapshot.stage);
    };
    assert!(
        state.current_candidate.is_none(),
        "a failed pin must not leave a candidate on the previous tip"
    );

    drop(lock);
    let reopened = reopen(&fixture);
    assert_eq!(
        reopened.classify_draft(&draft).expect("classify"),
        crate::pending::DraftClassification::CleanAbsent
    );
}

#[test]
fn step_closing_candidate_pin_uses_one_object_barrier_and_one_ref_barrier() {
    // `cas_ref` already fsyncs pending objects, then CAS, then the ref.
    // A second caller `fsync_pending` after success is a redundant git_dir
    // fsync and must not be part of the pin. The pin remains durable before
    // any draft write: classify_draft is CleanAbsent until publish_draft.
    let fixture = temp_dir("pending-crash-pin-barriers");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let (lock, plan) = drive_to_empty_step_closing(&store, &session);
    let draft = candidate_draft(&plan);

    lattice_store::fault::reset_barrier_counts();
    store
        .advance_pending_turn(
            &session,
            PendingStage::StepClosing(StepClosingState {
                plan_key: plan.plan_key,

                bindings: plan.bindings.clone(),
                closure_cursor: 0,
                closures: Vec::new(),
                current_candidate: Some(draft.clone()),
            }),
            104,
            &lock,
        )
        .expect("pin candidate");
    assert_eq!(
        lattice_store::fault::object_barrier_count(),
        1,
        "the pin must fsync snapshot objects once, before CAS"
    );
    assert_eq!(
        lattice_store::fault::ref_barrier_count(),
        1,
        "cas_ref already fsyncs the published ref; the pin must not add a second ref barrier"
    );

    assert_eq!(
        store.classify_draft(&draft).expect("classify"),
        crate::pending::DraftClassification::CleanAbsent
    );
    let tip = store
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    let PendingStage::StepClosing(state) = &tip.snapshot.stage else {
        panic!("expected StepClosing, got {:?}", tip.snapshot.stage);
    };
    assert_eq!(
        state.current_candidate.as_ref().map(|c| c.invocation_id),
        Some(plan.bindings[0].invocation_id)
    );
}

#[test]
fn unwritable_finalization_keeps_step_closing_and_active_turn() {
    // FinalizeBudget-exhaustion shape (issue #144): while the store cannot
    // durably publish, the Turn keeps its journal tip and active-turn, and
    // no NotExecuted/StepClosed/TurnCommit appears; the next command only
    // performs finite reconciliation.
    let fixture = temp_dir("pending-crash-finalize");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let driver = JournalDriver::new(&store, &session);
    driver.begin();
    driver.advance(PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    });
    driver.advance(PendingStage::MessageComplete {
        key: attempt_key(0, 0),
        message: assistant_text("done"),
        usage: None,
    });
    driver.advance(PendingStage::StepClosed {
        kind: crate::pending::StepClosedKind::FinishTurn {
            final_message: assistant_text("done"),
        },
    });
    driver.advance(PendingStage::TurnClosing(turn_closing(
        completed_candidate(None),
    )));
    drop(driver);

    {
        let lock = store
            .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
            .expect("lock");
        lattice_store::fault::fail_object_fsync(true);
        let err = store.finalize_turn(&session, &turn_id(), &lock);
        assert!(err.is_err(), "unwritable store fails the finalization");
        // Nothing was published: active-turn present, no pin, no turn
        // commit. Release the guard before the next command reacquires it
        // (independent same-process acquisitions conflict by design).
        assert!(store.read_active_turn(&session).expect("read").is_some());
        assert!(store.read_turn_pin(&session, TURN).expect("pin").is_none());
    }

    // The next command reconciles only, with a finite budget: the plan for
    // the TurnClosing tip is exactly one FinalizeTurn.
    let reopened = reopen(&fixture);
    let tip = reopened
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    let plan = reconcile_closure(&tip.snapshot, &InvocationFactsSnapshot::empty(), "plan");
    assert_eq!(plan.actions, vec![ReconciliationAction::FinalizeTurn]);

    // Once the store is writable again, the exact retry completes.
    let lock = reopened
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let committed = reopened
        .finalize_turn(&session, &turn_id(), &lock)
        .expect("retry completes");
    assert!(reopened.read_active_turn(&session).expect("read").is_none());
    assert_eq!(
        reopened
            .read_turn_pin(&session, TURN)
            .expect("pin")
            .as_deref(),
        Some(committed.commit_id.as_str())
    );
}

#[test]
fn crash_after_final_publication_reports_failure_but_is_idempotent() {
    // A ref-fsync failure after the transaction published: the caller sees
    // a durability error, the transaction is nevertheless complete, and
    // the idempotent retry returns the exact pinned TurnCommit.
    let fixture = temp_dir("pending-crash-post-publish");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let driver = JournalDriver::new(&store, &session);
    driver.begin();
    driver.advance(PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    });
    driver.advance(PendingStage::MessageComplete {
        key: attempt_key(0, 0),
        message: assistant_text("done"),
        usage: None,
    });
    driver.advance(PendingStage::StepClosed {
        kind: crate::pending::StepClosedKind::FinishTurn {
            final_message: assistant_text("done"),
        },
    });
    driver.advance(PendingStage::TurnClosing(turn_closing(
        completed_candidate(None),
    )));
    drop(driver);

    {
        let lock = store
            .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
            .expect("lock");
        lattice_store::fault::fail_ref_fsync(true);
        let err = store.finalize_turn(&session, &turn_id(), &lock);
        assert!(
            matches!(
                err,
                Err(SessionError::Store(
                    lattice_store::StoreError::Durability { .. }
                ))
            ),
            "got {err:?}"
        );
    }

    // Reopen observes the completed transaction and retries idempotently.
    let reopened = reopen(&fixture);
    assert!(reopened.read_active_turn(&session).expect("read").is_none());
    let lock = reopened
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let again = reopened
        .finalize_turn(&session, &turn_id(), &lock)
        .expect("idempotent");
    let pinned = reopened
        .read_turn_pin(&session, TURN)
        .expect("pin")
        .expect("pinned");
    assert_eq!(again.commit_id, pinned);
}

#[test]
fn draft_outcome_crash_windows_close_from_the_classified_material() {
    let fixture = temp_dir("pending-crash-draft");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let driver = JournalDriver::new(&store, &session);
    driver.begin();
    let plan_one = one_binding_plan();
    driver.advance(PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    });
    driver.advance(PendingStage::BindingPlanned {
        plan: plan_one.clone(),
        usage: None,
    });
    driver.advance(PendingStage::Bound {
        plan: plan_one.clone(),
    });
    driver.advance(PendingStage::StepClosing(StepClosingState {
        plan_key: plan_one.plan_key,

        bindings: plan_one.bindings.clone(),
        closure_cursor: 0,
        closures: vec![],
        current_candidate: None,
    }));
    let lock = driver.lock;
    let binding = plan_one.bindings[0].clone();

    // Pin the draft candidate (the crash window is between the pin and the
    // terminal fact).
    let draft = ResultDraft::new(
        binding.invocation_id,
        binding.binding_digest,
        turn_id(),
        binding.address.message,
        0,
        "{\"rows\":[]}".to_string(),
    )
    .expect("draft");
    store
        .advance_pending_turn(
            &session,
            PendingStage::StepClosing(StepClosingState {
                plan_key: plan_one.plan_key,

                bindings: plan_one.bindings.clone(),
                closure_cursor: 0,
                closures: vec![],
                current_candidate: Some(draft.clone()),
            }),
            104,
            &lock,
        )
        .expect("pin candidate");

    // (a) Crash before any draft write: reopen classifies CleanAbsent and
    // the plan closes with unavailable material.
    let reopened = reopen(&fixture);
    assert_eq!(
        reopened.classify_draft(&draft).expect("classify"),
        crate::pending::DraftClassification::CleanAbsent
    );
    let mut facts = reserved_facts(&plan_one);
    facts.invocations.insert(
        binding.invocation_id,
        InvocationRecordFact {
            owner: plan_one.plan_key,
            lifecycle: InvocationLifecycleFact::Terminal {
                outcome: ExecutionOutcome::Succeeded,
                material: ResultMaterial::Unavailable {
                    reason: UnavailableMaterialReason::DraftPublicationFailed,
                    digest: None,
                    byte_length: None,
                },
            },
        },
    );
    let tip = reopened
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    let plan = reconcile_closure(&tip.snapshot, &facts, "plan");
    let closure_material = plan
        .actions
        .iter()
        .find_map(|a| match a {
            ReconciliationAction::AdvanceStepClosing { state } => state.closures.last().cloned(),
            _ => None,
        })
        .expect("closure");
    assert_eq!(
        closure_material.material,
        MaterialAvailability::Unavailable {
            reason: UnavailableMaterialReason::DraftPublicationFailed,
            digest: None,
            byte_length: None,
        }
    );

    // (b) The durable draft window: publish, reopen classifies Durable, and
    // the terminal's available material must match the candidate.
    let durable = reopened
        .publish_draft(&draft, &lock)
        .expect("durable publish");
    assert!(matches!(
        durable,
        crate::pending::DraftPublication::Durable { .. }
    ));
    let reopened = reopen(&fixture);
    assert_eq!(
        reopened.classify_draft(&draft).expect("classify"),
        crate::pending::DraftClassification::Durable
    );
    let mut facts = reserved_facts(&plan_one);
    facts.invocations.insert(
        binding.invocation_id,
        InvocationRecordFact {
            owner: plan_one.plan_key,
            lifecycle: InvocationLifecycleFact::Terminal {
                outcome: ExecutionOutcome::Succeeded,
                material: ResultMaterial::Available {
                    digest: draft.content_digest,
                    byte_length: draft.byte_length,
                },
            },
        },
    );
    let tip = reopened
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    let plan = reconcile_closure(&tip.snapshot, &facts, "plan");
    let closure = plan
        .actions
        .iter()
        .find_map(|a| match a {
            ReconciliationAction::AdvanceStepClosing { state } => state.closures.last().cloned(),
            _ => None,
        })
        .expect("closure");
    assert_eq!(
        closure.material,
        MaterialAvailability::Available {
            digest: draft.content_digest,
            byte_length: draft.byte_length,
        }
    );
    assert_eq!(
        closure.outcome,
        lattice_conversation::ToolResultOutcome::Succeeded
    );
}

#[test]
fn executed_reconciliation_commits_the_recovered_turn_with_reachable_journal() {
    // End-to-end: crash at BindingPlanned with an absent plan; reconcile,
    // execute the plan through the store and the synthetic facts port, and
    // verify the committed Turn, its permanent pin, the reachable typed
    // journal chain, and the transcript.
    let fixture = temp_dir("pending-crash-execute");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let driver = JournalDriver::new(&store, &session);
    driver.begin();
    driver.advance(PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    });
    driver.advance(PendingStage::BindingPlanned {
        plan: one_binding_plan(),
        usage: None,
    });
    drop(driver);

    // Reopen and reconcile.
    let reopened = reopen(&fixture);
    let tip = reopened
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    let mut facts = InvocationFactsSnapshot::empty();
    let plan = reconcile_closure(&tip.snapshot, &facts, "plan");
    assert_eq!(
        plan.outcome,
        TurnOutcome::Interrupted {
            reason: TurnInterruption::ProcessInterruptedDuringToolBinding
        }
    );

    // Execute the plan in order.
    let lock = reopened
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    execute_plan(&reopened, &session, &lock, &plan.actions, &mut facts).expect("execute");
    assert!(reopened.read_active_turn(&session).expect("read").is_none());

    // The recovered Turn is committed with its permanent pin.
    let pinned = reopened
        .read_turn_pin(&session, TURN)
        .expect("pin")
        .expect("pinned");
    let commit = reopened
        .get_turn_commit(&session, &pinned)
        .expect("commit")
        .expect("found");
    assert_eq!(commit.outcome, plan.outcome);
    assert_eq!(commit.origin, ClosureOrigin::Reconciled);

    // The journal chain stays reachable from the pin: TurnCommit ->
    // Committed -> ... -> TurnStarted, and the Committed object carries the
    // typed reconciled outcome.
    let commit_oid = crate::git_graph::helpers::parse_oid(&pinned).expect("oid");
    let parents = reopened
        .kernel()
        .commit_parents(commit_oid)
        .expect("parents");
    assert_eq!(parents.len(), 1);
    let committed_tip = reopened
        .read_pending_snapshot_at(parents[0])
        .expect("committed journal object");
    match committed_tip.snapshot.stage {
        PendingStage::Committed(ref facts) => {
            assert_eq!(facts.candidate.outcome, plan.outcome);
            assert_eq!(facts.candidate.origin, ClosureOrigin::Reconciled);
        }
        other => panic!(
            "expected Committed journal object, got {}",
            other.stage_name()
        ),
    }
    let root = reopened
        .resolve_turn_started(&committed_tip)
        .expect("chain root");
    assert!(root.snapshot.predecessor.is_none());

    // The transcript keeps user, exact bound assistant, and closure framing.
    let transcript = reopened.head_transcript(&session).expect("transcript");
    assert_eq!(transcript.messages.len(), 3);
    assert_eq!(
        crate::test_helpers::text(&transcript.messages[0]),
        Some("please continue")
    );
    assert_eq!(
        transcript.messages[1].role,
        lattice_conversation::MessageRole::Assistant
    );
    assert_eq!(
        transcript.messages[2].role,
        lattice_conversation::MessageRole::Tool
    );
    let head = reopened
        .kernel()
        .ref_target(&format!("refs/lattice/sessions/{session}/heads/main"))
        .expect("head");
    assert_eq!(head.expect("present").to_string(), pinned);

    // A further command has nothing to reconcile.
    assert!(reopened.read_active_turn(&session).expect("read").is_none());
}

#[test]
fn advance_after_a_published_but_undurable_ref_is_an_exact_idempotent_retry() {
    // F7: the CAS moved `active-turn` and the ref barrier then failed. The
    // caller sees `Durability`, but the snapshot IS published. After a
    // strict reopen the byte-identical retry of that exact transition must
    // confirm its own published snapshot and re-attempt the barrier, rather
    // than fail as an illegal `ModelAttemptStarted -> ModelAttemptStarted`
    // edge. Confirmation is exact: a different candidate at the same
    // position still conflicts.
    let fixture = temp_dir("pending-crash-advance-published");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let driver = JournalDriver::new(&store, &session);
    let begun = driver.begin();
    drop(driver);

    let stage = PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    };
    {
        let lock = store
            .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
            .expect("lock");
        lattice_store::fault::fail_ref_fsync(true);
        let err = store.advance_pending_turn(&session, stage.clone(), 100, &lock);
        assert!(
            matches!(
                err,
                Err(SessionError::Store(
                    lattice_store::StoreError::Durability { .. }
                ))
            ),
            "got {err:?}"
        );
    }

    // The ref did move: published, durability unproven.
    let published = store
        .read_active_turn(&session)
        .expect("read")
        .expect("tip");
    assert_ne!(
        published.oid, begun.oid,
        "the CAS completed before the barrier failed"
    );

    let reopened = reopen(&fixture);
    let lock = reopened
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let again = reopened
        .advance_pending_turn(&session, stage, 100, &lock)
        .expect("the exact retry confirms its own published snapshot");
    assert_eq!(again.oid, published.oid, "no second snapshot is published");

    // Same stage, different pinned commit time: a different candidate, so
    // the illegal-transition conflict stands.
    let other = reopened.advance_pending_turn(
        &session,
        PendingStage::ModelAttemptStarted {
            key: attempt_key(0, 0),
        },
        101,
        &lock,
    );
    assert!(
        matches!(other, Err(SessionError::Conflict(_))),
        "a different candidate must not be confirmed: got {other:?}"
    );
    assert_eq!(
        reopened
            .read_active_turn(&session)
            .expect("read")
            .expect("tip")
            .oid,
        published.oid
    );
}

#[test]
fn advance_before_publication_leaves_the_previous_tip_and_writes_no_ref() {
    // F7 counterpart: an object-barrier failure happens *before* the CAS, so
    // nothing is published and the exact retry is a normal first publication.
    let fixture = temp_dir("pending-crash-advance-unpublished");
    let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
    let session = SESSION.to_string();
    let driver = JournalDriver::new(&store, &session);
    let begun = driver.begin();
    drop(driver);

    let stage = PendingStage::ModelAttemptStarted {
        key: attempt_key(0, 0),
    };
    {
        let lock = store
            .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
            .expect("lock");
        lattice_store::fault::fail_object_fsync(true);
        let err = store.advance_pending_turn(&session, stage.clone(), 100, &lock);
        assert!(
            matches!(
                err,
                Err(SessionError::Store(
                    lattice_store::StoreError::Durability { .. }
                ))
            ),
            "got {err:?}"
        );
    }
    assert_eq!(
        store
            .read_active_turn(&session)
            .expect("read")
            .expect("tip")
            .oid,
        begun.oid,
        "an unpublished failure never moves active-turn"
    );

    let reopened = reopen(&fixture);
    let lock = reopened
        .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    let advanced = reopened
        .advance_pending_turn(&session, stage, 100, &lock)
        .expect("retry publishes for the first time");
    assert_eq!(
        advanced.snapshot.predecessor.as_deref(),
        Some(begun.oid.to_string().as_str())
    );
}

// ─── Real process-crash coverage (F2, AC#12) ────────────────────────────
//
// Fault injection above demonstrates call ordering and classification. This
// fixture demonstrates *process-crash* consistency the way ADR 0005 §18
// names it and #143 already gates first-create: a child process is SIGKILLed
// mid-journal, and a strict reopen may only observe the old tip or a
// complete new one. It is not a claim of hardware power-loss consistency.

const CRASH_CHILD_ENV: &str = "LATTICE_PENDING_CRASH_CHILD";
const CRASH_TEST_NAME: &str =
    "git_graph::tests::pending_crash::sigkill_leaves_only_the_old_tip_or_a_complete_new_one";
const CRASH_TURNS: u32 = 200;

fn crash_turn_id(index: u32) -> lancet_protocol::TurnId {
    lancet_protocol::TurnId::parse(&format!("00000000-0000-4000-8000-{index:012x}"))
        .expect("turn id")
}

fn crash_head(store: &GitSessionGraphStore) -> Option<String> {
    store
        .kernel()
        .ref_target(&format!("refs/lattice/sessions/{SESSION}/heads/main"))
        .expect("head")
        .map(|oid| oid.to_string())
}

/// Child mode: drive complete Turn lifecycles in a tight loop so the parent's
/// SIGKILL lands at an arbitrary point inside a snapshot advance or the final
/// ref transaction.
fn sigkill_child_loop(root: &std::path::Path) {
    let store = GitSessionGraphStore::create(root.join("sessions.git")).expect("create store");
    store
        .create_session(SESSION, None, "coder", "main", &super::test_project_key())
        .expect("session");

    let lock = store
        .lock_execution(SESSION, LockMode::Exclusive, Blocking::Block)
        .expect("lock");
    for index in 0..CRASH_TURNS {
        let turn = crash_turn_id(index);
        let mut facts = turn_started_facts(crash_head(&store));
        facts.turn_id = turn;
        store.begin_pending_turn(facts, &lock).expect("begin");
        if index == 0 {
            // Signal readiness only once a Turn is genuinely pending, so
            // even a zero-delay kill lands inside the journal rather than
            // before it and the parent's progress assertion is exact.
            std::fs::write(root.join("ready"), b"ready").expect("ready marker");
        }
        store
            .advance_pending_turn(
                SESSION,
                PendingStage::ModelAttemptStarted {
                    key: lancet_protocol::ModelAttemptKey {
                        turn_id: turn,
                        model_step_ordinal: 0,
                        attempt_ordinal: 0,
                    },
                },
                100,
                &lock,
            )
            .expect("attempt");
        store
            .advance_pending_turn(
                SESSION,
                PendingStage::StepClosed {
                    kind: crate::pending::StepClosedKind::FinishTurn {
                        final_message: assistant_text("done"),
                    },
                },
                101,
                &lock,
            )
            .expect("closed");
        let mut candidate = completed_candidate(None);
        candidate.turn_id = turn;
        store
            .advance_pending_turn(
                SESSION,
                PendingStage::TurnClosing(TurnClosingFacts {
                    turn_id: turn,
                    candidate,
                }),
                102,
                &lock,
            )
            .expect("closing");
        store
            .finalize_turn(SESSION, &turn, &lock)
            .expect("finalize");
    }
}

/// Parent assertions after one kill: the published Turn pins are a contiguous
/// prefix, the branch head is the last of them, any surviving `active-turn`
/// is exactly the next Turn and reconciles to one deterministic closure, and
/// nothing half-published is observable.
fn assert_crash_consistency(root: &std::path::Path) {
    // Strict reopen: session-ref invariants, journal-object validation, and
    // turn-pin identity all run before the handle exists.
    let store = GitSessionGraphStore::open(root.join("sessions.git")).expect("strict reopen");
    let session = SESSION.to_string();

    let mut published = 0u32;
    let mut last_pin: Option<String> = None;
    for index in 0..CRASH_TURNS {
        let turn = crash_turn_id(index).to_string();
        match store.read_turn_pin(&session, &turn).expect("pin") {
            Some(pin) => {
                assert_eq!(published, index, "turn pins must be a contiguous prefix");
                let commit = store
                    .get_turn_commit(&session, &pin)
                    .expect("commit")
                    .expect("a pin always resolves to its conversation commit");
                // Pin/manifest identity itself was already enforced by the
                // strict reopen above.
                assert_eq!(commit.session_id, session);
                published += 1;
                last_pin = Some(pin);
            }
            None => break,
        }
    }

    let active = store.read_active_turn(&session).expect("read active turn");
    if let Some(tip) = &active {
        assert_eq!(
            tip.snapshot.turn_id,
            crash_turn_id(published),
            "the pending Turn is exactly the first unpublished one"
        );
        // A survivor is always closable from durable facts alone.
        let plan = reconcile_closure(&tip.snapshot, &InvocationFactsSnapshot::empty(), "one plan");
        assert_eq!(plan.turn_id, tip.snapshot.turn_id);
    }
    assert!(
        published > 0 || active.is_some(),
        "the child made no observable progress before the kill"
    );
    assert_eq!(
        crash_head(&store),
        last_pin,
        "the branch head is the last fully published Turn: the final transaction \
         never lands partially"
    );
}

#[test]
fn sigkill_leaves_only_the_old_tip_or_a_complete_new_one() {
    if let Ok(dir) = std::env::var(CRASH_CHILD_ENV) {
        sigkill_child_loop(std::path::Path::new(&dir));
        return;
    }

    // Several kill points so the window is not always the same stage.
    for delay_ms in [0u64, 25, 90] {
        let dir = temp_dir("pending-sigkill");
        let exe = std::env::current_exe().expect("test binary");
        let mut child = std::process::Command::new(exe)
            .args([CRASH_TEST_NAME, "--exact", "--nocapture"])
            .env(CRASH_CHILD_ENV, dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn crash child");

        // Wait until the child is inside the Turn loop, then kill it there.
        let ready = dir.path().join("ready");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !ready.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(ready.exists(), "child never reached the Turn loop");
        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        child.kill().expect("SIGKILL the child");
        let status = child.wait().expect("wait for child");
        assert!(
            !status.success(),
            "the child must be killed, not finish {CRASH_TURNS} turns"
        );

        assert_crash_consistency(dir.path());
    }
}
