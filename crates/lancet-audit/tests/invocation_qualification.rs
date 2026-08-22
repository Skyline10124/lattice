// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Non-production #139-A cross-domain seam and qualification harnesses.
//!
//! This is the only place that sees both the `lancet-audit` invocation ledger
//! and the `lattice-session` reconciliation domain. `lancet-audit`'s
//! production surface owns audit-domain types only, so ADR 0005 §19's graph
//! keeps no `lancet-audit -> lattice-session` edge and `lancet-api` cannot
//! reach LATTICE session types through audit. The projection below maps a
//! pinned ledger view into `InvocationFactsSnapshot`; the reducer's input
//! contract stays a session-domain type.
//!
//! Nothing here connects the production Tool dispatch path, JSON-RPC
//! transport, or the SQLite audit store, and no real Tool effect is executed:
//! every ToolContent and evidence record is synthetic.

use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use lancet_audit::invocation_store::{
    BindingIdentity, InvocationRecord, InvocationState, InvocationStore, PinnedLedgerView,
    PlanReservation, ReservationBinding, TerminalEvidence, TerminalMaterial, TerminalOutcome,
    UnavailableMaterialReason,
};
use lancet_protocol::{
    InvocationId, ModelAttemptKey, ModelMessageAddress, PlanKey, SessionId, ToolCallAddress, TurnId,
};
use lattice_conversation::{
    MessagePart, MessageRole, MessageSemantic, NativeMessage, ProviderOrigin, ToolResultOutcome,
    UnboundToolCall, arguments_digest, semantic_digest,
};
use lattice_session::pending::{
    BindingPlan, ClosedBinding, DraftClassification, DraftPublication, ExecutionOutcome,
    FrozenTurnConfig, IndeterminateReason, InvocationFactsSnapshot, InvocationLifecycleFact,
    InvocationRecordFact, MaterialAvailability, NotExecutedReason, PendingStage, PlanFacts,
    PlannedBinding, ReconciliationAction, ResultDraft, ResultMaterial, StepClosedKind,
    StepClosingState, ToolCatalogEntry, ToolCatalogSnapshot, TurnExecutionLimits, TurnStartedFacts,
    binding_digest, reconcile,
};
use lattice_session::{GitSessionGraphStore, SessionGraphStore};
use lattice_store::{Blocking, LockMode, ProjectStoreKey, REPO_LOCK_FILE, StateLayout};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// The cross-domain projection (audit ledger -> session reconciliation facts)
// ---------------------------------------------------------------------------

/// Project one exact plan plus every queried candidate from a pinned ledger
/// view into the pure reconciliation vocabulary.
///
/// The view is already pinned to one immutable commit, so this neither
/// reopens nor advances it. Every queried candidate that has a record appears
/// in the snapshot, including records owned by other plans: the reducer
/// distinguishes a clean collision from corruption by owner comparison alone.
fn project_facts(
    view: &PinnedLedgerView,
    plan_key: PlanKey,
    candidate_ids: &[InvocationId],
) -> InvocationFactsSnapshot {
    let plan = view.plan(plan_key).map(project_plan_facts);
    let invocations = view
        .query_invocations(candidate_ids)
        .into_iter()
        .map(|(id, record)| (id, project_invocation_record(record)))
        .collect();
    InvocationFactsSnapshot { plan, invocations }
}

fn project_plan_facts(plan: &PlanReservation) -> PlanFacts {
    PlanFacts {
        plan_key: plan.plan_key,
        bindings: plan.bindings.iter().map(project_binding).collect(),
    }
}

fn project_binding(binding: &ReservationBinding) -> PlannedBinding {
    let identity = &binding.identity;
    let address = ToolCallAddress {
        message: ModelMessageAddress {
            session_id: identity.session_id,
            turn_id: identity.turn_id,
            model_step_ordinal: identity.model_step_ordinal,
        },
        part_ordinal: identity.part_ordinal,
    };
    PlannedBinding {
        invocation_id: binding.invocation_id,
        binding_digest: binding_digest(
            &binding.invocation_id,
            &address,
            &identity.method,
            &identity.descriptor_identity,
            &identity.arguments_digest,
        ),
        address,
        method: identity.method.clone(),
        descriptor_identity: identity.descriptor_identity.clone(),
        arguments_digest: identity.arguments_digest,
    }
}

fn project_invocation_record(record: InvocationRecord) -> InvocationRecordFact {
    InvocationRecordFact {
        owner: record.owner,
        lifecycle: match record.state {
            InvocationState::Reserved => InvocationLifecycleFact::Reserved,
            InvocationState::DispatchStarted => InvocationLifecycleFact::DispatchStarted,
            InvocationState::Terminal {
                outcome, material, ..
            } => InvocationLifecycleFact::Terminal {
                outcome: project_outcome(outcome),
                material: project_material(material),
            },
        },
    }
}

fn project_outcome(outcome: TerminalOutcome) -> ExecutionOutcome {
    match outcome {
        TerminalOutcome::Succeeded => ExecutionOutcome::Succeeded,
        TerminalOutcome::Failed => ExecutionOutcome::Failed,
        TerminalOutcome::Rejected => ExecutionOutcome::Rejected,
        TerminalOutcome::Cancelled => ExecutionOutcome::Cancelled,
        TerminalOutcome::NotExecuted { reason } => ExecutionOutcome::NotExecuted {
            reason: match reason {
                lancet_audit::invocation_store::NotExecutedReason::BeforeDispatch => {
                    NotExecutedReason::BeforeDispatch
                }
                lancet_audit::invocation_store::NotExecutedReason::CancelledBeforeDispatch => {
                    NotExecutedReason::CancelledBeforeDispatch
                }
                lancet_audit::invocation_store::NotExecutedReason::PrecedingInvocationIndeterminate => {
                    NotExecutedReason::PrecedingInvocationIndeterminate
                }
            },
        },
        TerminalOutcome::Indeterminate { reason } => ExecutionOutcome::Indeterminate {
            reason: match reason {
                lancet_audit::invocation_store::IndeterminateReason::MayHaveOccurred => {
                    IndeterminateReason::MayHaveOccurred
                }
            },
        },
    }
}

fn project_material(material: TerminalMaterial) -> ResultMaterial {
    match material {
        TerminalMaterial::Available {
            digest,
            byte_length,
        } => ResultMaterial::Available {
            digest,
            byte_length,
        },
        TerminalMaterial::Unavailable {
            reason,
            digest,
            byte_length,
        } => ResultMaterial::Unavailable {
            reason: match reason {
                UnavailableMaterialReason::DraftPublicationFailed => {
                    lattice_session::pending::UnavailableMaterialReason::DraftPublicationFailed
                }
                UnavailableMaterialReason::OutputTooLarge => {
                    lattice_session::pending::UnavailableMaterialReason::OutputTooLarge
                }
                UnavailableMaterialReason::NotExecuted => {
                    lattice_session::pending::UnavailableMaterialReason::NotExecuted
                }
            },
            digest,
            byte_length,
        },
    }
}

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// Harness shape this binary measures. Shape 1 was the retired per-step
/// `sessions-N.git` benchmark; shape 2 first-creates one global `sessions.git`
/// and one `invocations.git` and initializes every session fixture before the
/// timer. A latency baseline is bound to a shape, so the summarizer refuses to
/// compare across one. Bump this whenever the measured shape changes.
const HARNESS_SHAPE_VERSION: u32 = 2;

const STRESS_SESSIONS: u32 = 8;
const STRESS_STEPS: u32 = 20;
const STRESS_INVOCATIONS: u32 = 8;

const QUAL_TEMP_ROOT_ENV: &str = "LATTICE_INVOCATION_QUALIFICATION_TEMP_ROOT";
const QUAL_RUN_ROOT_ENV: &str = "LATTICE_INVOCATION_QUALIFICATION_RUN_ROOT";

/// Fixture directory for qualification and unit tests.
///
/// Ordinary `cargo test` uses [`TempDir`] and deletes the tree on drop.
/// The qualification runner supplies an explicit child of its temp root and
/// retains it until after the measured groups finish.
enum FixtureDir {
    Auto(TempDir),
    Retained(PathBuf),
}

impl FixtureDir {
    fn path(&self) -> &Path {
        match self {
            Self::Auto(tmp) => tmp.path(),
            Self::Retained(path) => path,
        }
    }

    fn retention(&self) -> &'static str {
        match self {
            Self::Auto(_) => "tempdir",
            Self::Retained(_) => "runner_owned",
        }
    }
}

fn validate_qualification_run_root(temp_root: &Path, run_root: &Path) -> Result<PathBuf, String> {
    if !temp_root.is_absolute() {
        return Err("qualification temp root must be an absolute path".into());
    }
    if !run_root.is_absolute() {
        return Err("qualification run root must be an absolute path".into());
    }

    let temp_meta = std::fs::symlink_metadata(temp_root)
        .map_err(|err| format!("qualification temp root is not readable: {err}"))?;
    if temp_meta.file_type().is_symlink() {
        return Err("qualification temp root must not be a symlink".into());
    }
    if !temp_meta.is_dir() {
        return Err("qualification temp root must be a directory".into());
    }

    let run_meta = std::fs::symlink_metadata(run_root)
        .map_err(|err| format!("qualification run root is not readable: {err}"))?;
    if run_meta.file_type().is_symlink() {
        return Err("qualification run root must not be a symlink".into());
    }
    if !run_meta.is_dir() {
        return Err("qualification run root must be a directory".into());
    }

    let temp_canon = temp_root
        .canonicalize()
        .map_err(|err| format!("canonicalize qualification temp root: {err}"))?;
    let run_canon = run_root
        .canonicalize()
        .map_err(|err| format!("canonicalize qualification run root: {err}"))?;
    if !run_canon.starts_with(&temp_canon) {
        return Err("qualification run root must be below the runner temp root".into());
    }
    if run_canon == temp_canon {
        return Err("qualification run root must be a strict child of the temp root".into());
    }

    let mut entries = std::fs::read_dir(&run_canon)
        .map_err(|err| format!("read qualification run root: {err}"))?;
    if entries.next().is_some() {
        return Err("qualification run root must be empty (collision)".into());
    }

    Ok(run_canon)
}

fn fixture_dir_from(
    temp_root: Option<&std::ffi::OsStr>,
    run_root: Option<&std::ffi::OsStr>,
) -> FixtureDir {
    match (temp_root, run_root) {
        (None, None) => FixtureDir::Auto(TempDir::new().expect("create qualification temp dir")),
        (Some(temp), Some(run)) => {
            let path = validate_qualification_run_root(Path::new(temp), Path::new(run))
                .unwrap_or_else(|err| panic!("{err}"));
            FixtureDir::Retained(path)
        }
        _ => panic!("set both {QUAL_TEMP_ROOT_ENV} and {QUAL_RUN_ROOT_ENV}, or neither"),
    }
}

fn fixture_dir() -> FixtureDir {
    fixture_dir_from(
        std::env::var_os(QUAL_TEMP_ROOT_ENV).as_deref(),
        std::env::var_os(QUAL_RUN_ROOT_ENV).as_deref(),
    )
}

fn test_env() -> (FixtureDir, StateLayout, ProjectStoreKey) {
    let tmp = fixture_dir();
    let layout = StateLayout::in_state_root(tmp.path());
    let key = ProjectStoreKey::derive(tmp.path()).unwrap();
    (tmp, layout, key)
}

fn stress_turn_id(session: u32) -> TurnId {
    TurnId::parse(&format!("00000000-0000-4000-8{session:03x}-{session:012x}")).unwrap()
}

fn stress_plan_key(session: u32, step: u32) -> PlanKey {
    PlanKey {
        turn_id: stress_turn_id(session),
        model_step_ordinal: step,
    }
}

fn stress_binding(session: u32, step: u32, index: u32) -> ReservationBinding {
    let combo = (session as u64) << 24 | (step as u64) << 16 | index as u64;
    let id = InvocationId::parse(&format!("00000000-0000-4000-8000-{combo:012x}")).unwrap();
    ReservationBinding {
        invocation_id: id,
        identity: BindingIdentity {
            session_id: SessionId::parse(&format!("00000000{session:04x}40008000{session:012x}"))
                .unwrap(),
            turn_id: stress_turn_id(session),
            model_step_ordinal: step,
            part_ordinal: index,
            method: "test.echo".to_string(),
            descriptor_identity: "stress-descriptor".to_string(),
            arguments_digest: [0u8; 32],
        },
    }
}

fn terminal_evidence() -> TerminalEvidence {
    TerminalEvidence {
        admission: lancet_protocol::AdmissionEvidence {
            version: lancet_protocol::ADMISSION_EVIDENCE_VERSION,
            tool: "test.echo".to_string(),
            approved_capabilities: Vec::new(),
            denied_capabilities: Vec::new(),
            boundary_excluded: Vec::new(),
            max_boundary: lancet_protocol::Trust::In,
        },
        effects: lancet_protocol::ObservedEffectsRecord {
            version: lancet_protocol::OBSERVED_EFFECTS_RECORD_VERSION,
            effects: Vec::new(),
        },
        denied: lancet_protocol::DeniedOperationsRecord {
            version: lancet_protocol::DENIED_OPERATIONS_RECORD_VERSION,
            denied: Vec::new(),
        },
    }
}

/// One real PendingTurn journal plus the matching exact ledger reservation.
fn result_journal(step: u32) -> (TurnStartedFacts, BindingPlan, Vec<ReservationBinding>) {
    let session_number = 0x100 + step;
    let session_id = SessionId::parse(&format!(
        "00000000{session_number:04x}40008000{session_number:012x}"
    ))
    .unwrap();
    let turn_id = stress_turn_id(session_number);
    let plan_key = PlanKey {
        turn_id,
        model_step_ordinal: 0,
    };
    let descriptor_identity = "d".repeat(64);
    let mut parts = Vec::new();
    let mut planned = Vec::new();
    let mut reservations = Vec::new();

    for part_ordinal in 0..STRESS_INVOCATIONS {
        let invocation_id = stress_binding(session_number, step, part_ordinal).invocation_id;
        let arguments = serde_json::json!({ "part": part_ordinal });
        let arguments_digest = arguments_digest(&arguments);
        let address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id,
                turn_id,
                model_step_ordinal: 0,
            },
            part_ordinal,
        };
        planned.push(PlannedBinding {
            invocation_id,
            binding_digest: binding_digest(
                &invocation_id,
                &address,
                "test.echo",
                &descriptor_identity,
                &arguments_digest,
            ),
            address,
            method: "test.echo".to_string(),
            descriptor_identity: descriptor_identity.clone(),
            arguments_digest,
        });
        reservations.push(ReservationBinding {
            invocation_id,
            identity: BindingIdentity {
                session_id,
                turn_id,
                model_step_ordinal: 0,
                part_ordinal,
                method: "test.echo".to_string(),
                descriptor_identity: descriptor_identity.clone(),
                arguments_digest,
            },
        });
        parts.push(MessagePart::semantic_only(MessageSemantic::ToolCall {
            call: UnboundToolCall {
                provider_call_id: Some(format!("call-{part_ordinal}")),
                name: "test.echo".to_string(),
                arguments,
            },
        }));
    }

    let plan = BindingPlan {
        plan_key,
        message: NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::Anthropic),
            parts,
        ),
        bindings: planned,
    };
    let user_message = NativeMessage::new(
        MessageRole::User,
        None,
        vec![MessagePart::semantic_only(MessageSemantic::Text {
            text: "qualification".to_string(),
        })],
    );
    let facts = TurnStartedFacts {
        turn_id,
        session_id,
        parent_conversation_tip: None,
        user_message,
        config: FrozenTurnConfig {
            agent_name: "qualification".to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            system_prompt: None,
            profile_snapshot_json: "{}".to_string(),
            policy_snapshot_json: "{}".to_string(),
            catalog: ToolCatalogSnapshot::new(
                "qualification-build".to_string(),
                vec![ToolCatalogEntry {
                    name: "test.echo".to_string(),
                    descriptor_identity,
                    description: "qualification".to_string(),
                    input_schema: serde_json::json!({"type": "object"}),
                }],
            )
            .unwrap(),
            limits: TurnExecutionLimits {
                timeout_secs: 60,
                max_model_steps: lattice_session::pending::MAX_MODEL_STEPS,
                max_model_attempts: 1,
            },
        },
        accepted_at: "2026-08-17T00:00:00Z".to_string(),
    };
    (facts, plan, reservations)
}

/// Assert that no other repository's write lock is held right now.
///
/// Repository write locks are strict `flock`s registered process-wide, so a
/// non-blocking strict acquire reports `Busy` exactly when someone holds it.
/// Each store write takes and releases its lock inside one call, so probing
/// the *other* repository immediately before entering a write proves the two
/// repository write locks are never held together.
fn assert_repo_write_lock_free(git_dir: &std::path::Path, who: &str) {
    let probe = lattice_store::lock::acquire_flock_strict(
        &git_dir.join(REPO_LOCK_FILE),
        Blocking::NonBlocking,
    );
    match probe {
        Ok(guard) => drop(guard),
        Err(e) => panic!("{who}: a second repository write lock is held: {e:?}"),
    }
}

// ---------------------------------------------------------------------------
// Projection contract
// ---------------------------------------------------------------------------

#[test]
fn pinned_projection_preserves_foreign_candidate_collisions_and_exact_plan_order() {
    let (_tmp, layout, key) = test_env();
    let store = InvocationStore::create(&layout, &key).unwrap();
    let bindings = [stress_binding(0, 0, 0), stress_binding(0, 0, 1)];
    store
        .reserve_batch(stress_plan_key(0, 0), &bindings)
        .unwrap();
    let view = store.pinned_view().unwrap();
    let unknown = InvocationId::parse("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap();

    let collision = project_facts(
        &view,
        stress_plan_key(0, 1),
        &[bindings[0].invocation_id, unknown],
    );
    assert!(collision.plan.is_none());
    assert_eq!(collision.invocations.len(), 1);
    assert_eq!(
        collision.invocations[&bindings[0].invocation_id].owner,
        stress_plan_key(0, 0)
    );

    let exact = project_facts(
        &view,
        stress_plan_key(0, 0),
        &[bindings[1].invocation_id, bindings[0].invocation_id],
    );
    let plan = exact.plan.expect("exact plan");
    assert_eq!(
        plan.bindings
            .iter()
            .map(|binding| binding.invocation_id)
            .collect::<Vec<_>>(),
        vec![bindings[0].invocation_id, bindings[1].invocation_id],
        "projection preserves the reservation's exact order, not the query order"
    );
}

// ---------------------------------------------------------------------------
// Clean UUID collision, end to end over the real stores (L-GH144-01)
// ---------------------------------------------------------------------------

#[test]
fn clean_collision_revises_candidates_end_to_end_over_real_stores() {
    let (tmp, layout, key) = test_env();
    let invocations = InvocationStore::create(&layout, &key).unwrap();
    let sessions_dir = tmp.path().join("sessions.git");
    let sessions = GitSessionGraphStore::create(&sessions_dir).unwrap();

    // Step 0: another plan already owns every candidate ID this Turn planned.
    let (facts, plan, reservations) = result_journal(0);
    let foreign_key = PlanKey {
        turn_id: stress_turn_id(0xfff),
        model_step_ordinal: 3,
    };
    let foreign: Vec<ReservationBinding> = reservations
        .iter()
        .enumerate()
        .map(|(index, binding)| ReservationBinding {
            invocation_id: binding.invocation_id,
            identity: BindingIdentity {
                session_id: SessionId::parse("ffffffff111144448888ffffffffffff").unwrap(),
                turn_id: foreign_key.turn_id,
                model_step_ordinal: foreign_key.model_step_ordinal,
                part_ordinal: index as u32,
                method: "test.echo".to_string(),
                descriptor_identity: "foreign-descriptor".to_string(),
                arguments_digest: [7u8; 32],
            },
        })
        .collect();
    invocations.reserve_batch(foreign_key, &foreign).unwrap();

    // Step 1: the durable BindingPlanned journal for this Turn.
    let turn_session_id = facts.session_id;
    let session_id = turn_session_id.to_string();
    sessions
        .create_session(&session_id, None, "collision", "main", &key)
        .unwrap();
    let lock = sessions
        .lock_execution(&session_id, LockMode::Exclusive, Blocking::Block)
        .unwrap();
    sessions.begin_pending_turn(facts, &lock).unwrap();
    sessions
        .advance_pending_turn(
            &session_id,
            PendingStage::ModelAttemptStarted {
                key: ModelAttemptKey {
                    turn_id: plan.plan_key.turn_id,
                    model_step_ordinal: 0,
                    attempt_ordinal: 0,
                },
            },
            1,
            &lock,
        )
        .unwrap();
    let planned_tip = sessions
        .advance_pending_turn(
            &session_id,
            PendingStage::BindingPlanned {
                plan: plan.clone(),
                usage: None,
            },
            2,
            &lock,
        )
        .unwrap();

    // Step 2: pinned audit facts -> composition-layer projection.
    let candidate_ids: Vec<_> = plan.bindings.iter().map(|b| b.invocation_id).collect();
    let view = invocations.pinned_view().unwrap();
    let projected = project_facts(&view, plan.plan_key, &candidate_ids);
    assert!(projected.plan.is_none(), "this plan reserved nothing");
    assert!(
        projected
            .invocations
            .values()
            .all(|record| record.owner == foreign_key),
        "every candidate is foreign-owned"
    );

    // Step 3: the pure reducer asks for a candidate revision, not a closure.
    let decision = reconcile(&planned_tip.snapshot, &projected).expect("collision decision");
    assert!(
        decision.closure().is_none(),
        "a collision must not produce a Turn closure"
    );
    let request = decision.revision().expect("revision request").clone();
    assert_eq!(request.session_id, turn_session_id);
    assert_eq!(request.turn_id, plan.plan_key.turn_id);
    assert_eq!(request.plan, plan);
    assert_eq!(request.colliding_candidates, candidate_ids);

    // Step 4/5: fresh UUIDs generated in the I/O layer, CAS-published through
    // the real SessionStore. A stale expected tip fails closed first.
    let fresh: Vec<InvocationId> = (0..plan.bindings.len())
        .map(|index| InvocationId::parse(&format!("11111111-2222-4333-8444-{index:012x}")).unwrap())
        .collect();
    let mut next = fresh.clone().into_iter();

    // Post-CAS durability failure: the publication is real but its barrier
    // failed, so it is never reported as success.
    lattice_store::fault::fail_ref_fsync(true);
    assert!(
        sessions
            .execute_candidate_revision(&request, &planned_tip, 3, &lock, || next.next().unwrap())
            .is_err(),
        "a ref durability failure is never reported as success"
    );

    // Recovery must not depend on re-minting the same UUIDs: retry with a
    // deliberately *different* generator and the already-published revision
    // is recognized structurally, its barrier repeated, and returned.
    let mut never_used = (0..plan.bindings.len()).map(|index| {
        InvocationId::parse(&format!("dddddddd-2222-4333-8444-{index:012x}")).unwrap()
    });
    let revised_tip = sessions
        .execute_candidate_revision(&request, &planned_tip, 3, &lock, || {
            never_used.next().unwrap()
        })
        .expect("retry recovers the published revision");
    assert_eq!(
        never_used.count(),
        plan.bindings.len(),
        "recovery generated no candidate at all"
    );

    // Idempotence: repeating the identical call publishes nothing new and
    // returns the same durable revision.
    let mut unused = (0..plan.bindings.len()).map(|index| {
        InvocationId::parse(&format!("99999999-2222-4333-8444-{index:012x}")).unwrap()
    });
    let again = sessions
        .execute_candidate_revision(&request, &planned_tip, 3, &lock, || unused.next().unwrap())
        .expect("idempotent recovery");
    assert_eq!(again.oid, revised_tip.oid);
    assert_eq!(unused.count(), plan.bindings.len());

    // Step 6: reopen the SessionStore and read the complete revised plan.
    drop(revised_tip);
    drop(sessions);
    let reopened = GitSessionGraphStore::open(&sessions_dir).unwrap();
    let tip = reopened
        .read_active_turn(&session_id)
        .unwrap()
        .expect("the Turn is still open");
    let PendingStage::BindingPlanned {
        plan: revised,
        usage,
    } = &tip.snapshot.stage
    else {
        panic!("a candidate revision stays at BindingPlanned");
    };
    assert!(usage.is_none());
    assert_eq!(revised.bindings.len(), plan.bindings.len());
    assert_eq!(revised.message, plan.message, "the message is unchanged");
    assert_eq!(revised.plan_key, plan.plan_key);
    for (index, (before, after)) in plan.bindings.iter().zip(&revised.bindings).enumerate() {
        assert_eq!(after.invocation_id, fresh[index]);
        assert!(
            !candidate_ids.contains(&after.invocation_id),
            "no old candidate may be reused"
        );
        assert_eq!(after.address, before.address);
        assert_eq!(after.method, before.method);
        assert_eq!(after.descriptor_identity, before.descriptor_identity);
        assert_eq!(after.arguments_digest, before.arguments_digest);
        assert_eq!(
            after.binding_digest,
            binding_digest(
                &after.invocation_id,
                &after.address,
                &after.method,
                &after.descriptor_identity,
                &after.arguments_digest,
            ),
            "the binding digest is recomputed over the new candidate"
        );
        assert_ne!(after.binding_digest, before.binding_digest);
    }
    revised
        .validate()
        .expect("the revised plan is self-consistent");

    // Nothing closed the Turn: no StepClosed, TurnClosing, TurnCommit, or
    // durable outcome exists between the collision and the revision.
    assert!(
        reopened.log(&session_id).unwrap().is_empty(),
        "the Turn is not committed"
    );

    // Step 7: project and reconcile again — the revised candidates are free,
    // so the Turn now has a real closure plan.
    let revised_ids: Vec<_> = revised.bindings.iter().map(|b| b.invocation_id).collect();
    let view = invocations.pinned_view().unwrap();
    let after = project_facts(&view, revised.plan_key, &revised_ids);
    assert!(
        after.invocations.is_empty(),
        "the fresh candidates are absent"
    );
    let decision = reconcile(&tip.snapshot, &after).expect("closure after revision");
    let closure = decision.closure().expect("a closure, not another revision");
    assert!(
        closure.actions.iter().any(|action| matches!(
            action,
            ReconciliationAction::ReserveExactPlan { bindings, .. }
                if bindings.iter().map(|b| b.invocation_id).eq(revised_ids.iter().copied())
        )),
        "the closure reserves exactly the revised candidates: {:?}",
        closure.actions
    );

    // CAS mismatch: once the journal moves past `BindingPlanned`, the stale
    // expected tip no longer matches and no successor is a revision of it, so
    // nothing is published and the durable tip is preserved.
    reopened
        .advance_pending_turn(
            &session_id,
            PendingStage::Bound {
                plan: revised.clone(),
            },
            4,
            &lock,
        )
        .unwrap();
    let bound_oid = reopened
        .read_active_turn(&session_id)
        .unwrap()
        .expect("still open")
        .oid;
    let mut rejected = (0..plan.bindings.len()).map(|index| {
        InvocationId::parse(&format!("88888888-2222-4333-8444-{index:012x}")).unwrap()
    });
    assert!(
        reopened
            .execute_candidate_revision(&request, &planned_tip, 5, &lock, || rejected
                .next()
                .unwrap())
            .is_err(),
        "a stale expected tip must not publish a second revision"
    );
    assert_eq!(
        reopened
            .read_active_turn(&session_id)
            .unwrap()
            .expect("still open")
            .oid,
        bound_oid,
        "the rejected revision left the durable tip untouched"
    );
}

// ---------------------------------------------------------------------------
// Qualification harnesses (run with --ignored --nocapture)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StressTimings {
    reserve_batch_ns: Mutex<Vec<u64>>,
    dispatch_started_ns: Mutex<Vec<u64>>,
    terminal_publish_ns: Mutex<Vec<u64>>,
    // Durable-first successful publish attempts only. Uncertain paths are covered by
    // uncertain_draft_revalidation_ns and full_terminal_finalize_ns.
    result_draft_publish_ns: Mutex<Vec<u64>>,
    uncertain_draft_revalidation_ns: Mutex<Vec<u64>>,
    full_terminal_finalize_ns: Mutex<Vec<u64>>,
    /// Per-binding `StepClosing` candidate pin on the SessionStore. Together
    /// with `result_draft_publish_ns` / `uncertain_draft_revalidation_ns` and
    /// `terminal_publish_ns`, this splits `full_terminal_finalize_ns`.
    session_journal_pin_ns: Mutex<Vec<u64>>,
    completed_writes: AtomicU64,
    max_consecutive_wait: AtomicU64,
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn record_write<T>(
    timings: &StressTimings,
    bucket: &Mutex<Vec<u64>>,
    operation: impl FnOnce() -> T,
) -> T {
    let completion_before = timings.completed_writes.load(Ordering::Relaxed);
    let started = Instant::now();
    let result = operation();
    let elapsed = elapsed_ns(started);
    bucket.lock().unwrap().push(elapsed);
    let completion_at = timings.completed_writes.fetch_add(1, Ordering::Relaxed);
    timings.max_consecutive_wait.fetch_max(
        completion_at.saturating_sub(completion_before),
        Ordering::Relaxed,
    );
    result
}

fn recorded(bucket: &Mutex<Vec<u64>>) -> Vec<u64> {
    bucket.lock().unwrap().clone()
}

fn count_created_repos_named(file_name: &str) -> usize {
    lattice_store::fault::created_repo_paths()
        .iter()
        .filter(|path| path.file_name().is_some_and(|name| name == file_name))
        .count()
}

fn assert_complete_loose_object_fanout(git_dir: &std::path::Path) {
    let objects = git_dir.join("objects");
    for prefix in 0u8..=0xff {
        let dir = objects.join(format!("{prefix:02x}"));
        assert!(
            dir.is_dir(),
            "first-create must publish fanout {}",
            dir.display()
        );
    }
}

fn is_retired_per_step_session_repo(name: &str) -> bool {
    name.starts_with("sessions-") && name.ends_with(".git")
}

/// Walk `root` without descending into `*.git` directories. The old harness
/// published `sessions-N.git` at the TempDir root; a layout regression would
/// publish it under `lattice/`.
fn assert_no_per_step_session_repositories(root: &std::path::Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).unwrap_or_else(|err| {
            panic!(
                "reading {} for leftover session repositories: {err}",
                dir.display()
            )
        });
        for entry in entries {
            let entry = entry.expect("directory entry");
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            assert!(
                !is_retired_per_step_session_repo(&name),
                "per-step session repositories are retired: {}",
                path.display()
            );
            let file_type = entry.file_type().unwrap_or_else(|err| {
                panic!(
                    "stat {} for leftover session repositories: {err}",
                    path.display()
                )
            });
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() && !name.ends_with(".git") {
                stack.push(path);
            }
        }
    }
}

struct ResultFinalizeFixture {
    _tmp: FixtureDir,
    layout: StateLayout,
    key: ProjectStoreKey,
    store: InvocationStore,
    session_store: GitSessionGraphStore,
    sessions_dir: std::path::PathBuf,
    invocations_git: std::path::PathBuf,
    journals: Vec<(TurnStartedFacts, BindingPlan, Vec<ReservationBinding>)>,
    session_repository_create_count: usize,
    invocation_repository_create_count: usize,
}

/// One global `sessions.git` and one `invocations.git`, with all 20 session
/// fixtures already created. Repository first-create and fanout publication
/// happen here, not inside the timed finalize loop.
fn prepare_result_finalize_fixture() -> ResultFinalizeFixture {
    lattice_store::fault::reset_repo_create_trace();
    let (tmp, layout, key) = test_env();
    let store = InvocationStore::create(&layout, &key).unwrap();
    let session_store = GitSessionGraphStore::create_layout(&layout).unwrap();
    let journals: Vec<_> = (0..STRESS_STEPS).map(result_journal).collect();
    for (facts, _, _) in &journals {
        session_store
            .create_session(&facts.session_id.to_string(), None, "stress", "main", &key)
            .unwrap();
    }
    let sessions_dir = session_store.dir().to_path_buf();
    let invocations_git = layout.project_store(&key, "invocations.git");
    let session_repository_create_count = count_created_repos_named("sessions.git");
    let invocation_repository_create_count = count_created_repos_named("invocations.git");
    assert_eq!(
        session_repository_create_count,
        1,
        "RESULT_FINALIZE_STRESS must first-create one global sessions.git, got {:?}",
        lattice_store::fault::created_repo_paths()
    );
    assert_eq!(
        invocation_repository_create_count,
        1,
        "RESULT_FINALIZE_STRESS must first-create one invocations.git, got {:?}",
        lattice_store::fault::created_repo_paths()
    );
    assert_complete_loose_object_fanout(&sessions_dir);
    assert_complete_loose_object_fanout(&invocations_git);
    assert_no_per_step_session_repositories(tmp.path());
    ResultFinalizeFixture {
        _tmp: tmp,
        layout,
        key,
        store,
        session_store,
        sessions_dir,
        invocations_git,
        journals,
        session_repository_create_count,
        invocation_repository_create_count,
    }
}

fn run_stress_session(store: &InvocationStore, timings: &StressTimings, session: u32) -> usize {
    let mut completed = 0;
    for step in 0..STRESS_STEPS {
        let pk = stress_plan_key(session, step);
        let bindings: Vec<_> = (0..STRESS_INVOCATIONS)
            .map(|i| stress_binding(session, step, i))
            .collect();
        record_write(timings, &timings.reserve_batch_ns, || {
            store.reserve_batch(pk, &bindings)
        })
        .unwrap();
        completed += 1;
        for b in &bindings {
            record_write(timings, &timings.dispatch_started_ns, || {
                store.dispatch_started(b.invocation_id)
            })
            .unwrap();
            completed += 1;
            record_write(timings, &timings.terminal_publish_ns, || {
                store.publish_terminal(
                    b.invocation_id,
                    TerminalOutcome::Succeeded,
                    TerminalMaterial::Unavailable {
                        reason: UnavailableMaterialReason::DraftPublicationFailed,
                        digest: None,
                        byte_length: None,
                    },
                    terminal_evidence(),
                )
            })
            .unwrap();
            completed += 1;
        }
    }
    completed
}

/// LOCAL_LEDGER_STRESS: 8 sessions × 20 steps × 8 invocations = 1,280
/// invocations, 2,720 ledger commits (ADR 0005 §20). Run with
/// `--ignored --nocapture`.
#[test]
#[ignore]
fn local_ledger_stress() {
    use std::sync::Barrier;

    let (_tmp, layout, key) = test_env();
    let store = Arc::new(InvocationStore::create(&layout, &key).unwrap());
    let timings = Arc::new(StressTimings::default());
    let barrier = Arc::new(Barrier::new(STRESS_SESSIONS as usize));
    store.reset_metrics();
    let started = Instant::now();

    let handles: Vec<_> = (0..STRESS_SESSIONS)
        .map(|session| {
            let store = Arc::clone(&store);
            let timings = Arc::clone(&timings);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let writer_started = Instant::now();
                let completed = run_stress_session(&store, &timings, session);
                (completed, elapsed_ns(writer_started))
            })
        })
        .collect();
    let per_writer: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    let per_writer_completed: Vec<_> = per_writer.iter().map(|(count, _)| *count).collect();
    let per_writer_wall_ns: Vec<_> = per_writer.iter().map(|(_, wall_ns)| *wall_ns).collect();
    let wall_ns = elapsed_ns(started);
    let store_metrics = store.metrics_snapshot();

    drop(store);
    let reopened = InvocationStore::open(&layout, &key).unwrap();
    let view = reopened.pinned_view().unwrap();
    let expected_ids: Vec<_> = (0..STRESS_SESSIONS)
        .flat_map(|session| {
            (0..STRESS_STEPS).flat_map(move |step| {
                (0..STRESS_INVOCATIONS)
                    .map(move |call| stress_binding(session, step, call).invocation_id)
            })
        })
        .collect();
    assert_eq!(
        view.snapshot
            .invocations
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        {
            let mut sorted = expected_ids.clone();
            sorted.sort_unstable();
            sorted
        }
    );
    let queried = view.query_invocations(&expected_ids);
    assert_eq!(queried.len(), expected_ids.len());
    for (id, record) in queried {
        let InvocationState::Terminal { outcome, .. } = &record.state else {
            panic!("{id} not terminal");
        };
        // Every write in this harness is a deterministic success; an
        // Indeterminate terminal would mean the store lost a durable
        // publication, not that the harness produced one.
        assert_eq!(
            *outcome,
            TerminalOutcome::Succeeded,
            "{id} closed as {outcome:?}, expected a deterministic success"
        );
    }

    let transitions = reopened.tail(None).unwrap();
    let expected = expected_ids.len();
    assert_eq!(transitions.len(), expected * 3);
    assert_eq!(
        transitions
            .iter()
            .filter(|transition| matches!(transition.record.state, InvocationState::Reserved))
            .count(),
        expected
    );
    assert_eq!(
        transitions
            .iter()
            .filter(|transition| {
                matches!(transition.record.state, InvocationState::DispatchStarted)
            })
            .count(),
        expected
    );
    assert_eq!(
        transitions
            .iter()
            .filter(|transition| {
                matches!(transition.record.state, InvocationState::Terminal { .. })
            })
            .count(),
        expected
    );
    assert_eq!(
        transitions
            .iter()
            .filter(|transition| matches!(
                &transition.record.state,
                InvocationState::Terminal {
                    outcome: TerminalOutcome::Indeterminate { .. },
                    ..
                }
            ))
            .count(),
        0,
        "no invocation may close indeterminate in a deterministic harness"
    );
    let transition_commits: std::collections::HashSet<_> = transitions
        .iter()
        .map(|transition| transition.commit_oid)
        .collect();
    let expected_commits = (STRESS_SESSIONS * STRESS_STEPS * (1 + 2 * STRESS_INVOCATIONS)) as usize;
    assert_eq!(transition_commits.len(), expected_commits);
    let expected_writes = (STRESS_STEPS * (1 + 2 * STRESS_INVOCATIONS)) as usize;
    assert!(
        per_writer_completed
            .iter()
            .all(|count| *count == expected_writes)
    );
    let fastest_writer_ns = *per_writer_wall_ns.iter().min().unwrap();
    let slowest_writer_ns = *per_writer_wall_ns.iter().max().unwrap();
    let writer_wall_max_to_min_ratio_milli =
        slowest_writer_ns.saturating_mul(1_000) / fastest_writer_ns;
    let report = serde_json::json!({
        "harness": "local-ledger",
        "harness_shape_version": HARNESS_SHAPE_VERSION,
        "invocations": expected,
        "wall_ns": wall_ns,
        "repository": store_metrics,
        "reserve_batch_ns": recorded(&timings.reserve_batch_ns),
        "dispatch_started_ns": recorded(&timings.dispatch_started_ns),
        "terminal_evidence_publish_ns": recorded(&timings.terminal_publish_ns),
        "per_writer_completed": per_writer_completed,
        "per_writer_wall_ns": per_writer_wall_ns,
        "writer_wall_max_to_min_ratio_milli": writer_wall_max_to_min_ratio_milli,
        "unexpected_indeterminate_count": 0,
        "cas_retry_count": 0,
        "cas_retry_mode": "structural_zero_under_exclusive_repository_flock",
        "max_consecutive_wait": timings.max_consecutive_wait.load(Ordering::Relaxed),
        "fixture_retention": _tmp.retention(),
    });
    println!("QUALIFICATION_JSON={report}");
}

/// RESULT_FINALIZE_STRESS drives the real durable order for every binding:
///
/// ```text
/// BindingPlanned -> exact reservation -> Bound
/// -> StepClosing(cursor 0, no candidate)      # the closing loop's baseline
/// per binding i:
///   -> DispatchStarted
///   -> StepClosing(cursor i, exact ResultDraft candidate)
///   -> CleanAbsent | Durable | Uncertain-then-revalidated draft
///   -> typed terminal + evidence
///   -> StepClosing(cursor i+1, closure appended, candidate cleared)
/// -> StepClosed(ContinueModel) -> reopen reconciles to one legal closure
/// ```
///
/// `Bound -> StepClosing` must carry cursor 0, no closures, and no candidate;
/// pinning a candidate and advancing the cursor are separate transitions.
///
/// Every ToolContent and evidence record is synthetic; no Tool executes.
/// First-create of `sessions.git` / `invocations.git` and session fixture
/// initialization are outside the timed finalize phase.

#[test]
fn result_finalize_setup_creates_one_global_session_and_invocation_repository() {
    let fixture = prepare_result_finalize_fixture();
    assert_eq!(fixture.session_repository_create_count, 1);
    assert_eq!(fixture.invocation_repository_create_count, 1);
    assert_eq!(fixture.journals.len(), STRESS_STEPS as usize);
    let session_ids: std::collections::BTreeSet<_> = fixture
        .journals
        .iter()
        .map(|(facts, _, _)| facts.session_id)
        .collect();
    assert_eq!(
        session_ids.len(),
        STRESS_STEPS as usize,
        "each step must own a distinct session in the shared SessionStore"
    );
    let invocation_count: usize = fixture
        .journals
        .iter()
        .map(|(_, _, bindings)| bindings.len())
        .sum();
    assert_eq!(invocation_count, 160);
    let creates_after_setup = lattice_store::fault::created_repo_paths().len();
    GitSessionGraphStore::open(&fixture.sessions_dir).unwrap();
    InvocationStore::open(&fixture.layout, &fixture.key).unwrap();
    assert_eq!(
        lattice_store::fault::created_repo_paths().len(),
        creates_after_setup,
        "strict open must not first-create a repository"
    );
}

#[test]
#[should_panic(expected = "per-step session repositories are retired")]
fn per_step_session_repository_under_lattice_root_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let sneak = tmp.path().join("lattice").join("sessions-0.git");
    std::fs::create_dir_all(&sneak).unwrap();
    assert_no_per_step_session_repositories(tmp.path());
}

#[test]
fn leftover_repository_walk_does_not_follow_symlinked_directories() {
    let fixture = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::create_dir_all(outside.path().join("sessions-0.git")).unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.path().join("escape")).unwrap();
    assert_no_per_step_session_repositories(fixture.path());
}

#[test]
#[should_panic(expected = "per-step session repositories are retired")]
fn leftover_repository_walk_still_rejects_a_symlink_named_like_a_retired_repo() {
    let fixture = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), fixture.path().join("sessions-0.git")).unwrap();
    assert_no_per_step_session_repositories(fixture.path());
}

fn make_empty_run_root(temp_root: &Path, name: &str) -> PathBuf {
    let run_root = temp_root.join("runs").join(name);
    std::fs::create_dir_all(&run_root).unwrap();
    run_root
}

#[test]
fn qualification_run_root_outside_temp_root_is_rejected() {
    let temp = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let err = validate_qualification_run_root(temp.path(), other.path()).unwrap_err();
    assert!(
        err.contains("below the runner temp root"),
        "outside root must fail closed: {err}"
    );
}

#[test]
fn qualification_run_root_equal_to_temp_root_is_rejected() {
    let temp = TempDir::new().unwrap();
    let err = validate_qualification_run_root(temp.path(), temp.path()).unwrap_err();
    assert!(
        err.contains("strict child"),
        "temp root itself is not a run root: {err}"
    );
}

#[test]
fn relative_qualification_run_root_is_rejected() {
    let temp = TempDir::new().unwrap();
    let err = validate_qualification_run_root(temp.path(), Path::new("runs/01")).unwrap_err();
    assert!(
        err.contains("absolute"),
        "relative run root must fail closed: {err}"
    );
}

#[test]
fn reused_qualification_run_root_is_rejected() {
    let temp = TempDir::new().unwrap();
    let run_root = make_empty_run_root(temp.path(), "01");
    std::fs::write(run_root.join("occupied"), b"x").unwrap();
    let err = validate_qualification_run_root(temp.path(), &run_root).unwrap_err();
    assert!(
        err.contains("empty"),
        "reused run root must fail closed: {err}"
    );
}

#[test]
fn symlink_qualification_run_root_is_rejected() {
    let temp = TempDir::new().unwrap();
    let real = make_empty_run_root(temp.path(), "real");
    let link = temp.path().join("runs").join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let err = validate_qualification_run_root(temp.path(), &link).unwrap_err();
    assert!(
        err.contains("symlink"),
        "symlinked run root must fail closed: {err}"
    );
}

#[test]
fn ordinary_fixture_dir_deletes_the_tree_on_drop() {
    let path;
    {
        let dir = fixture_dir_from(None, None);
        assert_eq!(dir.retention(), "tempdir");
        path = dir.path().to_path_buf();
        std::fs::write(path.join("marker"), b"x").unwrap();
        assert!(path.join("marker").exists());
    }
    assert!(
        !path.exists(),
        "ordinary tests must keep automatic TempDir cleanup"
    );
}

#[test]
fn retained_fixture_dir_survives_drop() {
    let temp = TempDir::new().unwrap();
    let run_root = make_empty_run_root(temp.path(), "01");
    let retained = validate_qualification_run_root(temp.path(), &run_root).unwrap();
    {
        let dir = FixtureDir::Retained(retained.clone());
        assert_eq!(dir.retention(), "runner_owned");
        std::fs::write(dir.path().join("marker"), b"x").unwrap();
    }
    assert!(
        retained.join("marker").exists(),
        "runner-owned roots must survive process drop until the runner cleans them"
    );
}

#[test]
fn fixture_dir_uses_a_validated_run_root_from_the_environment() {
    let temp = TempDir::new().unwrap();
    let run_root = make_empty_run_root(temp.path(), "01");
    let dir = fixture_dir_from(Some(temp.path().as_os_str()), Some(run_root.as_os_str()));
    assert_eq!(dir.retention(), "runner_owned");
    assert_eq!(dir.path(), run_root.canonicalize().unwrap());
}

#[test]
#[should_panic(expected = "set both")]
fn fixture_dir_rejects_a_partial_qualification_environment() {
    let temp = TempDir::new().unwrap();
    let _ = fixture_dir_from(Some(temp.path().as_os_str()), None);
}

#[test]
#[ignore]
fn result_finalize_stress() {
    let fixture = prepare_result_finalize_fixture();
    let ResultFinalizeFixture {
        store,
        session_store,
        sessions_dir,
        invocations_git,
        journals,
        session_repository_create_count,
        invocation_repository_create_count,
        key,
        layout,
        _tmp,
    } = fixture;
    let timings = StressTimings::default();
    let mut clean_absent_count = 0;
    let mut uncertain_count = 0;
    let mut durable_count = 0;
    store.reset_metrics();
    let creates_before_timing = lattice_store::fault::created_repo_paths().len();
    let started = Instant::now();

    for (step, (facts, plan, bindings)) in journals.iter().enumerate() {
        let session_id = facts.session_id.to_string();
        let execution_lock = session_store
            .lock_execution(&session_id, LockMode::Exclusive, Blocking::Block)
            .unwrap();
        session_store
            .begin_pending_turn(facts.clone(), &execution_lock)
            .unwrap();
        session_store
            .advance_pending_turn(
                &session_id,
                PendingStage::ModelAttemptStarted {
                    key: ModelAttemptKey {
                        turn_id: plan.plan_key.turn_id,
                        model_step_ordinal: 0,
                        attempt_ordinal: 0,
                    },
                },
                1,
                &execution_lock,
            )
            .unwrap();
        session_store
            .advance_pending_turn(
                &session_id,
                PendingStage::BindingPlanned {
                    plan: plan.clone(),
                    usage: None,
                },
                2,
                &execution_lock,
            )
            .unwrap();

        // The exact ordered reservation must be durable before `Bound`.
        assert_repo_write_lock_free(&sessions_dir, "reserve_batch");
        record_write(&timings, &timings.reserve_batch_ns, || {
            store.reserve_batch(plan.plan_key, bindings)
        })
        .unwrap();
        assert_repo_write_lock_free(&invocations_git, "Bound");
        session_store
            .advance_pending_turn(
                &session_id,
                PendingStage::Bound { plan: plan.clone() },
                3,
                &execution_lock,
            )
            .unwrap();

        // The closing loop starts at cursor 0 with nothing pinned.
        let mut state = StepClosingState {
            plan_key: plan.plan_key,
            bindings: plan.bindings.clone(),
            closure_cursor: 0,
            closures: Vec::new(),
            current_candidate: None,
        };
        let mut recorded_at = 4;
        session_store
            .advance_pending_turn(
                &session_id,
                PendingStage::StepClosing(state.clone()),
                recorded_at,
                &execution_lock,
            )
            .unwrap();

        for (index, binding) in bindings.iter().enumerate() {
            assert_repo_write_lock_free(&sessions_dir, "dispatch_started");
            record_write(&timings, &timings.dispatch_started_ns, || {
                store.dispatch_started(binding.invocation_id)
            })
            .unwrap();

            // The synthetic executor returns here; finalization begins with the
            // durable candidate pin and ends after terminal evidence.
            let finalize_started = Instant::now();

            let planned = &plan.bindings[index];
            let draft = ResultDraft::new(
                binding.invocation_id,
                planned.binding_digest,
                binding.identity.turn_id,
                ModelMessageAddress {
                    session_id: binding.identity.session_id,
                    turn_id: binding.identity.turn_id,
                    model_step_ordinal: binding.identity.model_step_ordinal,
                },
                binding.identity.part_ordinal,
                format!("{{\"invocation_id\":\"{}\"}}", binding.invocation_id),
            )
            .unwrap();

            // ADR 0005 §12: the exact candidate is pinned in a durable
            // StepClosing snapshot *before* the draft write is attempted.
            state.current_candidate = Some(draft.clone());
            recorded_at += 1;
            assert_repo_write_lock_free(&invocations_git, "StepClosing candidate pin");
            let pin_started = Instant::now();
            session_store
                .advance_pending_turn(
                    &session_id,
                    PendingStage::StepClosing(state.clone()),
                    recorded_at,
                    &execution_lock,
                )
                .unwrap();
            timings
                .session_journal_pin_ns
                .lock()
                .unwrap()
                .push(elapsed_ns(pin_started));

            let (material, closure_material, closure_outcome) = match index % 3 {
                0 => {
                    assert_eq!(
                        session_store.classify_draft(&draft).unwrap(),
                        DraftClassification::CleanAbsent
                    );
                    clean_absent_count += 1;
                    (
                        TerminalMaterial::Unavailable {
                            reason: UnavailableMaterialReason::DraftPublicationFailed,
                            digest: Some(draft.content_digest),
                            byte_length: Some(draft.byte_length),
                        },
                        MaterialAvailability::Unavailable {
                            reason:
                                lattice_session::pending::UnavailableMaterialReason::DraftPublicationFailed,
                            digest: Some(draft.content_digest),
                            byte_length: Some(draft.byte_length),
                        },
                        ToolResultOutcome::Succeeded,
                    )
                }
                1 => {
                    lattice_store::fault::fail_object_fsync(true);
                    assert!(matches!(
                        session_store
                            .publish_draft(&draft, &execution_lock)
                            .unwrap(),
                        DraftPublication::Uncertain { .. }
                    ));
                    // Reopen-equivalent revalidation: a fresh strict handle
                    // with no cached refs classifies the exact candidate.
                    let revalidation_started = Instant::now();
                    let reopened = GitSessionGraphStore::open(&sessions_dir).unwrap();
                    assert_eq!(
                        reopened.classify_draft(&draft).unwrap(),
                        DraftClassification::Durable
                    );
                    timings
                        .uncertain_draft_revalidation_ns
                        .lock()
                        .unwrap()
                        .push(elapsed_ns(revalidation_started));
                    let DraftPublication::Durable { draft_oid } = session_store
                        .publish_draft(&draft, &execution_lock)
                        .unwrap()
                    else {
                        panic!("exact draft retry must be durable");
                    };
                    assert!(!draft_oid.is_empty());
                    uncertain_count += 1;
                    (
                        TerminalMaterial::Available {
                            digest: draft.content_digest,
                            byte_length: draft.byte_length,
                        },
                        MaterialAvailability::Available {
                            digest: draft.content_digest,
                            byte_length: draft.byte_length,
                        },
                        ToolResultOutcome::Succeeded,
                    )
                }
                _ => {
                    let draft_started = Instant::now();
                    assert!(matches!(
                        session_store
                            .publish_draft(&draft, &execution_lock)
                            .unwrap(),
                        DraftPublication::Durable { .. }
                    ));
                    timings
                        .result_draft_publish_ns
                        .lock()
                        .unwrap()
                        .push(elapsed_ns(draft_started));
                    durable_count += 1;
                    (
                        TerminalMaterial::Available {
                            digest: draft.content_digest,
                            byte_length: draft.byte_length,
                        },
                        MaterialAvailability::Available {
                            digest: draft.content_digest,
                            byte_length: draft.byte_length,
                        },
                        ToolResultOutcome::Succeeded,
                    )
                }
            };

            // The terminal fact may only appear while the PendingTurn is in a
            // StepClosing snapshot that pins this exact candidate.
            let pinned = session_store
                .read_active_turn(&session_id)
                .unwrap()
                .expect("active turn");
            let PendingStage::StepClosing(pinned_state) = &pinned.snapshot.stage else {
                panic!("a terminal requires a durable StepClosing snapshot");
            };
            assert_eq!(pinned_state.closure_cursor as usize, index);
            assert_eq!(
                pinned_state
                    .current_candidate
                    .as_ref()
                    .map(|c| c.invocation_id),
                Some(binding.invocation_id)
            );

            assert_repo_write_lock_free(&sessions_dir, "publish_terminal");
            record_write(&timings, &timings.terminal_publish_ns, || {
                store.publish_terminal(
                    binding.invocation_id,
                    TerminalOutcome::Succeeded,
                    material,
                    terminal_evidence(),
                )
            })
            .unwrap();
            timings
                .full_terminal_finalize_ns
                .lock()
                .unwrap()
                .push(elapsed_ns(finalize_started));

            // Mid-loop reduce over the *projected* invocation facts. Unlike
            // the terminal `StepClosed` check below, `StepClosing` validates
            // the facts snapshot (exact reservation, ordering, and the pinned
            // candidate's digest), so this is where a bad projection or a
            // bad durable order would be caught. Run it on every binding of
            // the first step (covering all three draft classifications) and
            // on the last binding of every step; a pinned view replays the
            // whole ledger, so this stays O(steps), not O(invocations).
            if step == 0 || index + 1 == bindings.len() {
                let tip = session_store
                    .read_active_turn(&session_id)
                    .unwrap()
                    .expect("active turn");
                let facts = project_facts(
                    &store.pinned_view().unwrap(),
                    plan.plan_key,
                    &plan
                        .bindings
                        .iter()
                        .map(|b| b.invocation_id)
                        .collect::<Vec<_>>(),
                );
                let mid = reconcile(&tip.snapshot, &facts)
                    .expect("StepClosing reduces over the projected facts");
                let closure = mid.closure().expect("a closure, not a revision");
                let cursors: Vec<u32> = closure
                    .actions
                    .iter()
                    .filter_map(|action| match action {
                        ReconciliationAction::AdvanceStepClosing { state } => {
                            Some(state.closure_cursor)
                        }
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    cursors
                        .iter()
                        .copied()
                        .find(|cursor| *cursor > index as u32),
                    Some(index as u32 + 1),
                    "reopen resumes at the exact lowest unclosed ordinal, never skipping"
                );
                assert_eq!(
                    cursors.last().copied(),
                    Some(bindings.len() as u32),
                    "the closure closes every remaining binding"
                );
            }

            // Ordered ToolResult closure: the cursor advances by exactly one.
            state.current_candidate = None;
            state.closure_cursor = index as u32 + 1;
            state.closures.push(ClosedBinding {
                binding: planned.clone(),
                outcome: closure_outcome,
                material: closure_material,
            });
            recorded_at += 1;
            assert_repo_write_lock_free(&invocations_git, "StepClosing cursor advance");
            session_store
                .advance_pending_turn(
                    &session_id,
                    PendingStage::StepClosing(state.clone()),
                    recorded_at,
                    &execution_lock,
                )
                .unwrap();
        }

        // Every binding closed: publish the legal StepClosed.
        assert!(state.all_closed());
        recorded_at += 1;
        session_store
            .advance_pending_turn(
                &session_id,
                PendingStage::StepClosed {
                    kind: StepClosedKind::ContinueModel {
                        model_step_ordinal: plan.plan_key.model_step_ordinal,
                        ordered_tool_results: state.closures.clone(),
                        conversation_digest: semantic_digest(&plan.message),
                    },
                },
                recorded_at,
                &execution_lock,
            )
            .unwrap();

        // Reopen the SessionStore and prove the durable state closes legally:
        // the reducer must produce one closure, never typed corruption.
        drop(execution_lock);
        let reopened = GitSessionGraphStore::open(&sessions_dir).unwrap();
        let tip = reopened
            .read_active_turn(&session_id)
            .unwrap()
            .expect("active turn");
        let candidate_ids: Vec<_> = plan.bindings.iter().map(|b| b.invocation_id).collect();
        let view = store.pinned_view().unwrap();
        let projected = project_facts(&view, plan.plan_key, &candidate_ids);
        let decision = reconcile(&tip.snapshot, &projected).expect("one legal closure");
        let closure = decision.closure().expect("a closure, not a revision");
        assert_eq!(
            closure.origin,
            lattice_session::pending::ClosureOrigin::Reconciled
        );
        assert!(matches!(
            closure.outcome,
            lattice_session::pending::TurnOutcome::Interrupted { .. }
        ));
    }

    let wall_ns = elapsed_ns(started);
    let timed_finalize_repository_create_count =
        lattice_store::fault::created_repo_paths().len() - creates_before_timing;
    assert_eq!(
        timed_finalize_repository_create_count,
        0,
        "timed finalize must not first-create a repository: {:?}",
        lattice_store::fault::created_repo_paths()
    );
    let store_metrics = store.metrics_snapshot();

    drop(store);
    let reopened = InvocationStore::open(&layout, &key).unwrap();
    let view = reopened.pinned_view().unwrap();
    let expected = (STRESS_STEPS * STRESS_INVOCATIONS) as usize;
    assert_eq!(view.snapshot.invocations.len(), expected);
    assert!(clean_absent_count > 0);
    assert!(uncertain_count > 0);
    assert!(durable_count > 0);
    assert_eq!(
        clean_absent_count + uncertain_count + durable_count,
        expected
    );
    assert_eq!(
        timings.result_draft_publish_ns.lock().unwrap().len(),
        durable_count,
        "only durable-first publishes enter clean publish latency samples"
    );
    assert_eq!(
        timings
            .uncertain_draft_revalidation_ns
            .lock()
            .unwrap()
            .len(),
        uncertain_count
    );
    assert_eq!(
        timings.session_journal_pin_ns.lock().unwrap().len(),
        expected,
        "every binding pins a StepClosing candidate before draft/terminal"
    );
    assert_eq!(
        reopened.tail(None).unwrap().len(),
        expected * 3,
        "each invocation must have Reserved, DispatchStarted, and Terminal transitions"
    );
    let unavailable = view
        .snapshot
        .invocations
        .values()
        .filter(|record| {
            matches!(
                record.state,
                InvocationState::Terminal {
                    material: TerminalMaterial::Unavailable { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(unavailable, clean_absent_count);

    // Every invocation in this harness executes deterministically: an
    // `Indeterminate` terminal would mean a durable publication was lost,
    // and any other outcome would mean the harness fabricated one.
    for (id, record) in &view.snapshot.invocations {
        let InvocationState::Terminal { outcome, .. } = &record.state else {
            panic!("{id} is not terminal");
        };
        assert_eq!(
            *outcome,
            TerminalOutcome::Succeeded,
            "{id} closed as {outcome:?}, expected a deterministic success"
        );
    }
    let report = serde_json::json!({
        "harness": "result-finalize",
        "harness_shape_version": HARNESS_SHAPE_VERSION,
        "invocations": expected,
        "clean_absent_count": clean_absent_count,
        "uncertain_count": uncertain_count,
        "durable_count": durable_count,
        "unexpected_indeterminate_count": 0,
        "wall_ns": wall_ns,
        "repository": store_metrics,
        "reserve_batch_ns": recorded(&timings.reserve_batch_ns),
        "dispatch_started_ns": recorded(&timings.dispatch_started_ns),
        "result_draft_publish_ns": recorded(&timings.result_draft_publish_ns),
        "uncertain_draft_revalidation_ns": recorded(&timings.uncertain_draft_revalidation_ns),
        "session_journal_pin_ns": recorded(&timings.session_journal_pin_ns),
        "terminal_evidence_publish_ns": recorded(&timings.terminal_publish_ns),
        "full_terminal_finalize_ns": recorded(&timings.full_terminal_finalize_ns),
        "session_repository_create_count": session_repository_create_count,
        "invocation_repository_create_count": invocation_repository_create_count,
        "timed_finalize_repository_create_count": timed_finalize_repository_create_count,
        "cas_retry_count": 0,
        "cas_retry_mode": "structural_zero_under_exclusive_repository_flock",
        "fixture_retention": _tmp.retention(),
    });
    println!("QUALIFICATION_JSON={report}");
}

#[test]
#[ignore = "benchmark"]
fn cold_reopen_benchmark() {
    let t_start = Instant::now();
    let tmp = fixture_dir();
    let layout = StateLayout::in_state_root(tmp.path());
    let key = ProjectStoreKey::derive(tmp.path()).unwrap();
    InvocationStore::create(&layout, &key).unwrap();

    // The ledger is populated through a **fresh handle per write**. That is
    // the restart shape of the real workload, and it is the shape under which
    // anchor cadence must still advance: a store that counted commits only
    // within one handle's lifetime would never anchor here, and every cold
    // read below would silently degrade to a full genesis replay.
    let n = 1280;
    let mut candidate_ids = Vec::with_capacity(n as usize);
    for i in 0..n {
        let id = InvocationId::parse(&format!("00000000-0000-4000-8000-{i:012x}")).unwrap();
        candidate_ids.push(id);
        let plan_key = PlanKey {
            turn_id: TurnId::parse(&format!("11111111-1111-4111-8111-{i:012x}")).unwrap(),
            model_step_ordinal: 0,
        };
        let b = ReservationBinding {
            invocation_id: id,
            identity: BindingIdentity {
                session_id: SessionId::parse("aaaaaaaa111144448888cccccccccccc").unwrap(),
                turn_id: plan_key.turn_id,
                model_step_ordinal: 0,
                part_ordinal: 0,
                method: "test.echo".to_string(),
                descriptor_identity: "test-descriptor".to_string(),
                arguments_digest: [0u8; 32],
            },
        };
        InvocationStore::open(&layout, &key)
            .unwrap()
            .reserve_batch(plan_key, &[b])
            .unwrap();
        InvocationStore::open(&layout, &key)
            .unwrap()
            .dispatch_started(id)
            .unwrap();
    }
    let cp_path = tmp
        .path()
        .join("lattice/projects")
        .join(key.to_string())
        .join("invocations.git/derived-checkpoint.json");
    assert!(cp_path.exists());

    // Anchoring survived the restarts: the published checkpoint carries real
    // state rather than the genesis snapshot, and the tip is at most one
    // interval past it.
    let checkpoint_plans = {
        let bytes = std::fs::read(&cp_path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["snapshot"]["plans"].as_array().unwrap().len()
    };
    assert!(
        checkpoint_plans > 0,
        "restart-shaped writes must still advance the anchor past genesis"
    );

    // 1. Measure cold open WITH checkpoint
    let mut with_cp_ns = Vec::new();
    for _ in 0..20 {
        let t0 = Instant::now();
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (_, snap) = fresh.snapshot().unwrap();
        assert_eq!(snap.invocations.len(), n as usize);
        with_cp_ns.push(t0.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    }

    // 2. Measure cold open WITHOUT checkpoint (genesis replay)
    let cp_backup = std::fs::read(&cp_path).unwrap();
    let mut without_cp_ns = Vec::new();
    for _ in 0..20 {
        std::fs::remove_file(&cp_path).unwrap();
        let t0 = Instant::now();
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (_, snap) = fresh.snapshot().unwrap();
        assert_eq!(snap.invocations.len(), n as usize);
        without_cp_ns.push(t0.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    }
    std::fs::write(&cp_path, cp_backup).unwrap();

    // 3. Measure repeated project_facts projection from pinned view
    let mut project_facts_ns = Vec::new();
    let fresh = InvocationStore::open(&layout, &key).unwrap();
    let view = fresh.pinned_view().unwrap();
    let query_plan_key = PlanKey {
        turn_id: TurnId::parse("11111111-1111-4111-8111-000000000000").unwrap(),
        model_step_ordinal: 0,
    };
    for _ in 0..20 {
        let t0 = Instant::now();
        let facts = project_facts(&view, query_plan_key, &candidate_ids[0..8]);
        assert_eq!(facts.invocations.len(), 8);
        project_facts_ns.push(t0.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64);
    }

    let wall_ns = t_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
    let store_metrics = fresh.metrics_snapshot();
    let report = serde_json::json!({
        "harness": "cold-reopen",
        "harness_shape_version": HARNESS_SHAPE_VERSION,
        "writer_handle_lifetime": "one_open_per_write",
        "checkpoint_plans_after_restart_writes": checkpoint_plans,
        "invocations": n,
        "commits": n * 2 + 1,
        "wall_ns": wall_ns,
        "repository": store_metrics,
        "cold_read_with_checkpoint_ns": with_cp_ns,
        "cold_read_without_checkpoint_ns": without_cp_ns,
        "repeated_project_facts_ns": project_facts_ns,
        "cas_retry_count": 0,
        "cas_retry_mode": "structural_zero_under_exclusive_repository_flock",
        "fixture_retention": tmp.retention(),
    });
    println!("QUALIFICATION_JSON={report}");
}
