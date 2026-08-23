// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

// Private #139-B shell module: nothing routes through it until the Commit 6
// production switch, so the non-test lib build sees it as dead code.
#![allow(dead_code)]

//! Private `TurnCoordinator` dependency bundle (ADR 0005 §10, GitHub #139-B).
//!
//! Commit 1 of the #139-B hard cut lands only the coordinator's composition
//! shape: the blocking store adapters, the ledger→facts projection seam, and
//! the injectable identity source. Nothing in the production turn path
//! constructs or drives this bundle — `stream_turn` still runs the numeric /
//! SQLite-backed agent-loop orchestration until the single production switch,
//! and the temporary structural allowlist in `cargo xtask lancet-boundary`
//! tracks exactly those legacy files.
//!
//! Locking contract carried by every adapter (ADR 0005 §5, plan §2.2): the
//! coordinator holds the full-Turn `SessionExecutionLock`, but no repository
//! write lock is held across an await or a call into another repository.
//! Every blocking Git/filesystem operation invoked from async production
//! code runs through [`tokio::task::spawn_blocking`].

use std::sync::Arc;

use lancet_audit::invocation_store::InvocationStore;
use lancet_protocol::{InvocationId, PlanKey, TurnId};
use lattice_session::pending::InvocationFactsSnapshot;
use lattice_session::{GitSessionGraphStore, PendingTurnTip, SessionError};
use thiserror::Error;

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
}

impl CoordinatorStoreIo {
    pub(crate) fn new(
        session: Arc<GitSessionGraphStore>,
        invocations: Arc<InvocationStore>,
    ) -> Self {
        Self {
            session,
            invocations,
        }
    }

    /// Read the current PendingTurn journal tip for one Session.
    ///
    /// `Ok(None)` when no Turn is pending. Runs off the async workers.
    pub(crate) async fn read_active_turn(
        &self,
        session_id: String,
    ) -> Result<Option<PendingTurnTip>, CoordinatorIoError> {
        let store = Arc::clone(&self.session);
        tokio::task::spawn_blocking(move || store.read_active_turn(&session_id))
            .await
            .map_err(CoordinatorIoError::Blocking)?
            .map_err(CoordinatorIoError::Session)
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

/// The coordinator's private dependency bundle (plan §5 Commit 1).
///
/// Shell only. It is not wired into `stream_turn`, `run_turn`, or the
/// temporary CEP adapter, and it performs no reservation, `DispatchStarted`
/// publication, or Turn-commit I/O until the production hard cut.
pub(crate) struct CoordinatorDeps {
    pub(crate) io: CoordinatorStoreIo,
    pub(crate) ids: Arc<dyn IdSource>,
}

impl CoordinatorDeps {
    pub(crate) fn new(io: CoordinatorStoreIo, ids: Arc<dyn IdSource>) -> Self {
        Self { io, ids }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_audit::invocation_store::{BindingIdentity, ReservationBinding};
    use lancet_protocol::{ModelMessageAddress, SessionId, ToolCallAddress};
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
        let io = CoordinatorStoreIo::new(session, invocations);

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
    fn pin_invocation_facts_round_trips_a_real_reservation() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let key = ProjectStoreKey::derive(tmp.path()).unwrap();

        let session = Arc::new(GitSessionGraphStore::create_layout(&layout).unwrap());
        let invocations = Arc::new(InvocationStore::create(&layout, &key).unwrap());
        let io = CoordinatorStoreIo::new(session, Arc::clone(&invocations));
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

    #[test]
    fn address_types_are_nameable_for_future_binding_plans() {
        // The coordinator's binding plan needs structural addresses; this
        // keeps the import set honest while the shell is not yet wired.
        let _address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
                turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
                model_step_ordinal: 0,
            },
            part_ordinal: 0,
        };
    }
}
