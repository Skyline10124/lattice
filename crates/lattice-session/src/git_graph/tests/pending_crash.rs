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

use crate::git_graph::tests::{temp_dir, Fixture};
use crate::git_graph::GitSessionGraphStore;
use crate::graph::{SessionGraphStore, TurnCommitStatus};
use crate::pending::contract::{PendingStage, TurnClosingFacts};
use crate::pending::facts::{
    ExecutionOutcome, InvocationFact, InvocationFactsSnapshot, PlanFacts, ResultMaterial,
    UnavailableMaterialReason,
};
use crate::pending::outcome::{ClosureOrigin, TurnInterruption, TurnOutcome};
use crate::pending::reconcile::{ReconciliationAction, reconcile};
use crate::pending::step::{MaterialAvailability, StepClosingState};
use crate::pending::ResultDraft;
use crate::SessionError;

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
        Self { store, session, lock }
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
        self.store
            .advance_pending_turn(self.session, stage, recorded_at, &self.lock)
            .expect("advance")
    }
}

/// Reopen semantics: a fresh handle validates the same repository strict,
/// with no cached refs.
fn reopen(fixture: &Fixture) -> GitSessionGraphStore {
    GitSessionGraphStore::open(fixture.path().join("sessions.git")).expect("strict reopen")
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
                    facts
                        .invocations
                        .entry(binding.invocation_id)
                        .or_insert(InvocationFact::Reserved);
                }
            }
            ReconciliationAction::TerminalizeInvocation {
                invocation,
                outcome,
                material,
            } => {
                facts.invocations.insert(
                    *invocation,
                    InvocationFact::Terminal {
                        outcome: outcome.clone(),
                        material: material.clone(),
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
                let _ = store.advance_pending_turn(
                    session,
                    PendingStage::TurnClosing(TurnClosingFacts {
                        turn_id: tip.snapshot.turn_id,
                        candidate: candidate.clone(),
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
        let store = GitSessionGraphStore::create(fixture.path().join("sessions.git")).expect("store");
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
        let plan = reconcile(&tip.snapshot, &case.facts).expect("one closure");
        assert_eq!(plan.outcome, case.expected_outcome, "stage {}", case.name);
        assert_eq!(plan.origin, ClosureOrigin::Reconciled);
        // Deterministic: the same inputs produce the same plan.
        let again = reconcile(&tip.snapshot, &case.facts).expect("repeat");
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
        facts
            .invocations
            .insert(binding.invocation_id, InvocationFact::Reserved);
    }
    facts
}

fn dispatch_started_facts(plan: &crate::pending::BindingPlan) -> InvocationFactsSnapshot {
    let mut facts = reserved_facts(plan);
    facts
        .invocations
        .insert(plan.bindings[0].invocation_id, InvocationFact::DispatchStarted);
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
        matches!(err, Err(SessionError::Store(lattice_store::StoreError::Durability { .. }))),
        "got {err:?}"
    );
    let tip = store.read_active_turn(&session).expect("read").expect("tip");
    assert_eq!(tip.oid, begun.oid, "previous tip remains published");

    // Reopen after the crash: the plan is the TurnStarted closure.
    let reopened = reopen(&fixture);
    let tip = reopened.read_active_turn(&session).expect("read").expect("tip");
    let plan = reconcile(&tip.snapshot, &InvocationFactsSnapshot::empty()).expect("plan");
    assert!(matches!(
        plan.outcome,
        TurnOutcome::Interrupted {
            reason: TurnInterruption::ProcessInterruptedBeforeModelAttempt
        }
    ));
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
    driver.advance(PendingStage::TurnClosing(turn_closing(completed_candidate(None))));
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
    let tip = reopened.read_active_turn(&session).expect("read").expect("tip");
    let plan = reconcile(&tip.snapshot, &InvocationFactsSnapshot::empty()).expect("plan");
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
        reopened.read_turn_pin(&session, TURN).expect("pin").as_deref(),
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
    driver.advance(PendingStage::TurnClosing(turn_closing(completed_candidate(None))));
    drop(driver);

    {
        let lock = store
            .lock_execution(&session, LockMode::Exclusive, Blocking::Block)
            .expect("lock");
        lattice_store::fault::fail_ref_fsync(true);
        let err = store.finalize_turn(&session, &turn_id(), &lock);
        assert!(
            matches!(err, Err(SessionError::Store(lattice_store::StoreError::Durability { .. }))),
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
    let pinned = reopened.read_turn_pin(&session, TURN).expect("pin").expect("pinned");
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
        InvocationFact::Terminal {
            outcome: ExecutionOutcome::Succeeded,
            material: ResultMaterial::Unavailable {
                reason: UnavailableMaterialReason::DraftPublicationFailed,
                digest: None,
                byte_length: None,
            },
        },
    );
    let tip = reopened.read_active_turn(&session).expect("read").expect("tip");
    let plan = reconcile(&tip.snapshot, &facts).expect("plan");
    let closure_material = plan
        .actions
        .iter()
        .find_map(|a| match a {
            ReconciliationAction::AdvanceStepClosing { state } => {
                state.closures.last().cloned()
            }
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
    assert!(matches!(durable, crate::pending::DraftPublication::Durable { .. }));
    let reopened = reopen(&fixture);
    assert_eq!(
        reopened.classify_draft(&draft).expect("classify"),
        crate::pending::DraftClassification::Durable
    );
    let mut facts = reserved_facts(&plan_one);
    facts.invocations.insert(
        binding.invocation_id,
        InvocationFact::Terminal {
            outcome: ExecutionOutcome::Succeeded,
            material: ResultMaterial::Available {
                digest: draft.content_digest,
                byte_length: draft.byte_length,
            },
        },
    );
    let tip = reopened.read_active_turn(&session).expect("read").expect("tip");
    let plan = reconcile(&tip.snapshot, &facts).expect("plan");
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
    assert_eq!(closure.outcome, lattice_conversation::ToolResultOutcome::Succeeded);
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
    let plan = reconcile(&tip.snapshot, &facts).expect("plan");
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
    let commit = reopened.get_turn_commit(&session, &pinned).expect("commit").expect("found");
    // Interrupted closes as legacy `error` with no logic-bearing terminal
    // string; the typed outcome lives in the journal chain.
    assert_eq!(commit.status, TurnCommitStatus::Error);
    assert_eq!(commit.terminal_reason, None);

    // The journal chain stays reachable from the pin: TurnCommit ->
    // Committed -> ... -> TurnStarted, and the Committed object carries the
    // typed reconciled outcome.
    let commit_oid = crate::git_graph::helpers::parse_oid(&pinned).expect("oid");
    let parents = reopened.kernel().commit_parents(commit_oid).expect("parents");
    assert_eq!(parents.len(), 1);
    let committed_tip = reopened
        .read_pending_snapshot_at(parents[0])
        .expect("committed journal object");
    match committed_tip.snapshot.stage {
        PendingStage::Committed(ref facts) => {
            assert_eq!(facts.candidate.outcome, plan.outcome);
            assert_eq!(facts.candidate.origin, ClosureOrigin::Reconciled);
        }
        other => panic!("expected Committed journal object, got {}", other.stage_name()),
    }
    let root = reopened
        .resolve_turn_started(&committed_tip)
        .expect("chain root");
    assert!(root.snapshot.predecessor.is_none());

    // The transcript keeps the user message and the explicit closure
    // framing; the branch head is the recovered TurnCommit.
    let transcript = reopened.head_transcript(&session).expect("transcript");
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[0].content, "please continue");
    assert_eq!(transcript.messages[1].role, "tool");
    let head = reopened
        .kernel()
        .ref_target(&format!("refs/lattice/sessions/{session}/heads/main"))
        .expect("head");
    assert_eq!(head.expect("present").to_string(), pinned);

    // A further command has nothing to reconcile.
    assert!(reopened.read_active_turn(&session).expect("read").is_none());
}
