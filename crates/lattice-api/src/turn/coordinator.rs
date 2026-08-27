// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Private `TurnCoordinator` dependency bundle (ADR 0005 §10, GitHub #139-B).
//!
//! The coordinator owns the production cross-store Turn saga. It freezes and
//! journals accepted inputs, drives one provider attempt at a time, reserves exact
//! invocation bindings before dispatch, and reconciles interrupted Turns without
//! repeating provider or Tool execution.
//!
//! Locking contract carried by every adapter (ADR 0005 §5, plan §2.2): the
//! coordinator holds the full-Turn `SessionExecutionLock`, but no repository
//! write lock is held across an await or a call into another repository.
//! Every blocking Git/filesystem operation invoked from async production
//! code runs through [`tokio::task::spawn_blocking`].

use std::sync::Arc;

use lancet_api::WorkspaceHistory;
use lancet_audit::invocation_store::InvocationStore;
use lancet_protocol::{InvocationId, PlanKey, TurnId};
use lattice_session::pending::InvocationFactsSnapshot;
use lattice_session::{GitSessionGraphStore, PendingTurnTip, SessionError};
use thiserror::Error;

use super::finalize::{
    AttemptOutcome, FinalizeBudget, FinalizeClock, FinalizeFailure, TokioFinalizeClock,
    finalize_under_budget,
};
use super::invocation_facts;

/// Errors surfaced by the coordinator's private store adapters.
#[derive(Debug, Error)]
pub(crate) enum CoordinatorIoError {
    #[error("session store error: {0}")]
    Session(#[from] SessionError),
    #[error("invocation store error: {0}")]
    InvocationStore(#[from] lancet_audit::invocation_store::InvocationStoreError),
    #[error("blocking adapter task failed: {0}")]
    Blocking(#[from] tokio::task::JoinError),
    #[error("terminal evidence durability failed after {attempts} attempt(s)")]
    EvidenceDurabilityFailed { attempts: u32 },
    #[error("project-store binding mismatch: {0}")]
    ProjectBinding(String),
    #[error("workspace history error: {0}")]
    WorkspaceHistory(String),
}

/// Injectable canonical identity source (ADR 0005 §1).
///
/// Production uses UUID v4 randomness; deterministic sources exist only in
/// tests. The coordinator never derives identity from content, order, or
/// provider metadata.
pub(crate) trait IdSource: Send + Sync {
    /// Generate one caller-allocated Tool invocation identity.
    fn new_invocation_id(&self) -> InvocationId;
    /// Generate one accepted user Turn identity.
    fn new_turn_id(&self) -> TurnId;
}

/// Production identity source: UUID v4 through the protocol's strict parser.
pub(crate) struct RandomIdSource;

impl IdSource for RandomIdSource {
    fn new_invocation_id(&self) -> InvocationId {
        InvocationId::parse(&uuid::Uuid::new_v4().to_string())
            .expect("uuid v4 display is canonical hyphenated lowercase")
    }

    fn new_turn_id(&self) -> TurnId {
        TurnId::parse(&uuid::Uuid::new_v4().to_string())
            .expect("uuid v4 display is canonical hyphenated lowercase")
    }
}

/// Blocking session/invocation store adapters (plan §2.2).
///
/// Both handles are shared behind `Arc` because `spawn_blocking` requires
/// `'static` closures and the coordinator must not clone repository handles
/// per call: a store owns its kernel write-lock state and its strictly-opened
/// identity.
pub(crate) struct CoordinatorStoreIo {
    session: Arc<GitSessionGraphStore>,
    invocations: Arc<InvocationStore>,
    project_store_key: lattice_session::ProjectStoreKey,
}

impl CoordinatorStoreIo {
    pub(crate) fn new(
        session: Arc<GitSessionGraphStore>,
        invocations: Arc<InvocationStore>,
        project_store_key: lattice_session::ProjectStoreKey,
    ) -> Result<Self, CoordinatorIoError> {
        if invocations.project_store_key() != project_store_key {
            return Err(CoordinatorIoError::ProjectBinding(format!(
                "InvocationStore key {} does not match Session key {project_store_key}",
                invocations.project_store_key()
            )));
        }
        Ok(Self {
            session,
            invocations,
            project_store_key,
        })
    }

    /// Read the current PendingTurn journal tip for one Session.
    ///
    /// `Ok(None)` when no Turn is pending. Runs off the async workers.
    pub(crate) async fn read_active_turn(
        &self,
        session_id: String,
    ) -> Result<Option<PendingTurnTip>, CoordinatorIoError> {
        let store = Arc::clone(&self.session);
        let tip = tokio::task::spawn_blocking(move || store.read_active_turn(&session_id))
            .await
            .map_err(CoordinatorIoError::Blocking)?
            .map_err(CoordinatorIoError::Session)?;
        if let Some(tip) = &tip {
            let pending_key = pending_project_store_key(tip)?;
            if pending_key != self.project_store_key {
                return Err(CoordinatorIoError::ProjectBinding(format!(
                    "PendingTurn key {pending_key} does not match InvocationStore key {}",
                    self.project_store_key
                )));
            }
        }
        Ok(tip)
    }

    #[cfg(feature = "test-crash-barriers")]
    pub(crate) async fn crash_barrier_snapshot(
        &self,
        session_id: String,
    ) -> Result<
        (
            lattice_session::PendingTurnTip,
            lancet_audit::invocation_store::PinnedLedgerView,
            Option<lattice_session::pending::DraftClassification>,
        ),
        CoordinatorIoError,
    > {
        let session = Arc::clone(&self.session);
        let invocations = Arc::clone(&self.invocations);
        tokio::task::spawn_blocking(move || {
            let tip = session.read_active_turn(&session_id)?.ok_or_else(|| {
                lattice_session::SessionError::JournalCorruption {
                    detail: "crash barrier observed no active Turn".into(),
                }
            })?;
            let draft_classification = match &tip.snapshot.stage {
                lattice_session::pending::PendingStage::StepClosing(state) => state
                    .current_candidate
                    .as_ref()
                    .map(|candidate| session.classify_draft(candidate))
                    .transpose()?,
                _ => None,
            };
            let ledger = invocations
                .pinned_view()
                .map_err(CoordinatorIoError::InvocationStore)?;
            Ok((tip, ledger, draft_classification))
        })
        .await
        .map_err(CoordinatorIoError::Blocking)?
    }
    pub(crate) async fn begin_turn(
        &self,
        facts: lattice_session::pending::TurnStartedFacts,
        lock: Arc<lattice_session::FlockGuard>,
    ) -> Result<PendingTurnTip, CoordinatorIoError> {
        if facts.project_store_key != self.project_store_key {
            return Err(CoordinatorIoError::ProjectBinding(format!(
                "TurnStarted key {} does not match InvocationStore key {}",
                facts.project_store_key, self.project_store_key
            )));
        }
        let store = Arc::clone(&self.session);
        tokio::task::spawn_blocking(move || store.begin_pending_turn(facts, &lock))
            .await
            .map_err(CoordinatorIoError::Blocking)?
            .map_err(CoordinatorIoError::Session)
    }

    pub(crate) async fn advance_turn(
        &self,
        session_id: String,
        stage: lattice_session::pending::PendingStage,
        recorded_at: i64,
        lock: Arc<lattice_session::FlockGuard>,
    ) -> Result<PendingTurnTip, CoordinatorIoError> {
        let store = Arc::clone(&self.session);
        tokio::task::spawn_blocking(move || {
            store.advance_pending_turn(&session_id, stage, recorded_at, &lock)
        })
        .await
        .map_err(CoordinatorIoError::Blocking)?
        .map_err(CoordinatorIoError::Session)
    }

    pub(crate) async fn reserve_plan(
        &self,
        plan: lattice_session::pending::BindingPlan,
    ) -> Result<(), CoordinatorIoError> {
        let store = Arc::clone(&self.invocations);
        let bindings = plan
            .bindings
            .iter()
            .map(
                |binding| lancet_audit::invocation_store::ReservationBinding {
                    invocation_id: binding.invocation_id,
                    identity: lancet_audit::invocation_store::BindingIdentity {
                        session_id: binding.address.message.session_id,
                        turn_id: binding.address.message.turn_id,
                        model_step_ordinal: binding.address.message.model_step_ordinal,
                        part_ordinal: binding.address.part_ordinal,
                        method: binding.method.clone(),
                        descriptor_identity: binding.descriptor_identity.clone(),
                        arguments_digest: binding.arguments_digest,
                    },
                },
            )
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || store.reserve_batch(plan.plan_key, &bindings))
            .await
            .map_err(CoordinatorIoError::Blocking)?
            .map_err(CoordinatorIoError::InvocationStore)
    }

    pub(crate) async fn finalize_turn(
        &self,
        session_id: String,
        turn_id: TurnId,
        lock: Arc<lattice_session::FlockGuard>,
    ) -> Result<lattice_session::TurnCommit, CoordinatorIoError> {
        let store = Arc::clone(&self.session);
        tokio::task::spawn_blocking(move || store.finalize_turn(&session_id, &turn_id, &lock))
            .await
            .map_err(CoordinatorIoError::Blocking)?
            .map_err(CoordinatorIoError::Session)
    }

    pub(crate) async fn publish_draft(
        &self,
        candidate: lattice_session::pending::ResultDraft,
        lock: Arc<lattice_session::FlockGuard>,
    ) -> Result<lattice_session::pending::DraftPublication, CoordinatorIoError> {
        let store = Arc::clone(&self.session);
        tokio::task::spawn_blocking(move || store.publish_draft(&candidate, &lock))
            .await
            .map_err(CoordinatorIoError::Blocking)?
            .map_err(CoordinatorIoError::Session)
    }

    pub(crate) async fn classify_draft(
        &self,
        candidate: lattice_session::pending::ResultDraft,
    ) -> Result<lattice_session::pending::DraftClassification, CoordinatorIoError> {
        let store = Arc::clone(&self.session);
        tokio::task::spawn_blocking(move || store.classify_draft(&candidate))
            .await
            .map_err(CoordinatorIoError::Blocking)?
            .map_err(CoordinatorIoError::Session)
    }

    pub(crate) fn session_store(&self) -> Arc<GitSessionGraphStore> {
        Arc::clone(&self.session)
    }

    /// Pin one invocation ledger view and project it into the pure
    /// reconciliation vocabulary.
    ///
    /// The view is pinned inside the blocking adapter so the projection and
    /// the reducer always see one immutable ledger commit, never a moving
    /// tip.
    pub(crate) async fn pin_invocation_facts(
        &self,
        plan_key: PlanKey,
        candidate_ids: Vec<InvocationId>,
    ) -> Result<InvocationFactsSnapshot, CoordinatorIoError> {
        let store = Arc::clone(&self.invocations);
        let pinned = tokio::task::spawn_blocking(move || {
            let view = store.pinned_view()?;
            Ok(invocation_facts::project_facts(
                &view,
                plan_key,
                &candidate_ids,
            ))
        })
        .await
        .map_err(CoordinatorIoError::Blocking)?;
        pinned.map_err(CoordinatorIoError::InvocationStore)
    }
}

fn pending_project_store_key(
    tip: &PendingTurnTip,
) -> Result<lattice_session::ProjectStoreKey, CoordinatorIoError> {
    match &tip.snapshot.stage {
        lattice_session::pending::PendingStage::TurnStarted(facts) => Ok(facts.project_store_key),
        _ => tip
            .snapshot
            .root
            .as_ref()
            .map(|facts| facts.project_store_key)
            .ok_or_else(|| {
                CoordinatorIoError::ProjectBinding(format!(
                    "pending Turn {} has no persisted TurnStarted project binding",
                    tip.snapshot.turn_id
                ))
            }),
    }
}

impl CoordinatorStoreIo {
    /// Publish one frozen terminal candidate under the fixed
    /// `FinalizeBudget` (plan §5 Commit 5): the budget is anchored at
    /// executor return — the moment this method is called — and covers the
    /// repository-lock wait, the append, and the retry backoff.
    ///
    /// The candidate is immutable for the whole call: every attempt
    /// publishes byte-identical facts. Corruption, identity, material, and
    /// digest mismatches fail closed immediately; only durability failures
    /// retry inside the 60-second envelope.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn publish_terminal_under_budget(
        &self,
        invocation: lancet_protocol::InvocationId,
        outcome: lancet_audit::invocation_store::TerminalOutcome,
        material: lancet_audit::invocation_store::TerminalMaterial,
        evidence: lancet_audit::invocation_store::TerminalEvidence,
    ) -> Result<(), FinalizeFailure<lancet_audit::invocation_store::InvocationStoreError>> {
        let clock = TokioFinalizeClock;
        let budget = FinalizeBudget::anchored_at(clock.now());
        self.publish_terminal_with_budget(invocation, outcome, material, evidence, budget)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn publish_terminal_with_budget(
        &self,
        invocation: lancet_protocol::InvocationId,
        outcome: lancet_audit::invocation_store::TerminalOutcome,
        material: lancet_audit::invocation_store::TerminalMaterial,
        evidence: lancet_audit::invocation_store::TerminalEvidence,
        budget: FinalizeBudget,
    ) -> Result<(), FinalizeFailure<lancet_audit::invocation_store::InvocationStoreError>> {
        let clock = TokioFinalizeClock;
        let store = Arc::clone(&self.invocations);
        // The frozen candidate is shared immutably by every attempt; each
        // retry publishes byte-identical facts.
        let candidate = Arc::new((invocation, outcome, material, evidence));
        finalize_under_budget(&clock, budget, move || {
            let store = Arc::clone(&store);
            let candidate = Arc::clone(&candidate);
            async move {
                match tokio::task::spawn_blocking(move || {
                    let (invocation, outcome, material, evidence) = &*candidate;
                    store.publish_terminal(
                        *invocation,
                        outcome.clone(),
                        material.clone(),
                        evidence.clone(),
                    )
                })
                .await
                {
                    Ok(Ok(())) => AttemptOutcome::Published,
                    Ok(Err(error)) => AttemptOutcome::Failed { error },
                    Err(join) => AttemptOutcome::Failed {
                        error: lancet_audit::invocation_store::InvocationStoreError::Corruption {
                            detail: format!("blocking publication task failed: {join}"),
                        },
                    },
                }
            }
        })
        .await
    }

    pub(crate) async fn revalidate_draft_under_budget(
        &self,
        candidate: lattice_session::pending::ResultDraft,
        budget: FinalizeBudget,
    ) -> Result<
        lattice_session::pending::DraftClassification,
        FinalizeFailure<lancet_audit::invocation_store::InvocationStoreError>,
    > {
        let clock = TokioFinalizeClock;
        let candidate = Arc::new(candidate);
        let classification = Arc::new(std::sync::Mutex::new(None));
        finalize_under_budget(&clock, budget, {
            let candidate = Arc::clone(&candidate);
            let classification = Arc::clone(&classification);
            move || {
                let candidate = Arc::clone(&candidate);
                let classification = Arc::clone(&classification);
                async move {
                    match self.classify_draft((*candidate).clone()).await {
                        Ok(value) => {
                            *classification.lock().expect("classification lock") = Some(value);
                            AttemptOutcome::Published
                        }
                        Err(error) => AttemptOutcome::Failed {
                            error: coordinator_error_for_finalize(error),
                        },
                    }
                }
            }
        })
        .await?;
        (*classification.lock().expect("classification lock")).ok_or_else(|| {
            FinalizeFailure::Fatal(
                lancet_audit::invocation_store::InvocationStoreError::Corruption {
                    detail: "draft revalidation completed without a classification".into(),
                },
            )
        })
    }
}

fn coordinator_error_for_finalize(
    error: CoordinatorIoError,
) -> lancet_audit::invocation_store::InvocationStoreError {
    match error {
        CoordinatorIoError::Session(lattice_session::SessionError::Store(error)) => {
            lancet_audit::invocation_store::InvocationStoreError::Store(error)
        }
        other => lancet_audit::invocation_store::InvocationStoreError::Corruption {
            detail: format!("draft revalidation failed: {other}"),
        },
    }
}

/// Production Turn coordinator. Owns the durable Session/Invocation saga.
pub(crate) struct TurnCoordinator {
    pub(crate) io: CoordinatorStoreIo,
    pub(crate) ids: Arc<dyn IdSource>,
}
impl TurnCoordinator {
    pub(crate) fn new(io: CoordinatorStoreIo, ids: Arc<dyn IdSource>) -> Self {
        Self { io, ids }
    }

    pub(crate) async fn reconcile_pending(
        &self,
        session_id: String,
        lock: Arc<lattice_session::FlockGuard>,
        workspace_history: Arc<WorkspaceHistory>,
    ) -> Result<Option<lattice_session::TurnCommit>, CoordinatorIoError> {
        loop {
            let Some(tip) = self.io.read_active_turn(session_id.clone()).await? else {
                return Ok(None);
            };
            let (plan_key, candidates) = match &tip.snapshot.stage {
                lattice_session::pending::PendingStage::BindingPlanned { plan, .. }
                | lattice_session::pending::PendingStage::Bound { plan } => (
                    plan.plan_key,
                    plan.bindings
                        .iter()
                        .map(|binding| binding.invocation_id)
                        .collect(),
                ),
                lattice_session::pending::PendingStage::StepClosing(state) => (
                    state.plan_key,
                    state
                        .bindings
                        .iter()
                        .map(|binding| binding.invocation_id)
                        .collect(),
                ),
                _ => (
                    PlanKey {
                        turn_id: tip.snapshot.turn_id,
                        model_step_ordinal: 0,
                    },
                    Vec::new(),
                ),
            };
            let facts = if candidates.is_empty() {
                InvocationFactsSnapshot::empty()
            } else {
                self.io.pin_invocation_facts(plan_key, candidates).await?
            };
            match lattice_session::pending::reconcile(&tip.snapshot, &facts)? {
                lattice_session::pending::ReconciliationDecision::ReviseCandidates(request) => {
                    let store = self.io.session_store();
                    let ids = Arc::clone(&self.ids);
                    let lock = Arc::clone(&lock);
                    tokio::task::spawn_blocking(move || {
                        store.execute_candidate_revision(
                            &request,
                            &tip,
                            tip.snapshot.recorded_at.ok_or_else(|| {
                                SessionError::JournalCorruption {
                                    detail: "pending journal commit has no readable committer time"
                                        .into(),
                                }
                            })?,
                            &lock,
                            || ids.new_invocation_id(),
                        )
                    })
                    .await
                    .map_err(CoordinatorIoError::Blocking)??;
                }
                lattice_session::pending::ReconciliationDecision::Closure(plan) => {
                    let mut finalized = None;
                    let mut workspace_guard = None;
                    for action in plan.actions {
                        let current = self
                            .io
                            .read_active_turn(session_id.clone())
                            .await?
                            .ok_or_else(|| {
                                SessionError::Conflict("active Turn disappeared".into())
                            })?;
                        let recorded_at = require_recorded_at(&current.snapshot)?;
                        match action {
                            lattice_session::pending::ReconciliationAction::ReserveExactPlan {
                                plan_key,
                                bindings,
                            } => {
                                let plan = lattice_session::pending::BindingPlan {
                                    plan_key,
                                    message: match &current.snapshot.stage {
                                        lattice_session::pending::PendingStage::BindingPlanned { plan, .. }
                                        | lattice_session::pending::PendingStage::Bound { plan } => plan.message.clone(),
                                        _ => return Err(SessionError::JournalCorruption { detail: "reservation action outside binding stage".into() }.into()),
                                    },
                                    bindings,
                                };
                                self.io.reserve_plan(plan).await?;
                            }
                            lattice_session::pending::ReconciliationAction::TerminalizeInvocation { invocation, method, outcome, material } => {
                                match self.io.publish_terminal_under_budget(
                                    invocation,
                                    map_outcome(outcome),
                                    map_material(material),
                                    empty_terminal_evidence(&method),
                                ).await {
                                    Ok(()) => {}
                                    Err(FinalizeFailure::EvidenceDurabilityFailed { attempts }) => {
                                        return Err(CoordinatorIoError::EvidenceDurabilityFailed { attempts });
                                    }
                                    Err(FinalizeFailure::Fatal(inner)) => {
                                        return Err(CoordinatorIoError::InvocationStore(inner));
                                    }
                                }
                            }
                            lattice_session::pending::ReconciliationAction::PublishBound { plan } => {
                                self.io.advance_turn(session_id.clone(), lattice_session::pending::PendingStage::Bound { plan }, recorded_at, Arc::clone(&lock)).await?;
                            }
                            lattice_session::pending::ReconciliationAction::AdvanceStepClosing { state } => {
                                self.io.advance_turn(session_id.clone(), lattice_session::pending::PendingStage::StepClosing(state), recorded_at, Arc::clone(&lock)).await?;
                            }
                            lattice_session::pending::ReconciliationAction::PublishStepClosed { kind } => {
                                self.io.advance_turn(session_id.clone(), lattice_session::pending::PendingStage::StepClosed { kind }, recorded_at, Arc::clone(&lock)).await?;
                            }
                            lattice_session::pending::ReconciliationAction::PublishTurnClosing { candidate } => {
                                let (guard, checkpoint) = acquire_workspace_checkpoint(Arc::clone(&workspace_history)).await?;
                                let candidate = candidate
                                    .with_workspace_history_checkpoint(checkpoint)
                                    .map_err(CoordinatorIoError::Session)?;
                                self.io.advance_turn(session_id.clone(), lattice_session::pending::PendingStage::TurnClosing(lattice_session::pending::TurnClosingFacts { turn_id: current.snapshot.turn_id, candidate }), recorded_at, Arc::clone(&lock)).await?;
                                workspace_guard = Some(guard);
                            }
                            lattice_session::pending::ReconciliationAction::FinalizeTurn => {
                                if workspace_guard.is_none() {
                                    let (guard, current_checkpoint) = acquire_workspace_checkpoint(Arc::clone(&workspace_history)).await?;
                                    let pinned = match &current.snapshot.stage {
                                        lattice_session::pending::PendingStage::TurnClosing(facts) => &facts.candidate.workspace_history_checkpoint,
                                        lattice_session::pending::PendingStage::Committed(facts) => &facts.candidate.workspace_history_checkpoint,
                                        other => return Err(SessionError::JournalCorruption { detail: format!("finalize action at {} has no pinned checkpoint", other.stage_name()) }.into()),
                                    };
                                    if pinned != &current_checkpoint {
                                        return Err(SessionError::JournalCorruption { detail: format!("pinned workspace checkpoint {pinned:?} differs from current {current_checkpoint:?}") }.into());
                                    }
                                    workspace_guard = Some(guard);
                                }
                                finalized = Some(self.io.finalize_turn(session_id.clone(), current.snapshot.turn_id, Arc::clone(&lock)).await?);
                            }
                        }
                    }
                    return Ok(finalized);
                }
            }
        }
    }
}

async fn acquire_workspace_checkpoint(
    workspace_history: Arc<WorkspaceHistory>,
) -> Result<
    (
        lattice_session::FlockGuard,
        lattice_session::pending::WorkspaceHistoryCheckpoint,
    ),
    CoordinatorIoError,
> {
    tokio::task::spawn_blocking(move || {
        let guard = workspace_history
            .lock_workspace_operation(
                lattice_session::LockMode::Shared,
                lattice_session::Blocking::Block,
            )
            .map_err(|error| CoordinatorIoError::WorkspaceHistory(error.to_string()))?
            .ok_or_else(|| {
                CoordinatorIoError::WorkspaceHistory(
                    "production Turn has disabled workspace history".into(),
                )
            })?;
        let current = workspace_history
            .current()
            .map_err(|error| CoordinatorIoError::WorkspaceHistory(error.to_string()))?;
        let checkpoint =
            lattice_session::pending::WorkspaceHistoryCheckpoint::from_current(current)
                .map_err(CoordinatorIoError::Session)?;
        Ok((guard, checkpoint))
    })
    .await
    .map_err(CoordinatorIoError::Blocking)?
}

fn require_recorded_at(
    snapshot: &lattice_session::pending::PendingSnapshot,
) -> Result<i64, CoordinatorIoError> {
    snapshot.recorded_at.ok_or_else(|| {
        SessionError::JournalCorruption {
            detail: "pending journal commit has no readable committer time".into(),
        }
        .into()
    })
}

fn map_outcome(
    outcome: lattice_session::pending::ExecutionOutcome,
) -> lancet_audit::invocation_store::TerminalOutcome {
    use lancet_audit::invocation_store::{IndeterminateReason, NotExecutedReason, TerminalOutcome};
    use lattice_session::pending::ExecutionOutcome;
    match outcome {
        ExecutionOutcome::Succeeded => TerminalOutcome::Succeeded,
        ExecutionOutcome::Failed => TerminalOutcome::Failed,
        ExecutionOutcome::Rejected => TerminalOutcome::Rejected,
        ExecutionOutcome::Cancelled => TerminalOutcome::Cancelled,
        ExecutionOutcome::NotExecuted { reason } => TerminalOutcome::NotExecuted {
            reason: match reason {
                lattice_session::pending::NotExecutedReason::BeforeDispatch => {
                    NotExecutedReason::BeforeDispatch
                }
                lattice_session::pending::NotExecutedReason::CancelledBeforeDispatch => {
                    NotExecutedReason::CancelledBeforeDispatch
                }
                lattice_session::pending::NotExecutedReason::PrecedingInvocationIndeterminate => {
                    NotExecutedReason::PrecedingInvocationIndeterminate
                }
            },
        },
        ExecutionOutcome::Indeterminate { .. } => TerminalOutcome::Indeterminate {
            reason: IndeterminateReason::MayHaveOccurred,
        },
    }
}

fn map_material(
    material: lattice_session::pending::ResultMaterial,
) -> lancet_audit::invocation_store::TerminalMaterial {
    use lancet_audit::invocation_store::{TerminalMaterial, UnavailableMaterialReason};
    match material {
        lattice_session::pending::ResultMaterial::Available {
            digest,
            byte_length,
        } => TerminalMaterial::Available {
            digest,
            byte_length,
        },
        lattice_session::pending::ResultMaterial::Unavailable {
            reason,
            digest,
            byte_length,
        } => TerminalMaterial::Unavailable {
            reason: match reason {
                lattice_session::pending::UnavailableMaterialReason::DraftPublicationFailed => {
                    UnavailableMaterialReason::DraftPublicationFailed
                }
                lattice_session::pending::UnavailableMaterialReason::OutputTooLarge => {
                    UnavailableMaterialReason::OutputTooLarge
                }
                lattice_session::pending::UnavailableMaterialReason::NotExecuted => {
                    UnavailableMaterialReason::NotExecuted
                }
            },
            digest,
            byte_length,
        },
    }
}

fn empty_terminal_evidence(tool: &str) -> lancet_audit::invocation_store::TerminalEvidence {
    lancet_audit::invocation_store::TerminalEvidence {
        admission: lancet_protocol::AdmissionEvidence {
            version: lancet_protocol::ADMISSION_EVIDENCE_VERSION,
            tool: tool.to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_audit::invocation_store::{BindingIdentity, ReservationBinding};
    use lancet_protocol::SessionId;
    use lattice_session::{ProjectStoreKey, StateLayout};

    /// Deterministic identity source: strictly-increasing v4-shaped IDs.
    struct SequencedIdSource {
        next: std::sync::atomic::AtomicU64,
    }

    impl IdSource for SequencedIdSource {
        fn new_invocation_id(&self) -> InvocationId {
            let index = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            InvocationId::parse(&format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
        }

        fn new_turn_id(&self) -> TurnId {
            let index = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            TurnId::parse(&format!("00000000-0000-4000-8000-{index:012x}")).unwrap()
        }
    }

    fn tokio_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
    }

    #[test]
    fn adapters_read_real_stores_off_the_async_workers() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let key = ProjectStoreKey::derive(tmp.path()).unwrap();

        let session = Arc::new(GitSessionGraphStore::create_layout(&layout).unwrap());
        let invocations = Arc::new(InvocationStore::create(&layout, &key).unwrap());
        let io = CoordinatorStoreIo::new(session, invocations, key).unwrap();

        let session_id = "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c".to_string();
        let plan_key = PlanKey {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            model_step_ordinal: 0,
        };

        tokio_runtime().block_on(async {
            // No Session and no pending Turn: both reads are ordinary empties.
            assert!(
                io.read_active_turn(session_id.clone())
                    .await
                    .unwrap()
                    .is_none()
            );
            let facts = io
                .pin_invocation_facts(
                    plan_key,
                    vec![InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap()],
                )
                .await
                .unwrap();
            assert_eq!(facts, InvocationFactsSnapshot::empty());
        });
    }

    #[test]
    fn workspace_checkpoint_guard_excludes_mutation_until_session_finalization_releases_it() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let key = ProjectStoreKey::derive(tmp.path()).unwrap();
        let history = Arc::new(
            WorkspaceHistory::create(lancet_api::WorkspaceHistoryBacking::for_key(layout, key))
                .unwrap(),
        );

        tokio_runtime().block_on(async {
            let (guard, checkpoint) = acquire_workspace_checkpoint(Arc::clone(&history))
                .await
                .unwrap();
            assert_eq!(
                checkpoint,
                lattice_session::WorkspaceHistoryCheckpoint::Empty
            );
            assert!(
                history
                    .lock_workspace_operation(
                        lattice_session::LockMode::Exclusive,
                        lattice_session::Blocking::NonBlocking,
                    )
                    .is_err(),
                "the shared closing guard must exclude a mutation"
            );
            drop(guard);
            assert!(
                history
                    .lock_workspace_operation(
                        lattice_session::LockMode::Exclusive,
                        lattice_session::Blocking::NonBlocking,
                    )
                    .is_ok(),
                "the mutation lock is available only after finalization drops the guard"
            );
        });
    }

    #[test]
    fn coordinator_rejects_an_invocation_store_for_another_project() {
        let tmp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let key = ProjectStoreKey::derive(tmp.path()).unwrap();
        let other_key = ProjectStoreKey::derive(other.path()).unwrap();
        let session = Arc::new(GitSessionGraphStore::create_layout(&layout).unwrap());
        let invocations = Arc::new(InvocationStore::create(&layout, &other_key).unwrap());
        let before = invocations.pinned_view().unwrap().commit_oid;

        let err = match CoordinatorStoreIo::new(session, Arc::clone(&invocations), key) {
            Ok(_) => panic!("mismatched project stores must fail closed"),
            Err(err) => err,
        };
        assert!(matches!(err, CoordinatorIoError::ProjectBinding(_)));
        assert_eq!(invocations.pinned_view().unwrap().commit_oid, before);
    }

    #[test]
    fn pin_invocation_facts_round_trips_a_real_reservation() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let key = ProjectStoreKey::derive(tmp.path()).unwrap();

        let session = Arc::new(GitSessionGraphStore::create_layout(&layout).unwrap());
        let invocations = Arc::new(InvocationStore::create(&layout, &key).unwrap());
        let io = CoordinatorStoreIo::new(session, Arc::clone(&invocations), key).unwrap();
        let turn_id = TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap();
        let plan_key = PlanKey {
            turn_id,
            model_step_ordinal: 2,
        };
        let invocation = InvocationId::parse("00000000-0000-4000-8000-000000000007").unwrap();
        let binding = ReservationBinding {
            invocation_id: invocation,
            identity: BindingIdentity {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
                turn_id,
                model_step_ordinal: 2,
                part_ordinal: 1,
                method: "observe.workspace".to_string(),
                descriptor_identity: "d".repeat(64),
                arguments_digest: lattice_conversation::arguments_digest(&serde_json::json!({})),
            },
        };

        // Reserve through the store directly, then read the same facts
        // through the async adapter.
        invocations.reserve_batch(plan_key, &[binding]).unwrap();

        tokio_runtime().block_on(async {
            let facts = io
                .pin_invocation_facts(plan_key, vec![invocation])
                .await
                .unwrap();
            let plan = facts.plan.clone().expect("reservation is visible");
            assert_eq!(plan.plan_key, plan_key);
            assert_eq!(
                facts.lifecycle_of(&invocation),
                Some(&lattice_session::pending::InvocationLifecycleFact::Reserved)
            );
            assert!(matches!(
                facts.validate_against_plan(&plan_key, &plan.bindings),
                Ok(lattice_session::pending::PlanValidation::ExactMatch)
            ));
        });
    }

    #[test]
    fn random_id_source_emits_strictly_parsable_ids() {
        let source = RandomIdSource;
        let first = source.new_invocation_id();
        let second = source.new_invocation_id();
        assert_ne!(first, second);
        assert_ne!(source.new_turn_id(), source.new_turn_id());
        assert_eq!(first.as_str().len(), 36);
    }

    #[test]
    fn sequenced_id_source_is_deterministic() {
        let source = SequencedIdSource {
            next: std::sync::atomic::AtomicU64::new(0),
        };
        assert_eq!(
            source.new_invocation_id().as_str(),
            "00000000-0000-4000-8000-000000000000"
        );
        assert_eq!(
            source.new_turn_id().as_str(),
            "00000000-0000-4000-8000-000000000001"
        );
    }
}
