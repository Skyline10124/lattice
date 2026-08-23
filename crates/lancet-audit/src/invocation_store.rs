// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Non-production InvocationStore on the `lattice-store` kernel
//! (ADR 0005 §11, GitHub #139-A).
//!
//! One authoritative append-only ledger plus one ordered high-water witness
//! per project. Publication advances the ledger first and the witness second;
//! readers accept only an ancestor witness and write retries repair a lagging witness:
//!
//! ```text
//! refs/lattice/invocations/ledger
//! refs/lattice/invocations/high-water
//! ```
//!
//! Every lifecycle transition (reservation, `DispatchStarted`, terminal +
//! evidence) is a direct child of the prior tip, published by CAS.
//! `reserve_batch` is all-or-nothing: one commit records all bindings plus
//! their owning `PlanKey`. The ledger cannot be rebased, merged, truncated,
//! rewritten, or deleted.
//!
//! This module does **not** connect to the production Tool dispatch path,
//! JSON-RPC transport, or SQLite audit store, and it owns only audit-domain
//! types: nothing here depends on `lattice-session`. Projection into the
//! session-domain reconciliation vocabulary lives in the non-production
//! integration seam (`tests/invocation_qualification.rs`), which is the only
//! place that sees both domains.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use gix::objs::tree::EntryKind;
use lancet_protocol::{
    AdmissionEvidence, DeniedOperationsRecord, InvocationId, ObservedEffectsRecord, PlanKey,
};
use lattice_store::{
    ProjectStoreKey, RefExpectation, RefTxnEdit, StateLayout, StoreError, StoreRepo, TreeSpec,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The single authoritative ledger ref (ADR 0005 §11).
pub const LEDGER_REF: &str = "refs/lattice/invocations/ledger";
/// Monotonic topology witness advanced after `LEDGER_REF` in a second transaction.
const LEDGER_WITNESS_REF: &str = "refs/lattice/invocations/high-water";

/// Repository directory and store kind are separate identities.
const REPO_NAME: &str = "invocations.git";
const STORE_KIND: &str = "invocations";

/// Schema version — bump on any record-shape change.
const STORE_SCHEMA_VERSION: u32 = 3;

/// Version of the per-commit event payload.
const LEDGER_EVENT_VERSION: u32 = 1;

/// Tree path for the one append-only event inside each ledger commit.
const EVENT_BLOB: &str = "event.json";

/// File name of the derived on-disk snapshot cache in the store directory.
const DERIVED_CHECKPOINT_FILE: &str = "derived-checkpoint.json";

/// Schema version of the derived on-disk snapshot cache.
const DERIVED_CHECKPOINT_SCHEMA_VERSION: u32 = 1;

/// Tree path for the cryptographic state digest inside each ledger commit.
const STATE_DIGEST_BLOB: &str = "state_digest";

/// Smallest number of commits between anchored checkpoints.
///
/// The real interval scales with state size (see [`checkpoint_interval`]), so
/// this only bounds the interval for a small ledger.
const CHECKPOINT_MIN_INTERVAL: u64 = 32;

/// Records per commit of checkpoint interval.
///
/// A checkpoint costs O(state) bytes to compute and write. Taking one every
/// O(state) commits makes that cost amortize to O(1) bytes per commit, so the
/// cumulative checkpoint write volume over N transitions is O(N) rather than
/// the O(N^2) a fixed interval produces.
const CHECKPOINT_STATE_DIVISOR: u64 = 4;

/// Commits between anchored checkpoints at the given state size.
fn checkpoint_interval(state_len: usize) -> u64 {
    let scaled = (state_len as u64) / CHECKPOINT_STATE_DIVISOR;
    scaled.max(CHECKPOINT_MIN_INTERVAL)
}

// ---------------------------------------------------------------------------
// Record types (stored in ledger commits)
// ---------------------------------------------------------------------------

/// One ledger entry per invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvocationRecord {
    /// Owning plan's structural address.
    pub owner: PlanKey,
    /// Current lifecycle state.
    pub state: InvocationState,
    /// Binding identity: method, descriptor, arguments digest.
    pub binding: BindingIdentity,
}

/// Binding identity recorded at reservation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingIdentity {
    pub session_id: lancet_protocol::SessionId,
    pub turn_id: lancet_protocol::TurnId,
    pub model_step_ordinal: u32,
    pub part_ordinal: u32,
    pub method: String,
    pub descriptor_identity: String,
    #[serde(with = "hex_digest")]
    pub arguments_digest: [u8; 32],
}

/// Lifecycle states (mirrors `InvocationLifecycleFact` but store-internal).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum InvocationState {
    Reserved,
    DispatchStarted,
    Terminal {
        outcome: TerminalOutcome,
        error: Option<TerminalErrorClassification>,
        material: TerminalMaterial,
        evidence: Box<TerminalEvidence>,
    },
}

/// Closed-sum outcome stored in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalOutcome {
    Succeeded,
    Failed,
    Rejected,
    Cancelled,
    NotExecuted { reason: NotExecutedReason },
    Indeterminate { reason: IndeterminateReason },
}

/// Typed terminal error classification, derived from the closed outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalErrorClassification {
    ToolFailure,
    AdmissionRejected,
    Cancelled,
    NotExecuted(NotExecutedReason),
    Indeterminate(IndeterminateReason),
}

impl TerminalOutcome {
    fn error_classification(&self) -> Option<TerminalErrorClassification> {
        match self {
            Self::Succeeded => None,
            Self::Failed => Some(TerminalErrorClassification::ToolFailure),
            Self::Rejected => Some(TerminalErrorClassification::AdmissionRejected),
            Self::Cancelled => Some(TerminalErrorClassification::Cancelled),
            Self::NotExecuted { reason } => Some(TerminalErrorClassification::NotExecuted(*reason)),
            Self::Indeterminate { reason } => {
                Some(TerminalErrorClassification::Indeterminate(*reason))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotExecutedReason {
    BeforeDispatch,
    CancelledBeforeDispatch,
    PrecedingInvocationIndeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndeterminateReason {
    MayHaveOccurred,
}

/// Result material stored in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalMaterial {
    Available {
        #[serde(with = "hex_digest")]
        digest: [u8; 32],
        byte_length: u64,
    },
    Unavailable {
        reason: UnavailableMaterialReason,
        #[serde(default, with = "hex_digest_option")]
        digest: Option<[u8; 32]>,
        #[serde(default)]
        byte_length: Option<u64>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableMaterialReason {
    DraftPublicationFailed,
    OutputTooLarge,
    NotExecuted,
}

/// Structured Cell evidence published atomically with the terminal state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalEvidence {
    pub admission: AdmissionEvidence,
    pub effects: ObservedEffectsRecord,
    pub denied: DeniedOperationsRecord,
}

/// The authoritative exact ordered reservation for one plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanReservation {
    pub plan_key: PlanKey,
    pub bindings: Vec<ReservationBinding>,
}

/// One immutable event stored by one authoritative ledger commit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum LedgerEvent {
    Genesis {
        version: u32,
    },
    Reserve {
        plan: PlanReservation,
    },
    Transition {
        invocation_id: InvocationId,
        state: InvocationState,
    },
}

/// The replayed current state at a fixed ledger tip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerSnapshot {
    pub plans: Vec<PlanReservation>,
    pub invocations: BTreeMap<InvocationId, InvocationRecord>,
}

/// One durable lifecycle transition in authoritative commit order.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleTransition {
    /// Commit that published this transition; an operator may use it as a tail cursor.
    pub commit_oid: gix::ObjectId,
    /// Invocation whose state changed.
    pub invocation_id: InvocationId,
    /// Complete record as it existed immediately after this transition.
    pub record: InvocationRecord,
}

type TransitionGroup = (gix::ObjectId, Vec<LifecycleTransition>);

/// A read-only ledger snapshot pinned to one reachable authoritative commit.
#[derive(Debug, Clone, PartialEq)]
pub struct PinnedLedgerView {
    pub commit_oid: gix::ObjectId,
    pub snapshot: LedgerSnapshot,
}

impl PinnedLedgerView {
    #[must_use]
    pub fn query_invocations(
        &self,
        ids: &[InvocationId],
    ) -> BTreeMap<InvocationId, InvocationRecord> {
        ids.iter()
            .filter_map(|id| {
                self.snapshot
                    .invocations
                    .get(id)
                    .cloned()
                    .map(|record| (*id, record))
            })
            .collect()
    }

    #[must_use]
    pub fn plan(&self, plan_key: PlanKey) -> Option<&PlanReservation> {
        self.snapshot
            .plans
            .iter()
            .find(|plan| plan.plan_key == plan_key)
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// InvocationStore-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum InvocationStoreError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),

    #[error("duplicate invocation {id} in another plan")]
    DuplicateInvocation { id: InvocationId },

    #[error("plan key {plan_key:?} already recorded with different bindings")]
    DifferentPlan { plan_key: PlanKey },

    #[error("invalid reservation: {detail}")]
    InvalidReservation { detail: String },

    #[error("invocation {id} is not in state {expected} (actual: {actual})")]
    InvalidTransition {
        id: InvocationId,
        expected: String,
        actual: String,
    },

    #[error("invocation {id} not found")]
    NotFound { id: InvocationId },

    #[error("ledger commit {commit_oid} is not reachable from the authoritative tip")]
    UnreachableCommit { commit_oid: gix::ObjectId },

    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("ledger changed during a lock-free read; retry the read")]
    ConcurrentRead,

    #[error("ledger corruption: {detail}")]
    Corruption { detail: String },
}

pub type Result<T> = std::result::Result<T, InvocationStoreError>;

impl InvocationStoreError {
    /// Narrow retryability classification for terminal/evidence publication
    /// under the fixed `FinalizeBudget` (ADR 0005 §12, #139-B Commit 5).
    ///
    /// Only explicit kernel store/durability failures and the lock-free read
    /// race are retriable. Corruption, topology, binding, identity, material
    /// and digest mismatches fail immediately and are never retried.
    pub fn is_durability_retryable(&self) -> bool {
        match self {
            Self::Store(store) => matches!(
                store,
                StoreError::Io { .. }
                    | StoreError::Durability { .. }
                    | StoreError::Busy { .. }
                    | StoreError::Lock { .. }
            ),
            Self::ConcurrentRead => true,
            Self::DuplicateInvocation { .. }
            | Self::DifferentPlan { .. }
            | Self::InvalidReservation { .. }
            | Self::InvalidTransition { .. }
            | Self::NotFound { .. }
            | Self::UnreachableCommit { .. }
            | Self::Serde(_)
            | Self::Corruption { .. } => false,
        }
    }
}

// ---------------------------------------------------------------------------
// InvocationStore
// ---------------------------------------------------------------------------

#[derive(Default)]
struct InvocationStoreMetrics {
    lock_wait_ns: AtomicU64,
    lock_wait_max_ns: AtomicU64,
    lock_count: AtomicU64,
    append_barrier_ns: AtomicU64,
    append_barrier_max_ns: AtomicU64,
    append_count: AtomicU64,
}

/// Raw qualification measurements accumulated by one store handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct InvocationStoreMetricsSnapshot {
    pub repository_lock_wait_ns: u64,
    pub repository_lock_wait_max_ns: u64,
    pub repository_lock_count: u64,
    pub append_fsync_barrier_ns: u64,
    pub append_fsync_barrier_max_ns: u64,
    pub append_count: u64,
}

struct ValidatedWriteState {
    tip: gix::ObjectId,
    snapshot: LedgerSnapshot,
    /// Commits published since the newest anchor **on the ledger**, not since
    /// this handle was opened.
    ///
    /// Deriving this from the ledger rather than from handle lifetime is what
    /// makes anchoring survive a restart. A writer that reopens the store
    /// before every write is the normal shape of this workload; counting only
    /// within one handle's lifetime means such a writer never reaches the
    /// interval, never anchors, and leaves every later cold read replaying the
    /// whole ledger.
    commits_since_anchor: u64,
}

/// What one publication must anchor.
///
/// An ordinary transition writes only its event, so the work under the
/// repository write lock is bounded by the event. Every `checkpoint_interval`
/// commits one publication additionally anchors the resulting state digest in
/// its commit tree and republishes the derived checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicationKind {
    Ordinary,
    Anchor,
}

/// A resolved write state plus the ledger's own distance from its newest
/// anchor, so a freshly opened handle inherits the cadence instead of
/// restarting it.
struct ResolvedWriteState {
    tip: gix::ObjectId,
    snapshot: LedgerSnapshot,
    commits_since_anchor: u64,
}

/// The outcome of a full genesis replay.
struct RebuiltLedger {
    snapshot: LedgerSnapshot,
    anchor: Option<(gix::ObjectId, LedgerSnapshot)>,
    commits_since_anchor: u64,
}

/// Cryptographic SHA-256 digest of a snapshot's canonical JSON bytes.
///
/// Anchored in *anchor* ledger commit trees (`state_digest`) so a derived
/// snapshot cache can be verified against the immutable git commit instead of
/// becoming an unauthenticated second authority (P1-1).
///
/// This is O(state bytes). It runs only when an anchor commit is published,
/// never on an ordinary transition, so the per-transition critical section
/// stays O(event bytes) and independent of accumulated project history.
fn compute_state_digest(
    snapshot: &LedgerSnapshot,
) -> std::result::Result<[u8; 32], lattice_store::StoreError> {
    let bytes = lattice_store::canon::canonical_json_bytes(snapshot)?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().into())
}

/// Derived on-disk snapshot cache bound to an exact ledger commit (L-GH139A-02).
///
/// This is a derived, rebuildable projection, not a second authority: the
/// authoritative state remains `refs/lattice/invocations/ledger` and its
/// monotonic high-water witness. If this file is missing, corrupted, or
/// refers to a commit that is not reachable from the authoritative tip, it is
/// discarded and the snapshot is reconstructed from genesis.
///
/// `ledger_tip` always names an *anchor* commit, the only kind whose tree
/// carries the `state_digest` this snapshot is authenticated against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DerivedCheckpoint {
    schema_version: u32,
    #[serde(with = "hex_oid")]
    ledger_tip: gix::ObjectId,
    snapshot: LedgerSnapshot,
}

/// Non-production Git InvocationStore (#139-A).
pub struct InvocationStore {
    pub(crate) repo: StoreRepo,
    metrics: Arc<InvocationStoreMetrics>,
    validated_write_state: Mutex<Option<ValidatedWriteState>>,
}

impl InvocationStore {
    /// Open an existing invocations store (strict — fails if missing).
    pub fn open(layout: &StateLayout, key: &ProjectStoreKey) -> Result<Self> {
        let repo =
            StoreRepo::open_project(layout, key, REPO_NAME, STORE_KIND, STORE_SCHEMA_VERSION)?;
        Ok(Self {
            repo,
            metrics: Arc::new(InvocationStoreMetrics::default()),
            validated_write_state: Mutex::new(None),
        })
    }

    /// Read accumulated raw qualification instrumentation.
    #[must_use]
    pub fn metrics_snapshot(&self) -> InvocationStoreMetricsSnapshot {
        InvocationStoreMetricsSnapshot {
            repository_lock_wait_ns: self.metrics.lock_wait_ns.load(Ordering::Relaxed),
            repository_lock_wait_max_ns: self.metrics.lock_wait_max_ns.load(Ordering::Relaxed),
            repository_lock_count: self.metrics.lock_count.load(Ordering::Relaxed),
            append_fsync_barrier_ns: self.metrics.append_barrier_ns.load(Ordering::Relaxed),
            append_fsync_barrier_max_ns: self.metrics.append_barrier_max_ns.load(Ordering::Relaxed),
            append_count: self.metrics.append_count.load(Ordering::Relaxed),
        }
    }

    /// Reset qualification counters before a measured run.
    pub fn reset_metrics(&self) {
        self.metrics.lock_wait_ns.store(0, Ordering::Relaxed);
        self.metrics.lock_wait_max_ns.store(0, Ordering::Relaxed);
        self.metrics.lock_count.store(0, Ordering::Relaxed);
        self.metrics.append_barrier_ns.store(0, Ordering::Relaxed);
        self.metrics
            .append_barrier_max_ns
            .store(0, Ordering::Relaxed);
        self.metrics.append_count.store(0, Ordering::Relaxed);
    }

    /// Create a new invocations store with a genesis commit.
    pub fn create(layout: &StateLayout, key: &ProjectStoreKey) -> Result<Self> {
        StoreRepo::create_project(
            layout,
            key,
            REPO_NAME,
            STORE_KIND,
            STORE_SCHEMA_VERSION,
            |repo| {
                let genesis = LedgerEvent::Genesis {
                    version: LEDGER_EVENT_VERSION,
                };
                let genesis_snapshot = LedgerSnapshot::default();
                let genesis_digest = compute_state_digest(&genesis_snapshot)?;
                let blob = lattice_store::canon::canonical_json_bytes(&genesis)?;
                let blob_oid = repo.write_blob(&blob)?;
                let digest_blob_oid = repo.write_blob(&genesis_digest)?;
                let mut tree = TreeSpec::new();
                tree.upsert(EVENT_BLOB, EntryKind::Blob, blob_oid);
                tree.upsert(STATE_DIGEST_BLOB, EntryKind::Blob, digest_blob_oid);
                let tree_oid = repo.write_tree(&tree)?;
                let commit_oid = repo.write_commit_at(tree_oid, &[], "genesis", 0)?;
                repo.ref_transaction(&[
                    RefTxnEdit {
                        name: LEDGER_REF.to_string(),
                        expect: RefExpectation::MustNotExist,
                        new: Some(commit_oid),
                    },
                    RefTxnEdit {
                        name: LEDGER_WITNESS_REF.to_string(),
                        expect: RefExpectation::MustNotExist,
                        new: Some(commit_oid),
                    },
                ])?;
                let checkpoint = DerivedCheckpoint {
                    schema_version: DERIVED_CHECKPOINT_SCHEMA_VERSION,
                    ledger_tip: commit_oid,
                    snapshot: LedgerSnapshot::default(),
                };
                let bytes = lattice_store::canon::canonical_json_bytes(&checkpoint)?;
                let checkpoint_path = repo.git_dir().join(DERIVED_CHECKPOINT_FILE);
                std::fs::write(&checkpoint_path, &bytes).map_err(|source| StoreError::Io {
                    operation: "writing genesis derived checkpoint",
                    path: checkpoint_path,
                    source,
                })?;
                Ok(())
            },
        )?;
        Self::open(layout, key)
    }

    /// Read the current ledger snapshot from the authoritative tip.
    pub fn snapshot(&self) -> Result<(gix::ObjectId, LedgerSnapshot)> {
        let view = self.pinned_view()?;
        Ok((view.commit_oid, view.snapshot))
    }

    /// Read a topology-validated view pinned to the authoritative tip.
    pub fn pinned_view(&self) -> Result<PinnedLedgerView> {
        let tip = self.ledger_tip()?;
        self.pinned_view_inner(tip, tip)
    }

    /// Reopen a topology-validated view at an ancestor of the current tip.
    pub fn pinned_view_at(&self, commit_oid: gix::ObjectId) -> Result<PinnedLedgerView> {
        let tip = self.ledger_tip()?;
        self.pinned_view_inner(tip, commit_oid)
    }

    /// Return lifecycle transitions after an optional reachable exclusive cursor.
    ///
    /// The authoritative tip is pinned once. Immutable commits are then read without
    /// taking the repository write lock. Batch reservations preserve binding order;
    /// commit order is oldest-to-newest and the cursor commit itself is excluded.
    pub fn tail(&self, after: Option<gix::ObjectId>) -> Result<Vec<LifecycleTransition>> {
        let tip = self.ledger_tip()?;
        let (genesis, groups) = self.validated_transition_groups(tip)?;
        let start = match after {
            Some(cursor) if cursor == genesis => 0,
            Some(cursor) => groups
                .iter()
                .position(|(commit_oid, _)| *commit_oid == cursor)
                .map(|index| index + 1)
                .ok_or(InvocationStoreError::UnreachableCommit { commit_oid: cursor })?,
            None => 0,
        };

        Ok(groups[start..]
            .iter()
            .flat_map(|(_, group)| group.iter().cloned())
            .collect())
    }

    /// Reserve an entire batch atomically (ADR 0005 §11).
    ///
    /// Repeating the same `PlanKey` with byte-identical bindings is
    /// idempotent. A different binding for a recorded `PlanKey`, duplicate
    /// `InvocationId` in another plan, or domain mismatch fails closed.
    pub fn reserve_batch(&self, plan_key: PlanKey, bindings: &[ReservationBinding]) -> Result<()> {
        let prepared = PreparedEvent::new(
            LedgerEvent::Reserve {
                plan: PlanReservation {
                    plan_key,
                    bindings: bindings.to_vec(),
                },
            },
            format!("reserve {}", plan_key.turn_id),
        )?;

        let _lock = self.measured_write_lock()?;
        let mut write_state = self.write_state()?;
        let state = write_state.as_mut().expect("write state initialized");

        if let Some(existing) = state
            .snapshot
            .plans
            .iter()
            .find(|plan| plan.plan_key == plan_key)
        {
            return if existing.bindings == bindings {
                self.confirm_ledger_durable()
            } else {
                Err(InvocationStoreError::DifferentPlan { plan_key })
            };
        }

        validate_event_against_snapshot(&state.snapshot, &prepared.event)?;
        self.append(&mut write_state, &prepared)
    }

    /// Publish `DispatchStarted` for one invocation.
    pub fn dispatch_started(&self, id: InvocationId) -> Result<()> {
        let prepared = PreparedEvent::new(
            LedgerEvent::Transition {
                invocation_id: id,
                state: InvocationState::DispatchStarted,
            },
            format!("dispatch {id}"),
        )?;

        let _lock = self.measured_write_lock()?;
        let mut write_state = self.write_state()?;
        let state = write_state.as_mut().expect("write state initialized");
        let record = state
            .snapshot
            .invocations
            .get(&id)
            .ok_or(InvocationStoreError::NotFound { id })?;

        match &record.state {
            InvocationState::Reserved => {}
            InvocationState::DispatchStarted => return self.confirm_ledger_durable(),
            other => {
                return Err(InvocationStoreError::InvalidTransition {
                    id,
                    expected: "Reserved".to_string(),
                    actual: format!("{other:?}"),
                });
            }
        }

        validate_event_against_snapshot(&state.snapshot, &prepared.event)?;
        self.append(&mut write_state, &prepared)
    }

    /// Publish terminal outcome + evidence atomically for one invocation.
    pub fn publish_terminal(
        &self,
        id: InvocationId,
        outcome: TerminalOutcome,
        material: TerminalMaterial,
        evidence: TerminalEvidence,
    ) -> Result<()> {
        // Terminal evidence is the largest payload the ledger carries, so its
        // shape validation and canonical serialization matter most here.
        let prepared = PreparedEvent::new(
            LedgerEvent::Transition {
                invocation_id: id,
                state: InvocationState::Terminal {
                    error: outcome.error_classification(),
                    outcome,
                    material,
                    evidence: Box::new(evidence),
                },
            },
            format!("terminal {id}"),
        )?;
        let LedgerEvent::Transition { state: next, .. } = &prepared.event else {
            unreachable!("prepared a Transition event");
        };

        let _lock = self.measured_write_lock()?;
        let mut write_state = self.write_state()?;
        let state = write_state.as_mut().expect("write state initialized");
        let record = state
            .snapshot
            .invocations
            .get(&id)
            .ok_or(InvocationStoreError::NotFound { id })?;

        if let InvocationState::Terminal { .. } = &record.state {
            return if &record.state == next {
                self.confirm_ledger_durable()
            } else {
                Err(InvocationStoreError::InvalidTransition {
                    id,
                    expected: format!("idempotent retry of {:?}", record.state),
                    actual: format!("{next:?}"),
                })
            };
        }

        validate_event_against_snapshot(&state.snapshot, &prepared.event)?;
        self.append(&mut write_state, &prepared)
    }

    /// Query the state of specific invocation IDs.
    pub fn query_invocations(
        &self,
        ids: &[InvocationId],
    ) -> Result<BTreeMap<InvocationId, InvocationRecord>> {
        let (_, snapshot) = self.snapshot()?;
        let mut result = BTreeMap::new();
        for id in ids {
            if let Some(record) = snapshot.invocations.get(id) {
                result.insert(*id, record.clone());
            }
        }
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn measured_write_lock(&self) -> Result<lattice_store::FlockGuard> {
        let started = Instant::now();
        let guard = self.repo.write_lock()?;
        let elapsed = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.metrics
            .lock_wait_ns
            .fetch_add(elapsed, Ordering::Relaxed);
        self.metrics
            .lock_wait_max_ns
            .fetch_max(elapsed, Ordering::Relaxed);
        self.metrics.lock_count.fetch_add(1, Ordering::Relaxed);
        Ok(guard)
    }

    fn record_append_barrier(&self, started: Instant) {
        let elapsed = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.metrics
            .append_barrier_ns
            .fetch_add(elapsed, Ordering::Relaxed);
        self.metrics
            .append_barrier_max_ns
            .fetch_max(elapsed, Ordering::Relaxed);
        self.metrics.append_count.fetch_add(1, Ordering::Relaxed);
    }

    fn ledger_tip(&self) -> Result<gix::ObjectId> {
        for _ in 0..8 {
            let tip = self.required_ref(LEDGER_REF, "ledger ref missing")?;
            let witness = self.required_ref(LEDGER_WITNESS_REF, "ledger witness ref missing")?;
            if self.repo.ref_target(LEDGER_REF)? != Some(tip) {
                std::hint::spin_loop();
                continue;
            }
            if tip == witness || self.is_first_parent_ancestor(witness, tip)? {
                return Ok(tip);
            }
            if self.repo.ref_target(LEDGER_WITNESS_REF)? != Some(witness) {
                std::hint::spin_loop();
                continue;
            }
            return Err(InvocationStoreError::Corruption {
                detail: format!(
                    "ledger tip {tip} is behind or diverges from monotonic witness {witness}"
                ),
            });
        }
        Err(InvocationStoreError::ConcurrentRead)
    }

    fn required_ref(&self, name: &str, missing: &str) -> Result<gix::ObjectId> {
        self.repo
            .ref_target(name)?
            .ok_or_else(|| InvocationStoreError::Corruption {
                detail: missing.to_string(),
            })
    }

    fn is_first_parent_ancestor(
        &self,
        ancestor: gix::ObjectId,
        mut descendant: gix::ObjectId,
    ) -> Result<bool> {
        loop {
            if descendant == ancestor {
                return Ok(true);
            }
            let parents = self.repo.commit_parents(descendant)?;
            match parents.as_slice() {
                [parent] => descendant = *parent,
                [] => return Ok(false),
                _ => {
                    return Err(InvocationStoreError::Corruption {
                        detail: format!("ledger commit {descendant} has {} parents", parents.len()),
                    });
                }
            }
        }
    }

    fn read_event_at(&self, commit_oid: gix::ObjectId) -> Result<LedgerEvent> {
        let blob = self
            .repo
            .read_tree_blob(commit_oid, EVENT_BLOB)
            .map_err(|error| InvocationStoreError::Corruption {
                detail: format!("ledger commit {commit_oid} event is unreadable: {error}"),
            })?;
        serde_json::from_slice(&blob).map_err(|error| InvocationStoreError::Corruption {
            detail: format!("ledger commit {commit_oid} event is invalid: {error}"),
        })
    }

    fn ordered_events(&self, tip: gix::ObjectId) -> Result<Vec<(gix::ObjectId, LedgerEvent)>> {
        let mut events = Vec::new();
        let mut cursor = tip;
        loop {
            let parents = self.repo.commit_parents(cursor)?;
            if parents.len() > 1 {
                return Err(InvocationStoreError::Corruption {
                    detail: format!("ledger commit {cursor} has {} parents", parents.len()),
                });
            }
            events.push((cursor, self.read_event_at(cursor)?));
            let Some(parent) = parents.first().copied() else {
                break;
            };
            cursor = parent;
        }
        events.reverse();
        Ok(events)
    }

    fn events_after(
        &self,
        ancestor: gix::ObjectId,
        tip: gix::ObjectId,
    ) -> Result<Vec<(gix::ObjectId, LedgerEvent)>> {
        let mut events = Vec::new();
        let mut cursor = tip;
        while cursor != ancestor {
            let parents = self.repo.commit_parents(cursor)?;
            if parents.len() != 1 {
                return Err(InvocationStoreError::Corruption {
                    detail: format!(
                        "ledger commit {cursor} does not continue cached ancestor {ancestor}"
                    ),
                });
            }
            events.push((cursor, self.read_event_at(cursor)?));
            cursor = parents[0];
        }
        events.reverse();
        Ok(events)
    }

    fn pinned_view_inner(
        &self,
        tip: gix::ObjectId,
        target: gix::ObjectId,
    ) -> Result<PinnedLedgerView> {
        let snapshot = if target == tip {
            self.resolve_snapshot_at_tip(tip)?
        } else {
            self.validated_snapshot_at(tip, target)?
        };
        Ok(PinnedLedgerView {
            commit_oid: target,
            snapshot,
        })
    }

    /// Advance the anchor distance after one suffix event has been applied.
    ///
    /// Each ordinary commit adds one; every *authenticated* anchor resets to
    /// zero.  The digest is recomputed only when this commit actually carries
    /// a `state_digest`, so ordinary transitions remain O(event) while sparse
    /// anchors pay the O(state) verification cost.  A right-length but wrong
    /// digest is corruption, not an anchor.
    fn advance_anchor_distance(
        &self,
        base: u64,
        commit_oid: gix::ObjectId,
        snapshot: &LedgerSnapshot,
    ) -> Result<u64> {
        let Some(recorded) = self.recorded_state_digest_at(commit_oid)? else {
            return Ok(base + 1);
        };
        let actual = compute_state_digest(snapshot)?;
        if actual != recorded {
            return Err(InvocationStoreError::Corruption {
                detail: format!(
                    "commit {commit_oid} state_digest does not match its replayed snapshot"
                ),
            });
        }
        Ok(0)
    }

    /// Replay a suffix while carrying the ledger-derived anchor distance.
    ///
    /// Keeping application and anchor validation in one helper prevents read,
    /// cold-write, and peer-writer paths from disagreeing about what counts as
    /// an anchor.  Digest verification is sparse; it is never performed for
    /// ordinary commits.
    fn replay_suffix(
        &self,
        snapshot: &mut LedgerSnapshot,
        base: u64,
        suffix: &[(gix::ObjectId, LedgerEvent)],
    ) -> Result<u64> {
        let mut distance = base;
        for (commit_oid, event) in suffix {
            apply_replayed_event(snapshot, event, *commit_oid)?;
            distance = self.advance_anchor_distance(distance, *commit_oid, snapshot)?;
        }
        Ok(distance)
    }

    /// The anchored state digest of `commit_oid`, or `None` when it is an
    /// ordinary (non-anchor) commit whose tree carries no digest.
    ///
    /// Test-only tree inspection. Replay paths must use
    /// [`Self::recorded_state_digest_at`], which fails closed on a malformed
    /// blob instead of treating it as an ordinary commit.
    #[cfg(test)]
    fn try_read_state_digest_at(&self, commit_oid: gix::ObjectId) -> Option<[u8; 32]> {
        let bytes = self
            .repo
            .read_tree_blob(commit_oid, STATE_DIGEST_BLOB)
            .ok()?;
        let digest: [u8; 32] = bytes.try_into().ok()?;
        Some(digest)
    }

    /// Read a state digest while distinguishing an ordinary commit (no blob)
    /// from a malformed anchor blob.  The older `try_read_*` helper is kept
    /// for test-only tree inspection; replay paths must not turn a malformed
    /// digest into an ordinary commit by swallowing the error.
    fn recorded_state_digest_at(&self, commit_oid: gix::ObjectId) -> Result<Option<[u8; 32]>> {
        let tree = self.repo.commit_tree(commit_oid)?;
        let files = self.repo.tree_files(tree)?;
        let Some(bytes) = files.get(STATE_DIGEST_BLOB) else {
            return Ok(None);
        };
        if bytes.len() != 32 {
            return Err(InvocationStoreError::Corruption {
                detail: format!("commit {commit_oid} state_digest blob is not 32 bytes"),
            });
        }
        let mut digest = [0u8; 32];
        digest.copy_from_slice(bytes);
        Ok(Some(digest))
    }

    /// Read the cryptographically anchored state digest from an anchor commit.
    fn read_state_digest_at(&self, commit_oid: gix::ObjectId) -> Result<[u8; 32]> {
        let bytes = self.repo.read_tree_blob(commit_oid, STATE_DIGEST_BLOB)?;
        if bytes.len() != 32 {
            return Err(InvocationStoreError::Corruption {
                detail: format!("commit {commit_oid} state_digest blob is not 32 bytes"),
            });
        }
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&bytes);
        Ok(digest)
    }

    fn checkpoint_path(&self) -> PathBuf {
        self.repo.git_dir().join(DERIVED_CHECKPOINT_FILE)
    }

    fn save_derived_checkpoint(&self, tip: gix::ObjectId, snapshot: &LedgerSnapshot) -> Result<()> {
        let checkpoint = DerivedCheckpoint {
            schema_version: DERIVED_CHECKPOINT_SCHEMA_VERSION,
            ledger_tip: tip,
            snapshot: snapshot.clone(),
        };
        let bytes = lattice_store::canon::canonical_json_bytes(&checkpoint)?;
        let git_dir = self.repo.git_dir();
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let temp = git_dir.join(format!(
            ".checkpoint.tmp-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&temp, &bytes).map_err(|source| StoreError::Io {
            operation: "writing derived checkpoint temporary",
            path: temp.clone(),
            source,
        })?;
        let target = self.checkpoint_path();
        std::fs::rename(&temp, &target).map_err(|source| StoreError::Io {
            operation: "publishing derived checkpoint",
            path: target,
            source,
        })?;
        Ok(())
    }

    /// Load and authenticate a derived snapshot checkpoint against the ledger.
    ///
    /// The checkpoint is accepted if and only if:
    /// 1. Its schema version matches `DERIVED_CHECKPOINT_SCHEMA_VERSION`;
    /// 2. Its `ledger_tip` is equal to or an ancestor of `tip`;
    /// 3. Its snapshot shape passes `validate_snapshot_shape`;
    /// 4. Its snapshot canonical JSON SHA-256 matches the `state_digest`
    ///    cryptographically anchored in `ledger_tip`'s commit tree.
    ///
    /// Any mismatch or corruption returns `None`, discarding the unauthenticated
    /// cache and forcing a rebuild from genesis (P1-1).
    fn load_derived_checkpoint(
        &self,
        tip: gix::ObjectId,
    ) -> Option<(gix::ObjectId, LedgerSnapshot)> {
        let path = self.checkpoint_path();
        let bytes = std::fs::read(&path).ok()?;
        let checkpoint: DerivedCheckpoint = serde_json::from_slice(&bytes).ok()?;
        if checkpoint.schema_version != DERIVED_CHECKPOINT_SCHEMA_VERSION {
            return None;
        }
        let checkpoint_tip = checkpoint.ledger_tip;
        if checkpoint_tip != tip
            && !self
                .is_first_parent_ancestor(checkpoint_tip, tip)
                .unwrap_or(false)
        {
            return None;
        }
        if validate_snapshot_shape(&checkpoint.snapshot).is_err() {
            return None;
        }
        let expected_digest = self.read_state_digest_at(checkpoint_tip).ok()?;
        let actual_digest = compute_state_digest(&checkpoint.snapshot).ok()?;
        if actual_digest != expected_digest {
            return None;
        }
        Some((checkpoint_tip, checkpoint.snapshot))
    }

    /// Resolve and validate the snapshot at `tip` for write_state, using
    /// the verified derived checkpoint to avoid replaying history from genesis.
    fn resolve_snapshot_for_write(&self, tip: gix::ObjectId) -> Result<ResolvedWriteState> {
        if let Some((checkpoint_tip, mut snapshot)) = self.load_derived_checkpoint(tip) {
            if checkpoint_tip == tip {
                return Ok(ResolvedWriteState {
                    tip,
                    snapshot,
                    commits_since_anchor: 0,
                });
            }
            if let Ok(events) = self.events_after(checkpoint_tip, tip) {
                let commits_since_anchor = self.replay_suffix(&mut snapshot, 0, &events).ok();
                // Authentication happened at `checkpoint_tip`; the delta
                // events were read from the immutable authoritative ledger, so
                // the fold is authenticated too.  Every sparse suffix anchor
                // is checked by `replay_suffix`; a mismatch falls through to
                // a full rebuild, which selects the newest older valid anchor.
                if let Some(commits_since_anchor) = commits_since_anchor
                    && validate_snapshot_shape(&snapshot).is_ok()
                {
                    // `checkpoint_tip` is an anchor, so the distance restarts
                    // there and is carried forward across the replayed suffix.
                    return Ok(ResolvedWriteState {
                        tip,
                        snapshot,
                        commits_since_anchor,
                    });
                }
            }
        }

        // Full rebuild from genesis. Re-publish the checkpoint at the newest
        // anchor commit seen on the way, so a deleted or rejected checkpoint
        // file self-heals instead of making every later cold read replay the
        // whole ledger again.
        let rebuilt = self.rebuild_from_genesis(tip)?;
        if let Some((anchor_oid, anchor_snapshot)) = rebuilt.anchor {
            let _ = self.save_derived_checkpoint(anchor_oid, &anchor_snapshot);
        }
        Ok(ResolvedWriteState {
            tip,
            snapshot: rebuilt.snapshot,
            commits_since_anchor: rebuilt.commits_since_anchor,
        })
    }

    /// Replay the whole ledger, returning the snapshot at `tip`, the newest
    /// *verified* anchor with the snapshot as of that commit, and the ledger's
    /// distance from that anchor.
    ///
    /// An anchor is accepted here only when its recorded digest equals the
    /// digest of the snapshot just replayed at that commit. A corrupt digest
    /// of the right length would otherwise be selected, written into the
    /// checkpoint, and then rejected by every subsequent load, making each
    /// cold read rebuild from genesis forever.
    fn rebuild_from_genesis(&self, tip: gix::ObjectId) -> Result<RebuiltLedger> {
        let events = self.ordered_events(tip)?;
        let Some((genesis, first)) = events.first() else {
            return Err(InvocationStoreError::Corruption {
                detail: "ledger has no genesis commit".to_string(),
            });
        };
        validate_genesis_event(*genesis, first)?;

        // The digest is recomputed only where a commit actually records one.
        // Anchors are sparse, so this stays O(anchors x state) overall; doing
        // it unconditionally would recompute an O(state) digest at every
        // commit and put the O(N^2) replay right back, this time under the
        // write lock of a cold writer.  A missing blob is an ordinary commit;
        // a present but malformed or unreadable blob is corruption.
        let mut snapshot = LedgerSnapshot::default();
        let mut anchor = None;
        let mut commits_since_anchor = 0u64;
        let verify =
            |store: &Self, commit_oid: gix::ObjectId, snapshot: &LedgerSnapshot| -> Result<bool> {
                match store.recorded_state_digest_at(commit_oid)? {
                    Some(recorded) => Ok(recorded == compute_state_digest(snapshot)?),
                    None => Ok(false),
                }
            };
        if verify(self, *genesis, &snapshot)? {
            anchor = Some((*genesis, snapshot.clone()));
        } else {
            commits_since_anchor = 1;
        }
        for (commit_oid, event) in events.iter().skip(1) {
            apply_replayed_event(&mut snapshot, event, *commit_oid)?;
            if verify(self, *commit_oid, &snapshot)? {
                anchor = Some((*commit_oid, snapshot.clone()));
                commits_since_anchor = 0;
            } else {
                commits_since_anchor += 1;
            }
        }
        validate_snapshot_shape(&snapshot)?;
        Ok(RebuiltLedger {
            snapshot,
            anchor,
            commits_since_anchor,
        })
    }

    /// Resolve and validate the snapshot at `tip` for read operations.
    fn resolve_snapshot_at_tip(&self, tip: gix::ObjectId) -> Result<LedgerSnapshot> {
        let mut cache = self
            .validated_write_state
            .lock()
            .unwrap_or_else(|p| p.into_inner());

        if let Some(cached) = cache.as_mut() {
            if cached.tip == tip {
                return Ok(cached.snapshot.clone());
            }
            if self
                .is_first_parent_ancestor(cached.tip, tip)
                .unwrap_or(false)
                && let Ok(events) = self.events_after(cached.tip, tip)
            {
                let original = cached.snapshot.clone();
                let distance = self
                    .replay_suffix(&mut cached.snapshot, cached.commits_since_anchor, &events)
                    .ok();
                if let Some(distance) = distance
                    && validate_snapshot_shape(&cached.snapshot).is_ok()
                {
                    // A read never publishes a checkpoint: `tip` is usually an
                    // ordinary commit, and a checkpoint naming a commit with no
                    // anchored digest could never be authenticated afterwards.
                    cached.tip = tip;
                    cached.commits_since_anchor = distance;
                    return Ok(cached.snapshot.clone());
                }
                // `replay_suffix` mutates in place.  If an event or sparse
                // anchor was corrupt, discard the partial projection before
                // falling back to the authenticated checkpoint/genesis path.
                cached.snapshot = original;
            }
        }

        let resolved = self.resolve_snapshot_for_write(tip)?;
        let snapshot = resolved.snapshot.clone();
        *cache = Some(ValidatedWriteState {
            tip: resolved.tip,
            snapshot: resolved.snapshot,
            commits_since_anchor: resolved.commits_since_anchor,
        });
        Ok(snapshot)
    }

    fn validated_transition_groups(
        &self,
        tip: gix::ObjectId,
    ) -> Result<(gix::ObjectId, Vec<TransitionGroup>)> {
        let events = self.ordered_events(tip)?;
        let Some((genesis, first)) = events.first() else {
            return Err(InvocationStoreError::Corruption {
                detail: "ledger has no genesis commit".to_string(),
            });
        };
        validate_genesis_event(*genesis, first)?;

        let mut snapshot = LedgerSnapshot::default();
        let mut groups = Vec::with_capacity(events.len().saturating_sub(1));
        for (commit_oid, event) in events.iter().skip(1) {
            let transitions = apply_replayed_event(&mut snapshot, event, *commit_oid)?;
            groups.push((*commit_oid, transitions));
        }
        validate_snapshot_shape(&snapshot)?;
        Ok((*genesis, groups))
    }

    fn validated_snapshot_at(
        &self,
        tip: gix::ObjectId,
        target: gix::ObjectId,
    ) -> Result<LedgerSnapshot> {
        let events = self.ordered_events(tip)?;
        let Some((genesis, first)) = events.first() else {
            return Err(InvocationStoreError::Corruption {
                detail: "ledger has no genesis commit".to_string(),
            });
        };
        validate_genesis_event(*genesis, first)?;

        let mut snapshot = LedgerSnapshot::default();
        let mut target_snapshot = (*genesis == target).then(|| snapshot.clone());
        for (commit_oid, event) in events.iter().skip(1) {
            apply_replayed_event(&mut snapshot, event, *commit_oid)?;
            if *commit_oid == target {
                target_snapshot = Some(snapshot.clone());
            }
        }
        validate_snapshot_shape(&snapshot)?;
        target_snapshot.ok_or(InvocationStoreError::UnreachableCommit { commit_oid: target })
    }

    fn confirm_ledger_durable(&self) -> Result<()> {
        let tip = self.ledger_tip()?;
        self.synchronize_witness(tip)?;
        self.repo.fsync_pending(&[LEDGER_REF, LEDGER_WITNESS_REF])?;
        Ok(())
    }

    fn synchronize_witness(&self, tip: gix::ObjectId) -> Result<()> {
        let witness = self.required_ref(LEDGER_WITNESS_REF, "ledger witness ref missing")?;
        if witness == tip {
            return Ok(());
        }
        if !self.is_first_parent_ancestor(witness, tip)? {
            return Err(InvocationStoreError::Corruption {
                detail: format!(
                    "ledger tip {tip} is behind or diverges from monotonic witness {witness}"
                ),
            });
        }
        self.repo.ref_transaction(&[RefTxnEdit {
            name: LEDGER_WITNESS_REF.to_string(),
            expect: RefExpectation::MustMatch(witness),
            new: Some(tip),
        }])?;
        Ok(())
    }

    /// Return the cached validated state while the caller holds the repository write lock.
    fn write_state(&self) -> Result<std::sync::MutexGuard<'_, Option<ValidatedWriteState>>> {
        let tip = self.ledger_tip()?;
        self.synchronize_witness(tip)?;
        let mut state = self
            .validated_write_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match state.as_mut() {
            Some(cached) if cached.tip != tip => {
                // Another writer advanced the ledger. Fold its commits in and
                // carry the anchor distance across them, so a peer's anchor
                // counts for this handle too.  Replay mutates in place, so a
                // failed suffix must restore the cache the way the read path
                // does; otherwise the next write replays the same events
                // against a partially advanced snapshot.
                let events = self.events_after(cached.tip, tip)?;
                let original_snapshot = cached.snapshot.clone();
                let original_distance = cached.commits_since_anchor;
                let replayed = self
                    .replay_suffix(&mut cached.snapshot, original_distance, &events)
                    .and_then(|distance| {
                        validate_snapshot_shape(&cached.snapshot).map(|()| distance)
                    });
                match replayed {
                    Ok(distance) => {
                        cached.commits_since_anchor = distance;
                        cached.tip = tip;
                    }
                    Err(error) => {
                        cached.snapshot = original_snapshot;
                        cached.commits_since_anchor = original_distance;
                        return Err(error);
                    }
                }
            }
            Some(_) => {}
            None => {
                let resolved = self.resolve_snapshot_for_write(tip)?;
                *state = Some(ValidatedWriteState {
                    tip: resolved.tip,
                    snapshot: resolved.snapshot,
                    commits_since_anchor: resolved.commits_since_anchor,
                });
            }
        }
        Ok(state)
    }

    /// Apply one already-validated event and publish it.
    ///
    /// The cached snapshot is advanced **in place**: cloning it here would make
    /// the critical section O(state) and, across N transitions, O(N^2) work.
    /// In-place mutation means a failed publication leaves the cache ahead of
    /// the durable tip, so any failure drops the cache; the next call rebuilds
    /// it from the last anchored checkpoint plus a bounded delta.
    fn append(
        &self,
        write_state: &mut std::sync::MutexGuard<'_, Option<ValidatedWriteState>>,
        prepared: &PreparedEvent,
    ) -> Result<()> {
        let state = write_state.as_mut().expect("write state initialized");
        let parent = state.tip;

        if apply_ledger_event(&mut state.snapshot, &prepared.event, parent).is_err() {
            // The snapshot may be partially advanced; it is no longer a
            // faithful projection of `parent`.
            **write_state = None;
            return Err(InvocationStoreError::Corruption {
                detail: "validated event could not be applied to the cached snapshot".to_string(),
            });
        }

        let kind = if state.commits_since_anchor + 1
            >= checkpoint_interval(state.snapshot.invocations.len())
        {
            PublicationKind::Anchor
        } else {
            PublicationKind::Ordinary
        };

        let published = match kind {
            PublicationKind::Ordinary => self.publish_event(parent, prepared, None),
            PublicationKind::Anchor => self.publish_event(parent, prepared, Some(&state.snapshot)),
        };
        let commit_oid = match published {
            Ok(commit_oid) => commit_oid,
            Err(error) => {
                **write_state = None;
                return Err(error);
            }
        };

        state.tip = commit_oid;
        match kind {
            PublicationKind::Ordinary => state.commits_since_anchor += 1,
            PublicationKind::Anchor => {
                let _ = self.save_derived_checkpoint(commit_oid, &state.snapshot);
                state.commits_since_anchor = 0;
            }
        }
        Ok(())
    }

    /// Publish one immutable event as a child of `parent`.
    ///
    /// Everything this does is state-dependent or a durable side effect, so
    /// all of it must stay under the repository write lock. The event's
    /// canonical bytes and commit message were produced by [`PreparedEvent`]
    /// before the lock was taken.
    ///
    /// `anchor_snapshot` is `Some` only for an anchor commit. An ordinary
    /// commit therefore writes one small event blob and no snapshot-sized
    /// object, which is what keeps the critical section O(event bytes).
    fn publish_event(
        &self,
        parent: gix::ObjectId,
        prepared: &PreparedEvent,
        anchor_snapshot: Option<&LedgerSnapshot>,
    ) -> Result<gix::ObjectId> {
        let started = Instant::now();
        let blob_oid = self.repo.write_blob(&prepared.canonical)?;
        let mut tree = TreeSpec::new();
        tree.upsert(EVENT_BLOB, EntryKind::Blob, blob_oid);
        if let Some(snapshot) = anchor_snapshot {
            let state_digest = compute_state_digest(snapshot)?;
            let digest_blob_oid = self.repo.write_blob(&state_digest)?;
            tree.upsert(STATE_DIGEST_BLOB, EntryKind::Blob, digest_blob_oid);
        }
        let tree_oid = self.repo.write_tree(&tree)?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let commit_oid = self
            .repo
            .write_commit_at(tree_oid, &[parent], &prepared.message, now)?;
        self.repo.ref_transaction(&[RefTxnEdit {
            name: LEDGER_REF.to_string(),
            expect: RefExpectation::MustMatch(parent),
            new: Some(commit_oid),
        }])?;
        self.repo.ref_transaction(&[RefTxnEdit {
            name: LEDGER_WITNESS_REF.to_string(),
            expect: RefExpectation::MustMatch(parent),
            new: Some(commit_oid),
        }])?;
        self.record_append_barrier(started);
        Ok(commit_oid)
    }
}

fn validate_snapshot_shape(snapshot: &LedgerSnapshot) -> Result<()> {
    let mut plan_keys = BTreeSet::new();
    let mut invocation_ids = BTreeSet::new();

    for plan in &snapshot.plans {
        if !plan_keys.insert(plan.plan_key) {
            return Err(InvocationStoreError::Corruption {
                detail: format!("duplicate plan reservation {:?}", plan.plan_key),
            });
        }
        if plan.bindings.is_empty() {
            return Err(InvocationStoreError::Corruption {
                detail: format!("plan {:?} has no bindings", plan.plan_key),
            });
        }

        let session_id = plan.bindings[0].identity.session_id;
        let mut previous_part = None;
        for binding in &plan.bindings {
            if binding.identity.session_id != session_id {
                return Err(InvocationStoreError::Corruption {
                    detail: format!("plan {:?} spans sessions", plan.plan_key),
                });
            }
            if binding.identity.turn_id != plan.plan_key.turn_id
                || binding.identity.model_step_ordinal != plan.plan_key.model_step_ordinal
            {
                return Err(InvocationStoreError::Corruption {
                    detail: format!(
                        "invocation {} binding address does not belong to plan",
                        binding.invocation_id
                    ),
                });
            }
            if previous_part.is_some_and(|part| binding.identity.part_ordinal <= part) {
                return Err(InvocationStoreError::Corruption {
                    detail: format!("plan {:?} bindings are not strictly ordered", plan.plan_key),
                });
            }
            previous_part = Some(binding.identity.part_ordinal);
            if !invocation_ids.insert(binding.invocation_id) {
                return Err(InvocationStoreError::Corruption {
                    detail: format!("duplicate invocation {}", binding.invocation_id),
                });
            }

            let record = snapshot
                .invocations
                .get(&binding.invocation_id)
                .ok_or_else(|| InvocationStoreError::Corruption {
                    detail: format!("reserved invocation {} is missing", binding.invocation_id),
                })?;
            if record.owner != plan.plan_key || record.binding != binding.identity {
                return Err(InvocationStoreError::Corruption {
                    detail: format!(
                        "invocation {} does not match its authoritative reservation",
                        binding.invocation_id
                    ),
                });
            }
            validate_invocation_state(binding.invocation_id, &record.state)?;
        }
    }

    if invocation_ids.len() != snapshot.invocations.len() {
        return Err(InvocationStoreError::Corruption {
            detail: "ledger contains an invocation without an authoritative reservation"
                .to_string(),
        });
    }
    Ok(())
}

fn terminal_material_matches_outcome(
    outcome: &TerminalOutcome,
    material: &TerminalMaterial,
) -> bool {
    match (outcome, material) {
        (
            TerminalOutcome::NotExecuted { .. },
            TerminalMaterial::Unavailable {
                reason: UnavailableMaterialReason::NotExecuted,
                ..
            },
        ) => true,
        (TerminalOutcome::NotExecuted { .. }, _)
        | (
            _,
            TerminalMaterial::Unavailable {
                reason: UnavailableMaterialReason::NotExecuted,
                ..
            },
        )
        | (TerminalOutcome::Indeterminate { .. }, TerminalMaterial::Available { .. }) => false,
        _ => true,
    }
}

fn validate_invocation_state(id: InvocationId, state: &InvocationState) -> Result<()> {
    let InvocationState::Terminal {
        outcome,
        material,
        error,
        evidence,
    } = state
    else {
        return Ok(());
    };

    if error != &outcome.error_classification()
        || evidence.admission.version != lancet_protocol::ADMISSION_EVIDENCE_VERSION
        || evidence.effects.version != lancet_protocol::OBSERVED_EFFECTS_RECORD_VERSION
        || evidence.denied.version != lancet_protocol::DENIED_OPERATIONS_RECORD_VERSION
    {
        return Err(InvocationStoreError::Corruption {
            detail: format!("invocation {id} has invalid terminal evidence or classification"),
        });
    }

    let material_valid = terminal_material_matches_outcome(outcome, material);
    if !material_valid {
        return Err(InvocationStoreError::Corruption {
            detail: format!("invocation {id} has invalid terminal material"),
        });
    }
    Ok(())
}

fn validate_genesis_event(commit_oid: gix::ObjectId, event: &LedgerEvent) -> Result<()> {
    match event {
        LedgerEvent::Genesis { version } if *version == LEDGER_EVENT_VERSION => Ok(()),
        _ => Err(InvocationStoreError::Corruption {
            detail: format!("ledger root {commit_oid} is not the versioned genesis event"),
        }),
    }
}

/// One ledger event with everything that does not need the repository write
/// lock already done.
///
/// The lock serializes every writer of one project's ledger, so work done
/// inside it is work every other writer waits through. Shape validation and
/// canonical serialization depend only on the event, so they happen here,
/// before the lock. What remains under the lock is exactly the part that
/// cannot move: read the tip, validate the transition against the current
/// snapshot, write the objects, CAS the ledger ref, advance the witness, and
/// run the durability barrier.
struct PreparedEvent {
    event: LedgerEvent,
    canonical: Vec<u8>,
    message: String,
}

impl PreparedEvent {
    fn new(event: LedgerEvent, message: String) -> Result<Self> {
        validate_event_shape(&event)?;
        let canonical = lattice_store::canon::canonical_json_bytes(&event)?;
        Ok(Self {
            event,
            canonical,
            message,
        })
    }
}

/// Full event admission: shape first, then the ledger state it must fit.
///
/// The write paths call the two halves separately so the shape half runs
/// before the repository write lock is taken; replay calls this.
fn validate_ledger_event(snapshot: &LedgerSnapshot, event: &LedgerEvent) -> Result<()> {
    validate_event_shape(event)?;
    validate_event_against_snapshot(snapshot, event)
}

/// Everything an event can be rejected for without reading the ledger.
///
/// This half is deliberately snapshot-free so it can run outside the
/// repository write lock: internal duplicate candidates, cross-session
/// batches, addresses that do not belong to the plan key, non-ascending part
/// ordinals, and malformed terminal evidence/material are all properties of
/// the event alone.
fn validate_event_shape(event: &LedgerEvent) -> Result<()> {
    match event {
        LedgerEvent::Genesis { .. } => Err(InvocationStoreError::Corruption {
            detail: "genesis event appeared after the ledger root".to_string(),
        }),
        LedgerEvent::Reserve { plan } => {
            if plan.bindings.is_empty() {
                return Err(InvocationStoreError::Corruption {
                    detail: "reservation batch must not be empty".to_string(),
                });
            }

            let session_id = plan.bindings[0].identity.session_id;
            if plan
                .bindings
                .iter()
                .any(|binding| binding.identity.session_id != session_id)
            {
                return Err(InvocationStoreError::InvalidReservation {
                    detail: "one plan reservation cannot span sessions".to_string(),
                });
            }

            let mut ids = BTreeSet::new();
            let mut previous_part = None;
            for binding in &plan.bindings {
                if !ids.insert(binding.invocation_id) {
                    return Err(InvocationStoreError::DuplicateInvocation {
                        id: binding.invocation_id,
                    });
                }
                if binding.identity.turn_id != plan.plan_key.turn_id
                    || binding.identity.model_step_ordinal != plan.plan_key.model_step_ordinal
                {
                    return Err(InvocationStoreError::Corruption {
                        detail: format!(
                            "invocation {} binding address does not belong to plan",
                            binding.invocation_id
                        ),
                    });
                }
                if previous_part.is_some_and(|part| binding.identity.part_ordinal <= part) {
                    return Err(InvocationStoreError::Corruption {
                        detail: "reservation part ordinals must strictly ascend".to_string(),
                    });
                }
                previous_part = Some(binding.identity.part_ordinal);
            }
            Ok(())
        }
        LedgerEvent::Transition {
            invocation_id,
            state,
        } => validate_invocation_state(*invocation_id, state),
    }
}

/// Everything that can only be decided against the current ledger state.
///
/// Runs under the repository write lock, against the snapshot the caller is
/// about to append to.
fn validate_event_against_snapshot(snapshot: &LedgerSnapshot, event: &LedgerEvent) -> Result<()> {
    match event {
        LedgerEvent::Genesis { .. } => Err(InvocationStoreError::Corruption {
            detail: "genesis event appeared after the ledger root".to_string(),
        }),
        LedgerEvent::Reserve { plan } => {
            if snapshot
                .plans
                .iter()
                .any(|existing| existing.plan_key == plan.plan_key)
            {
                return Err(InvocationStoreError::DifferentPlan {
                    plan_key: plan.plan_key,
                });
            }
            for binding in &plan.bindings {
                if let Some(existing) = snapshot.invocations.get(&binding.invocation_id) {
                    return if existing.owner == plan.plan_key {
                        Err(InvocationStoreError::Corruption {
                            detail: format!(
                                "invocation {} exists without its plan reservation",
                                binding.invocation_id
                            ),
                        })
                    } else {
                        Err(InvocationStoreError::DuplicateInvocation {
                            id: binding.invocation_id,
                        })
                    };
                }
            }
            Ok(())
        }
        LedgerEvent::Transition {
            invocation_id,
            state,
        } => {
            let record = snapshot
                .invocations
                .get(invocation_id)
                .ok_or(InvocationStoreError::NotFound { id: *invocation_id })?;
            let legal = matches!(
                (&record.state, state),
                (InvocationState::Reserved, InvocationState::DispatchStarted)
                    | (
                        InvocationState::Reserved,
                        InvocationState::Terminal {
                            outcome: TerminalOutcome::NotExecuted { .. },
                            material: TerminalMaterial::Unavailable {
                                reason: UnavailableMaterialReason::NotExecuted,
                                ..
                            },
                            ..
                        }
                    )
                    | (
                        InvocationState::DispatchStarted,
                        InvocationState::Terminal {
                            outcome: TerminalOutcome::Succeeded
                                | TerminalOutcome::Failed
                                | TerminalOutcome::Rejected
                                | TerminalOutcome::Cancelled
                                | TerminalOutcome::Indeterminate { .. },
                            ..
                        }
                    )
            );
            if !legal {
                return Err(InvocationStoreError::InvalidTransition {
                    id: *invocation_id,
                    expected: format!("legal successor of {:?}", record.state),
                    actual: format!("{state:?}"),
                });
            }
            Ok(())
        }
    }
}

fn apply_ledger_event(
    snapshot: &mut LedgerSnapshot,
    event: &LedgerEvent,
    commit_oid: gix::ObjectId,
) -> Result<Vec<LifecycleTransition>> {
    validate_ledger_event(snapshot, event)?;
    match event {
        LedgerEvent::Genesis { .. } => unreachable!("genesis rejected by validation"),
        LedgerEvent::Reserve { plan } => {
            let mut transitions = Vec::with_capacity(plan.bindings.len());
            for binding in &plan.bindings {
                let record = InvocationRecord {
                    owner: plan.plan_key,
                    state: InvocationState::Reserved,
                    binding: binding.identity.clone(),
                };
                snapshot
                    .invocations
                    .insert(binding.invocation_id, record.clone());
                transitions.push(LifecycleTransition {
                    commit_oid,
                    invocation_id: binding.invocation_id,
                    record,
                });
            }
            snapshot.plans.push(plan.clone());
            Ok(transitions)
        }
        LedgerEvent::Transition {
            invocation_id,
            state,
        } => {
            let record = snapshot
                .invocations
                .get_mut(invocation_id)
                .expect("validated invocation exists");
            record.state = state.clone();
            Ok(vec![LifecycleTransition {
                commit_oid,
                invocation_id: *invocation_id,
                record: record.clone(),
            }])
        }
    }
}

fn apply_replayed_event(
    snapshot: &mut LedgerSnapshot,
    event: &LedgerEvent,
    commit_oid: gix::ObjectId,
) -> Result<Vec<LifecycleTransition>> {
    apply_ledger_event(snapshot, event, commit_oid).map_err(|error| {
        if matches!(error, InvocationStoreError::Corruption { .. }) {
            error
        } else {
            InvocationStoreError::Corruption {
                detail: format!("invalid ledger event at {commit_oid}: {error}"),
            }
        }
    })
}

/// One binding in a `reserve_batch` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservationBinding {
    pub invocation_id: InvocationId,
    pub identity: BindingIdentity,
}

// ---------------------------------------------------------------------------
// Hex digest serde (same convention as lattice-session)
// ---------------------------------------------------------------------------

mod hex_oid {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(
        oid: &gix::ObjectId,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&oid.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<gix::ObjectId, D::Error> {
        let s = String::deserialize(deserializer)?;
        gix::ObjectId::from_hex(s.as_bytes()).map_err(D::Error::custom)
    }
}

mod hex_digest {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(
        digest: &[u8; 32],
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        serializer.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<[u8; 32], D::Error> {
        let s = String::deserialize(deserializer)?;
        if s.len() != 64 {
            return Err(D::Error::custom("expected 64 hex chars"));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = hex_val(chunk[0]).ok_or_else(|| D::Error::custom("invalid hex"))?;
            let lo = hex_val(chunk[1]).ok_or_else(|| D::Error::custom("invalid hex"))?;
            out[i] = (hi << 4) | lo;
        }
        Ok(out)
    }

    fn hex_val(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    }
}

mod hex_digest_option {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        digest: &Option<[u8; 32]>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match digest {
            Some(digest) => super::hex_digest::serialize(digest, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Option<[u8; 32]>, D::Error> {
        let value = Option::<String>::deserialize(deserializer)?;
        value
            .map(|value| {
                serde_json::from_value::<Digest>(serde_json::Value::String(value))
                    .map(|digest| digest.0)
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }

    #[derive(Deserialize)]
    struct Digest(#[serde(with = "super::hex_digest")] [u8; 32]);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::{InvocationId, PlanKey, SessionId, TurnId};
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    fn test_env() -> (TempDir, StateLayout, ProjectStoreKey) {
        let tmp = TempDir::new().unwrap();
        let layout = StateLayout::in_state_root(tmp.path());
        let key = ProjectStoreKey::derive(tmp.path()).unwrap();
        (tmp, layout, key)
    }

    fn plan_key(step: u32) -> PlanKey {
        PlanKey {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            model_step_ordinal: step,
        }
    }

    fn binding(index: u32) -> ReservationBinding {
        let id = InvocationId::parse(&format!("00000000-0000-4000-8000-{index:012x}")).unwrap();
        ReservationBinding {
            invocation_id: id,
            identity: BindingIdentity {
                session_id: SessionId::parse("aaaaaaaa111144448888cccccccccccc").unwrap(),
                turn_id: plan_key(0).turn_id,
                model_step_ordinal: 0,
                part_ordinal: index,
                method: "test.echo".to_string(),
                descriptor_identity: "test-descriptor".to_string(),
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

    fn write_forged_commit<T: Serialize>(
        store: &InvocationStore,
        value: &T,
        parents: &[gix::ObjectId],
    ) -> gix::ObjectId {
        let bytes = lattice_store::canon::canonical_json_bytes(value).unwrap();
        let blob_oid = store.repo.write_blob(&bytes).unwrap();
        let dummy_digest = [0u8; 32];
        let digest_blob_oid = store.repo.write_blob(&dummy_digest).unwrap();
        let mut tree = TreeSpec::new();
        tree.upsert(EVENT_BLOB, EntryKind::Blob, blob_oid);
        tree.upsert(STATE_DIGEST_BLOB, EntryKind::Blob, digest_blob_oid);
        let tree_oid = store.repo.write_tree(&tree).unwrap();
        store
            .repo
            .write_commit_at(tree_oid, parents, "forged", 1)
            .unwrap()
    }

    fn rewrite_commit_digest(
        store: &InvocationStore,
        commit: gix::ObjectId,
        digest: &[u8],
        message: &str,
    ) -> gix::ObjectId {
        let event_blob = store.repo.read_tree_blob(commit, EVENT_BLOB).unwrap();
        let event_oid = store.repo.write_blob(&event_blob).unwrap();
        let digest_oid = store.repo.write_blob(digest).unwrap();
        let mut tree = TreeSpec::new();
        tree.upsert(EVENT_BLOB, EntryKind::Blob, event_oid);
        tree.upsert(STATE_DIGEST_BLOB, EntryKind::Blob, digest_oid);
        let tree_oid = store.repo.write_tree(&tree).unwrap();
        let parents = store.repo.commit_parents(commit).unwrap();
        store
            .repo
            .write_commit_at(tree_oid, &parents, message, 0)
            .unwrap()
    }

    fn assert_malformed_digest(err: &InvocationStoreError) {
        let text = err.to_string();
        assert!(
            text.contains("state_digest") && text.contains("32 bytes"),
            "expected malformed-digest corruption, got {text}"
        );
        assert!(
            !text.contains("already recorded") && !text.contains("invalid ledger event"),
            "a later write replayed a suffix against a mutated cache: {text}"
        );
    }

    fn replace_ledger_tip(
        store: &InvocationStore,
        old: gix::ObjectId,
        new: gix::ObjectId,
        update_witness: bool,
    ) {
        let mut edits = vec![RefTxnEdit {
            name: LEDGER_REF.to_string(),
            expect: RefExpectation::MustMatch(old),
            new: Some(new),
        }];
        if update_witness {
            edits.push(RefTxnEdit {
                name: LEDGER_WITNESS_REF.to_string(),
                expect: RefExpectation::MustMatch(old),
                new: Some(new),
            });
        }
        store.repo.ref_transaction(&edits).unwrap();
    }

    #[test]
    fn create_then_open_roundtrip() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        assert!(layout.project_store(&key, "invocations.git").is_dir());
        assert!(!layout.project_store(&key, "invocations").exists());
        let (_, snap) = store.snapshot().unwrap();
        assert!(snap.invocations.is_empty());
        drop(store);

        // Reopen.
        let store2 = InvocationStore::open(&layout, &key).unwrap();
        let (_, snap2) = store2.snapshot().unwrap();
        assert!(snap2.invocations.is_empty());
    }

    #[test]
    fn durable_snapshot_rejects_unknown_fields() {
        let mut value = serde_json::to_value(LedgerSnapshot::default()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future_field".to_string(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<LedgerSnapshot>(value).is_err());

        let binding = binding(0);
        let mut record = serde_json::to_value(InvocationRecord {
            owner: plan_key(0),
            binding: binding.identity,
            state: InvocationState::Reserved,
        })
        .unwrap();
        record
            .as_object_mut()
            .unwrap()
            .insert("future_field".to_string(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<InvocationRecord>(record).is_err());
    }

    #[test]
    fn pinned_view_reopens_the_same_reachable_commit() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        store.reset_metrics();

        let view = store.pinned_view().unwrap();
        let reopened = store.pinned_view_at(view.commit_oid).unwrap();
        assert_eq!(view, reopened);
        assert_eq!(view.snapshot.plans[0].bindings, vec![binding(0)]);
        assert_eq!(store.metrics_snapshot().repository_lock_count, 0);
    }

    #[test]
    fn pinned_view_rejects_unreachable_cursor() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let orphan = write_forged_commit(&store, &LedgerSnapshot::default(), &[]);

        let err = store.pinned_view_at(orphan).unwrap_err();
        assert!(matches!(
            err,
            InvocationStoreError::UnreachableCommit { commit_oid } if commit_oid == orphan
        ));
    }

    #[test]
    fn pinned_view_rejects_merge_tip() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let (tip, snapshot) = store.snapshot().unwrap();
        let genesis = store.repo.commit_parents(tip).unwrap()[0];
        let forged = write_forged_commit(&store, &snapshot, &[tip, genesis]);
        replace_ledger_tip(&store, tip, forged, true);

        assert!(matches!(
            store.pinned_view().unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
    }

    #[test]
    fn pinned_view_and_writes_reject_an_ancestor_ref_rewind() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let (tip, _) = store.snapshot().unwrap();
        let parent = store.repo.commit_parents(tip).unwrap()[0];
        replace_ledger_tip(&store, tip, parent, false);

        assert!(matches!(
            store.pinned_view().unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
        assert!(matches!(
            store.reserve_batch(plan_key(1), &[binding(1)]).unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
    }

    #[test]
    fn lock_free_reads_do_not_treat_ordered_publication_as_corruption() {
        let (_tmp, layout, key) = test_env();
        let store = Arc::new(InvocationStore::create(&layout, &key).unwrap());
        let done = Arc::new(AtomicBool::new(false));
        let reader_store = Arc::clone(&store);
        let reader_done = Arc::clone(&done);
        let reader = std::thread::spawn(move || {
            let mut successful_reads = 0_u64;
            while !reader_done.load(Ordering::Acquire) {
                match reader_store.pinned_view() {
                    Ok(_) => successful_reads += 1,
                    Err(InvocationStoreError::ConcurrentRead) => {}
                    Err(error) => panic!("lock-free read failed during publication: {error}"),
                }
            }
            successful_reads
        });

        for index in 0..64_u32 {
            let mut reservation = binding(index);
            reservation.identity.model_step_ordinal = index;
            reservation.identity.part_ordinal = 0;
            store
                .reserve_batch(plan_key(index), &[reservation])
                .unwrap();
        }
        done.store(true, Ordering::Release);
        assert!(reader.join().unwrap() > 0);
    }

    #[test]
    fn write_rejects_a_forged_illegal_ancestor_transition() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let tip = store.snapshot().unwrap().0;
        let event = LedgerEvent::Transition {
            invocation_id: binding(0).invocation_id,
            state: InvocationState::Terminal {
                outcome: TerminalOutcome::Succeeded,
                material: TerminalMaterial::Available {
                    digest: [0xaa; 32],
                    byte_length: 1,
                },
                error: None,
                evidence: Box::new(terminal_evidence()),
            },
        };
        let forged = write_forged_commit(&store, &event, &[tip]);
        replace_ledger_tip(&store, tip, forged, true);

        assert!(matches!(
            store.reserve_batch(plan_key(1), &[binding(1)]).unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
    }

    #[test]
    fn pinned_view_rejects_not_executed_with_the_wrong_material_reason() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let (tip, mut snapshot) = store.snapshot().unwrap();
        let outcome = TerminalOutcome::NotExecuted {
            reason: NotExecutedReason::BeforeDispatch,
        };
        snapshot
            .invocations
            .get_mut(&binding(0).invocation_id)
            .unwrap()
            .state = InvocationState::Terminal {
            error: outcome.error_classification(),
            outcome,
            material: TerminalMaterial::Unavailable {
                reason: UnavailableMaterialReason::DraftPublicationFailed,
                digest: None,
                byte_length: None,
            },
            evidence: Box::new(terminal_evidence()),
        };
        let forged = write_forged_commit(&store, &snapshot, &[tip]);
        replace_ledger_tip(&store, tip, forged, true);

        assert!(matches!(
            store.pinned_view().unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
    }

    #[test]
    fn pinned_view_rejects_executed_outcome_with_not_executed_material() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let (tip, mut snapshot) = store.snapshot().unwrap();
        snapshot
            .invocations
            .get_mut(&binding(0).invocation_id)
            .unwrap()
            .state = InvocationState::Terminal {
            outcome: TerminalOutcome::Succeeded,
            material: TerminalMaterial::Unavailable {
                reason: UnavailableMaterialReason::NotExecuted,
                digest: None,
                byte_length: None,
            },
            error: None,
            evidence: Box::new(terminal_evidence()),
        };
        let forged = write_forged_commit(&store, &snapshot, &[tip]);
        replace_ledger_tip(&store, tip, forged, true);

        assert!(matches!(
            store.pinned_view().unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
    }

    #[test]
    fn pinned_view_rejects_truncated_non_genesis_root() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let (tip, snapshot) = store.snapshot().unwrap();
        let forged = write_forged_commit(&store, &snapshot, &[]);
        replace_ledger_tip(&store, tip, forged, true);

        assert!(matches!(
            store.pinned_view().unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
    }

    #[test]
    fn pinned_view_rejects_deleted_authoritative_ref() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let (tip, _) = store.snapshot().unwrap();
        store
            .repo
            .delete_ref(LEDGER_REF, RefExpectation::MustMatch(tip))
            .unwrap();

        assert!(matches!(
            store.pinned_view().unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
    }

    #[test]
    fn reserve_batch_creates_entries() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        let bindings = vec![binding(0), binding(1)];
        store.reserve_batch(plan_key(0), &bindings).unwrap();

        let (_, snap) = store.snapshot().unwrap();
        assert_eq!(snap.invocations.len(), 2);
        for b in &bindings {
            let record = &snap.invocations[&b.invocation_id];
            assert_eq!(record.owner, plan_key(0));
            assert!(matches!(record.state, InvocationState::Reserved));
        }
    }

    #[test]
    fn ledger_commits_store_bounded_events_not_growing_snapshots() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let first_tip = store.ledger_tip().unwrap();
        let first_len = store
            .repo
            .read_tree_blob(first_tip, EVENT_BLOB)
            .unwrap()
            .len();

        for ordinal in 1..100 {
            let mut next = binding(ordinal);
            next.identity.model_step_ordinal = ordinal;
            next.identity.part_ordinal = 0;
            store.reserve_batch(plan_key(ordinal), &[next]).unwrap();
        }

        let final_tip = store.ledger_tip().unwrap();
        let final_blob = store.repo.read_tree_blob(final_tip, EVENT_BLOB).unwrap();
        assert!(final_blob.len() <= first_len + 16);
        assert!(matches!(
            serde_json::from_slice::<LedgerEvent>(&final_blob).unwrap(),
            LedgerEvent::Reserve { .. }
        ));
        assert_eq!(store.snapshot().unwrap().1.invocations.len(), 100);
    }

    #[test]
    fn reserve_batch_rejects_mixed_sessions() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let mut other = binding(1);
        other.identity.session_id = SessionId::parse("bbbbbbbb222244448888dddddddddddd").unwrap();

        assert!(matches!(
            store
                .reserve_batch(plan_key(0), &[binding(0), other])
                .unwrap_err(),
            InvocationStoreError::InvalidReservation { .. }
        ));
    }

    #[test]
    fn reserve_batch_is_idempotent() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        let bindings = vec![binding(0)];
        store.reserve_batch(plan_key(0), &bindings).unwrap();
        store.reserve_batch(plan_key(0), &bindings).unwrap(); // idempotent
        let (_, snap) = store.snapshot().unwrap();
        assert_eq!(snap.invocations.len(), 1);
    }

    #[test]
    fn duplicate_id_in_another_plan_fails() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();

        // Same ID, different plan. The binding is rebased onto the second plan
        // key so it is shape-legal and the duplicate ID is the only fault
        // left; otherwise the address check rejects it first and this proves
        // nothing about duplicate detection.
        let mut foreign = binding(0);
        foreign.identity.model_step_ordinal = plan_key(1).model_step_ordinal;
        let err = store.reserve_batch(plan_key(1), &[foreign]).unwrap_err();
        assert!(
            matches!(err, InvocationStoreError::DuplicateInvocation { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn different_bindings_same_plan_fails() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();

        // Same plan, different binding.
        let err = store.reserve_batch(plan_key(0), &[binding(1)]).unwrap_err();
        assert!(
            matches!(err, InvocationStoreError::DifferentPlan { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn same_ids_in_different_order_is_not_idempotent() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let first = vec![binding(0), binding(1)];
        store.reserve_batch(plan_key(0), &first).unwrap();

        // Same ID set and the same ascending part ordinals, but the IDs are
        // paired with the other addresses. That is shape-legal, so it reaches
        // the durable-plan comparison instead of being rejected earlier, which
        // is what makes it a real test of order-sensitive idempotency.
        let mut reordered = first.clone();
        reordered[0].invocation_id = first[1].invocation_id;
        reordered[1].invocation_id = first[0].invocation_id;
        let err = store.reserve_batch(plan_key(0), &reordered).unwrap_err();
        assert!(
            matches!(err, InvocationStoreError::DifferentPlan { .. }),
            "got {err:?}"
        );
    }

    /// A literally reversed batch is malformed regardless of ledger state, so
    /// it is now rejected by shape validation before the ledger is read.
    #[test]
    fn a_descending_part_order_is_rejected_as_shape() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let first = vec![binding(0), binding(1)];
        store.reserve_batch(plan_key(0), &first).unwrap();

        let reversed = vec![first[1].clone(), first[0].clone()];
        let err = store.reserve_batch(plan_key(0), &reversed).unwrap_err();
        assert!(
            matches!(err, InvocationStoreError::Corruption { .. }),
            "got {err:?}"
        );
    }

    /// The shape half of admission must run before the repository write lock.
    ///
    /// The strict lock is not re-entrant, so if shape validation were still
    /// inside the critical section this call would block until the held guard
    /// is dropped and the receive would time out.
    #[test]
    fn shape_rejection_does_not_wait_for_the_repository_write_lock() {
        let (_tmp, layout, key) = test_env();
        let store = std::sync::Arc::new(InvocationStore::create(&layout, &key).unwrap());
        let held = store.repo.write_lock().unwrap();

        let (tx, rx) = std::sync::mpsc::channel();
        let writer = std::sync::Arc::clone(&store);
        let handle = std::thread::spawn(move || {
            let _ = tx.send(writer.reserve_batch(plan_key(0), &[]));
        });
        let result = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("shape rejection must not wait for the repository write lock");
        assert!(
            matches!(result, Err(InvocationStoreError::Corruption { .. })),
            "got {result:?}"
        );

        drop(held);
        handle.join().unwrap();
    }

    #[test]
    fn full_lifecycle_reserve_dispatch_terminal() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        let b = binding(0);
        store
            .reserve_batch(plan_key(0), std::slice::from_ref(&b))
            .unwrap();

        store.dispatch_started(b.invocation_id).unwrap();
        let (_, snap) = store.snapshot().unwrap();
        assert!(matches!(
            snap.invocations[&b.invocation_id].state,
            InvocationState::DispatchStarted
        ));

        store
            .publish_terminal(
                b.invocation_id,
                TerminalOutcome::Succeeded,
                TerminalMaterial::Unavailable {
                    reason: UnavailableMaterialReason::DraftPublicationFailed,
                    digest: None,
                    byte_length: None,
                },
                terminal_evidence(),
            )
            .unwrap();
        let (_, snap) = store.snapshot().unwrap();
        assert!(matches!(
            snap.invocations[&b.invocation_id].state,
            InvocationState::Terminal { .. }
        ));
    }

    #[test]
    fn tail_is_transition_ordered_and_cursor_exclusive() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let genesis = store.ledger_tip().unwrap();
        let mut bindings = vec![binding(1), binding(0)];
        bindings[0].identity.part_ordinal = 0;
        bindings[1].identity.part_ordinal = 1;
        store.reserve_batch(plan_key(0), &bindings).unwrap();
        let reserved = store.ledger_tip().unwrap();
        store.dispatch_started(bindings[0].invocation_id).unwrap();
        store
            .publish_terminal(
                bindings[0].invocation_id,
                TerminalOutcome::Succeeded,
                TerminalMaterial::Unavailable {
                    reason: UnavailableMaterialReason::DraftPublicationFailed,
                    digest: None,
                    byte_length: None,
                },
                terminal_evidence(),
            )
            .unwrap();

        let all = store.tail(Some(genesis)).unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].invocation_id, bindings[0].invocation_id);
        assert_eq!(all[1].invocation_id, bindings[1].invocation_id);
        assert!(matches!(all[0].record.state, InvocationState::Reserved));
        assert!(matches!(all[1].record.state, InvocationState::Reserved));
        assert!(matches!(
            all[2].record.state,
            InvocationState::DispatchStarted
        ));
        assert!(matches!(
            all[3].record.state,
            InvocationState::Terminal { .. }
        ));

        let after_reservation = store.tail(Some(reserved)).unwrap();
        assert_eq!(after_reservation, all[2..]);
        assert!(
            store
                .tail(Some(store.ledger_tip().unwrap()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dispatch_started_is_idempotent() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        store.dispatch_started(binding(0).invocation_id).unwrap();
        store.dispatch_started(binding(0).invocation_id).unwrap(); // idempotent
    }

    #[test]
    fn idempotent_retry_repeats_post_publication_barrier() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let invocation = binding(0).invocation_id;
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();

        lattice_store::fault::fail_ref_fsync(true);
        let err = store.dispatch_started(invocation).unwrap_err();
        assert!(matches!(
            err,
            InvocationStoreError::Store(StoreError::Durability { .. })
        ));
        assert_ne!(
            store.repo.ref_target(LEDGER_REF).unwrap(),
            store.repo.ref_target(LEDGER_WITNESS_REF).unwrap(),
            "ledger publication precedes the witness"
        );
        let (_, published) = store.snapshot().unwrap();
        assert!(matches!(
            published.invocations[&invocation].state,
            InvocationState::DispatchStarted
        ));

        lattice_store::fault::fail_ref_fsync(true);
        let retry_err = store.dispatch_started(invocation).unwrap_err();
        assert!(matches!(
            retry_err,
            InvocationStoreError::Store(StoreError::Durability { .. })
        ));
        assert_eq!(
            store.repo.ref_target(LEDGER_REF).unwrap(),
            store.repo.ref_target(LEDGER_WITNESS_REF).unwrap(),
            "retry repaired the lagging witness before reporting its barrier failure"
        );
        store.dispatch_started(invocation).unwrap();
    }

    /// Post-CAS fault matrix, `reserve_batch` arm: the reservation commit and
    /// ref edit are published, only the durability barrier fails. The exact
    /// retry repeats the barrier; a *different* batch for the same PlanKey
    /// fails closed against the already published reservation.
    #[test]
    fn reserve_batch_published_but_undurable_retries_exactly_and_rejects_a_different_batch() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let bindings = [binding(0), binding(1)];

        lattice_store::fault::fail_ref_fsync(true);
        let err = store.reserve_batch(plan_key(0), &bindings).unwrap_err();
        assert!(matches!(
            err,
            InvocationStoreError::Store(StoreError::Durability { .. })
        ));
        assert_ne!(
            store.repo.ref_target(LEDGER_REF).unwrap(),
            store.repo.ref_target(LEDGER_WITNESS_REF).unwrap(),
            "the ledger publication precedes the witness"
        );
        let (_, published) = store.snapshot().unwrap();
        assert_eq!(published.plans.len(), 1, "the batch is already durable");

        // A different batch for the same PlanKey is not an idempotent retry.
        assert!(matches!(
            store.reserve_batch(plan_key(0), &[binding(0)]),
            Err(InvocationStoreError::DifferentPlan { .. })
        ));

        // The exact retry repairs the witness and repeats the barrier.
        lattice_store::fault::fail_ref_fsync(true);
        assert!(matches!(
            store.reserve_batch(plan_key(0), &bindings),
            Err(InvocationStoreError::Store(StoreError::Durability { .. }))
        ));
        assert_eq!(
            store.repo.ref_target(LEDGER_REF).unwrap(),
            store.repo.ref_target(LEDGER_WITNESS_REF).unwrap(),
            "the retry repaired the lagging witness before reporting the failure"
        );
        store.reserve_batch(plan_key(0), &bindings).unwrap();
    }

    /// Post-CAS fault matrix, terminal arm: the terminal + evidence commit is
    /// published, only the barrier fails. The exact retry repeats the barrier;
    /// a different terminal for the same invocation fails closed.
    #[test]
    fn terminal_published_but_undurable_retries_exactly_and_rejects_a_different_terminal() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let b = binding(0);
        store
            .reserve_batch(plan_key(0), std::slice::from_ref(&b))
            .unwrap();
        store.dispatch_started(b.invocation_id).unwrap();
        let material = TerminalMaterial::Unavailable {
            reason: UnavailableMaterialReason::DraftPublicationFailed,
            digest: None,
            byte_length: None,
        };

        lattice_store::fault::fail_ref_fsync(true);
        let err = store
            .publish_terminal(
                b.invocation_id,
                TerminalOutcome::Succeeded,
                material.clone(),
                terminal_evidence(),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            InvocationStoreError::Store(StoreError::Durability { .. })
        ));
        let (_, published) = store.snapshot().unwrap();
        assert!(matches!(
            published.invocations[&b.invocation_id].state,
            InvocationState::Terminal { .. }
        ));

        // A different terminal is never an idempotent retry.
        assert!(matches!(
            store.publish_terminal(
                b.invocation_id,
                TerminalOutcome::Failed,
                material.clone(),
                terminal_evidence(),
            ),
            Err(InvocationStoreError::InvalidTransition { .. })
        ));

        lattice_store::fault::fail_ref_fsync(true);
        assert!(matches!(
            store.publish_terminal(
                b.invocation_id,
                TerminalOutcome::Succeeded,
                material.clone(),
                terminal_evidence(),
            ),
            Err(InvocationStoreError::Store(StoreError::Durability { .. }))
        ));
        assert_eq!(
            store.repo.ref_target(LEDGER_REF).unwrap(),
            store.repo.ref_target(LEDGER_WITNESS_REF).unwrap(),
            "the retry repaired the lagging witness before reporting the failure"
        );
        store
            .publish_terminal(
                b.invocation_id,
                TerminalOutcome::Succeeded,
                material,
                terminal_evidence(),
            )
            .unwrap();
    }

    /// Post-CAS fault matrix, `DispatchStarted` mismatch arm: a published but
    /// undurable dispatch never permits a *different* next transition to be
    /// treated as its retry.
    #[test]
    fn dispatch_started_published_but_undurable_rejects_a_foreign_invocation_retry() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let bindings = [binding(0), binding(1)];
        store.reserve_batch(plan_key(0), &bindings).unwrap();

        lattice_store::fault::fail_ref_fsync(true);
        assert!(matches!(
            store.dispatch_started(bindings[0].invocation_id),
            Err(InvocationStoreError::Store(StoreError::Durability { .. }))
        ));
        // An unknown invocation is not a retry of the pending barrier.
        let unknown = InvocationId::parse("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap();
        assert!(matches!(
            store.dispatch_started(unknown),
            Err(InvocationStoreError::NotFound { .. })
        ));
        store.dispatch_started(bindings[0].invocation_id).unwrap();
        let (_, published) = store.snapshot().unwrap();
        assert!(matches!(
            published.invocations[&bindings[1].invocation_id].state,
            InvocationState::Reserved,
        ));
    }

    #[test]
    fn terminal_is_idempotent() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        let b = binding(0);
        store
            .reserve_batch(plan_key(0), std::slice::from_ref(&b))
            .unwrap();
        store.dispatch_started(b.invocation_id).unwrap();
        let term = TerminalOutcome::Succeeded;
        let mat = TerminalMaterial::Unavailable {
            reason: UnavailableMaterialReason::DraftPublicationFailed,
            digest: None,
            byte_length: None,
        };
        let evidence = terminal_evidence();
        store
            .publish_terminal(b.invocation_id, term.clone(), mat.clone(), evidence.clone())
            .unwrap();
        store
            .publish_terminal(b.invocation_id, term, mat, evidence)
            .unwrap(); // idempotent
    }

    #[test]
    fn query_invocations_returns_matching_records() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        store
            .reserve_batch(plan_key(0), &[binding(0), binding(1)])
            .unwrap();

        let unknown = InvocationId::parse("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap();
        let result = store
            .query_invocations(&[binding(0).invocation_id, unknown])
            .unwrap();
        assert_eq!(result.len(), 1);
        assert!(result.contains_key(&binding(0).invocation_id));
    }

    #[test]
    fn not_executed_terminal_from_reserved() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        let b = binding(0);
        store
            .reserve_batch(plan_key(0), std::slice::from_ref(&b))
            .unwrap();
        // Skip DispatchStarted — NotExecuted paths go Reserved → Terminal.
        store
            .publish_terminal(
                b.invocation_id,
                TerminalOutcome::NotExecuted {
                    reason: NotExecutedReason::BeforeDispatch,
                },
                TerminalMaterial::Unavailable {
                    reason: UnavailableMaterialReason::NotExecuted,
                    digest: None,
                    byte_length: None,
                },
                terminal_evidence(),
            )
            .unwrap();
        let (_, snap) = store.snapshot().unwrap();
        let InvocationState::Terminal {
            outcome,
            error,
            material,
            evidence,
        } = &snap.invocations[&b.invocation_id].state
        else {
            panic!("expected terminal state");
        };
        assert_eq!(
            outcome,
            &TerminalOutcome::NotExecuted {
                reason: NotExecutedReason::BeforeDispatch,
            }
        );
        assert_eq!(
            error,
            &Some(TerminalErrorClassification::NotExecuted(
                NotExecutedReason::BeforeDispatch
            ))
        );
        assert!(matches!(
            material,
            TerminalMaterial::Unavailable {
                reason: UnavailableMaterialReason::NotExecuted,
                ..
            }
        ));
        assert_eq!(
            evidence.admission.version,
            lancet_protocol::ADMISSION_EVIDENCE_VERSION
        );
    }

    #[test]
    fn terminal_publish_rejects_outcome_material_type_confusion() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let id = binding(0).invocation_id;
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        store.dispatch_started(id).unwrap();

        assert!(matches!(
            store
                .publish_terminal(
                    id,
                    TerminalOutcome::Succeeded,
                    TerminalMaterial::Unavailable {
                        reason: UnavailableMaterialReason::NotExecuted,
                        digest: None,
                        byte_length: None,
                    },
                    terminal_evidence(),
                )
                .unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
        assert!(matches!(
            store
                .publish_terminal(
                    id,
                    TerminalOutcome::Indeterminate {
                        reason: IndeterminateReason::MayHaveOccurred,
                    },
                    TerminalMaterial::Available {
                        digest: [7; 32],
                        byte_length: 1,
                    },
                    terminal_evidence(),
                )
                .unwrap_err(),
            InvocationStoreError::Corruption { .. }
        ));
    }

    #[test]
    fn ledger_ancestry_is_linear() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        store.dispatch_started(binding(0).invocation_id).unwrap();
        store
            .publish_terminal(
                binding(0).invocation_id,
                TerminalOutcome::Succeeded,
                TerminalMaterial::Unavailable {
                    reason: UnavailableMaterialReason::DraftPublicationFailed,
                    digest: None,
                    byte_length: None,
                },
                terminal_evidence(),
            )
            .unwrap();

        // Walk ancestry: tip should have 3 transitions + 1 genesis = 4 commits.
        let (tip, _) = store.snapshot().unwrap();
        let commits = store.repo.rev_walk(&[tip], &[]).unwrap();
        assert_eq!(commits.len(), 4, "genesis + 3 transitions");

        // Each commit has exactly one parent except genesis.
        for (i, oid) in commits.iter().enumerate() {
            let parents = store.repo.commit_parents(*oid).unwrap();
            if i < commits.len() - 1 {
                assert_eq!(parents.len(), 1);
            } else {
                assert_eq!(parents.len(), 0, "genesis has no parent");
            }
        }
    }
    // -----------------------------------------------------------------------
    // Derived checkpoint / cache tests (L-GH139A-02)
    // -----------------------------------------------------------------------

    /// Read the checkpoint file and assert it is a genuinely authenticated
    /// anchor: its commit is reachable from `tip` and its tree carries a
    /// `state_digest` equal to the digest of the snapshot it stores.
    ///
    /// This is stricter than the old `ledger_tip == tip` assertion, which only
    /// held because every commit used to carry a digest.
    fn assert_authenticated_checkpoint(
        store: &InvocationStore,
        tip: gix::ObjectId,
    ) -> DerivedCheckpoint {
        let bytes = std::fs::read(store.checkpoint_path()).expect("checkpoint file");
        let cp: DerivedCheckpoint = serde_json::from_slice(&bytes).expect("checkpoint parses");
        assert_eq!(cp.schema_version, DERIVED_CHECKPOINT_SCHEMA_VERSION);
        assert!(
            cp.ledger_tip == tip || store.is_first_parent_ancestor(cp.ledger_tip, tip).unwrap(),
            "checkpoint anchor {} is not reachable from tip {tip}",
            cp.ledger_tip
        );
        let anchored = store
            .try_read_state_digest_at(cp.ledger_tip)
            .expect("checkpoint must name an anchor commit carrying a state_digest");
        assert_eq!(
            anchored,
            compute_state_digest(&cp.snapshot).unwrap(),
            "checkpoint snapshot is not the one anchored in its commit tree"
        );
        cp
    }

    #[test]
    fn cold_reopen_uses_derived_checkpoint_and_rebuilds_on_deletion() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        // Enough commits (3 per invocation) to cross
        // CHECKPOINT_MIN_INTERVAL and force a real anchor.
        const INVOCATIONS: usize = 15;
        for i in 0..INVOCATIONS as u32 {
            let mut b = binding(i);
            b.identity.model_step_ordinal = i;
            store.reserve_batch(plan_key(i), &[b.clone()]).unwrap();
            store.dispatch_started(b.invocation_id).unwrap();
            store
                .publish_terminal(
                    b.invocation_id,
                    TerminalOutcome::Succeeded,
                    TerminalMaterial::Unavailable {
                        reason: UnavailableMaterialReason::DraftPublicationFailed,
                        digest: None,
                        byte_length: None,
                    },
                    terminal_evidence(),
                )
                .unwrap();
        }

        let (tip_a, snap_a) = store.snapshot().unwrap();
        assert_eq!(snap_a.plans.len(), INVOCATIONS);
        assert_eq!(snap_a.invocations.len(), INVOCATIONS);

        // The writer published an anchored checkpoint on its own: this ledger
        // is past CHECKPOINT_MIN_INTERVAL commits, so an anchor exists beyond
        // genesis and it is cryptographically bound to its commit tree.
        let checkpoint_path = store.checkpoint_path();
        let cp = assert_authenticated_checkpoint(&store, tip_a);
        assert_ne!(
            cp.ledger_tip, tip_a,
            "an anchor is periodic, so it lags the tip rather than tracking it"
        );

        // Cold reopen: fresh store handle without in-memory cache.
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (tip_b, snap_b) = fresh.snapshot().unwrap();
        assert_eq!(tip_b, tip_a);
        assert_eq!(snap_b, snap_a);

        // Delete the checkpoint file: cold reopen must reconstruct from genesis.
        std::fs::remove_file(&checkpoint_path).unwrap();
        let fresh2 = InvocationStore::open(&layout, &key).unwrap();
        let (tip_c, snap_c) = fresh2.snapshot().unwrap();
        assert_eq!(tip_c, tip_a);
        assert_eq!(snap_c, snap_a);

        // Deletion self-heals at the newest anchor found during the rebuild,
        // and the healed file is authenticated exactly like the original.
        let healed = assert_authenticated_checkpoint(&fresh2, tip_a);
        assert_eq!(healed.ledger_tip, cp.ledger_tip);
        assert_eq!(healed.snapshot, cp.snapshot);
    }

    #[test]
    fn corrupted_checkpoint_is_safely_discarded_and_rebuilt() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let (tip, snap) = store.snapshot().unwrap();

        // Write invalid JSON into the checkpoint file.
        let checkpoint_path = store.checkpoint_path();
        std::fs::write(&checkpoint_path, b"not valid json").unwrap();

        // Cold open must discard corrupt checkpoint and rebuild from genesis.
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (fresh_tip, fresh_snap) = fresh.snapshot().unwrap();
        assert_eq!(fresh_tip, tip);
        assert_eq!(fresh_snap, snap);

        // Corrupt file was replaced by an authenticated anchored checkpoint.
        assert_authenticated_checkpoint(&fresh, tip);
    }

    #[test]
    fn mismatched_checkpoint_tip_is_discarded() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let (tip, snap) = store.snapshot().unwrap();

        // Write a checkpoint pointing to a non-existent / unreachable OID.
        let foreign_oid =
            gix::ObjectId::from_hex(b"1111111111111111111111111111111111111111").unwrap();
        let forged = DerivedCheckpoint {
            schema_version: DERIVED_CHECKPOINT_SCHEMA_VERSION,
            ledger_tip: foreign_oid,
            snapshot: LedgerSnapshot::default(),
        };
        let bytes = lattice_store::canon::canonical_json_bytes(&forged).unwrap();
        std::fs::write(store.checkpoint_path(), &bytes).unwrap();

        // Cold open discards unreachable checkpoint and produces the true state.
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (fresh_tip, fresh_snap) = fresh.snapshot().unwrap();
        assert_eq!(fresh_tip, tip);
        assert_eq!(fresh_snap, snap);
    }

    /// P1-2 regression: an ordinary transition must not write snapshot-sized
    /// work into the repository write lock, and anchored checkpoints must not
    /// accumulate O(N^2) bytes.
    ///
    /// Both properties are asserted structurally rather than by timing:
    ///
    /// 1. every non-anchor commit tree holds exactly one entry, `event.json`,
    ///    so publication is bounded by the event and not by accumulated state;
    /// 2. anchors are sparse and *thin out* as the ledger grows, because the
    ///    interval scales with state size. A fixed interval would place them
    ///    at a constant rate, which is what makes cumulative checkpoint bytes
    ///    quadratic.
    #[test]
    fn ordinary_transitions_write_no_snapshot_sized_object_and_anchors_thin_out() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        const INVOCATIONS: u32 = 600;
        for i in 0..INVOCATIONS {
            let mut b = binding(i);
            b.identity.model_step_ordinal = i;
            store.reserve_batch(plan_key(i), &[b.clone()]).unwrap();
            store.dispatch_started(b.invocation_id).unwrap();
        }

        let (tip, snapshot) = store.snapshot().unwrap();
        assert_eq!(snapshot.invocations.len(), INVOCATIONS as usize);

        // Walk the whole ledger and classify every commit.
        let mut anchor_positions = Vec::new();
        let mut commits = Vec::new();
        let mut cursor = tip;
        loop {
            commits.push(cursor);
            let parents = store.repo.commit_parents(cursor).unwrap();
            match parents.first() {
                Some(parent) => cursor = *parent,
                None => break,
            }
        }
        commits.reverse();

        for (index, commit) in commits.iter().enumerate() {
            let tree_oid = store.repo.commit_tree(*commit).unwrap();
            let files = store.repo.tree_files(tree_oid).unwrap();
            let is_anchor = files.contains_key(STATE_DIGEST_BLOB);
            if is_anchor {
                anchor_positions.push(index);
            } else {
                // (1) An ordinary commit carries the event and nothing else.
                assert_eq!(
                    files.keys().collect::<Vec<_>>(),
                    vec![EVENT_BLOB],
                    "ordinary commit {commit} wrote more than its event"
                );
            }
        }

        let total = commits.len();
        assert_eq!(
            total,
            1 + 2 * INVOCATIONS as usize,
            "genesis + 2 per invocation"
        );

        // (2) Anchors are sparse, and their gaps grow with state size.
        assert!(
            anchor_positions.len() >= 2,
            "expected at least genesis plus one periodic anchor, got {anchor_positions:?}"
        );
        let fixed_interval_anchors = total / CHECKPOINT_MIN_INTERVAL as usize;
        assert!(
            anchor_positions.len() < fixed_interval_anchors,
            "state-scaled anchoring must place fewer anchors ({}) than a fixed \
             {CHECKPOINT_MIN_INTERVAL}-commit interval would ({fixed_interval_anchors})",
            anchor_positions.len()
        );

        let gaps: Vec<usize> = anchor_positions
            .windows(2)
            .map(|pair| pair[1] - pair[0])
            .collect();
        assert!(
            gaps.windows(2).all(|pair| pair[1] >= pair[0]),
            "anchor gaps must never shrink as state grows: {gaps:?}"
        );
        assert!(
            *gaps.last().unwrap() > CHECKPOINT_MIN_INTERVAL as usize,
            "the final gap must exceed the floor once state is large: {gaps:?}"
        );

        // The sparse anchors still reconstruct the exact state on a cold read.
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (fresh_tip, fresh_snapshot) = fresh.snapshot().unwrap();
        assert_eq!(fresh_tip, tip);
        assert_eq!(fresh_snapshot, snapshot);
        assert_authenticated_checkpoint(&fresh, tip);
    }

    /// Restart regression: anchor cadence must survive a process restart.
    ///
    /// A writer that reopens the store before every write is the normal shape
    /// of this store's real workload, and it must still anchor. Counting
    /// commits only within one handle's lifetime means a restarting writer
    /// never reaches the interval, so no anchor is ever published beyond
    /// genesis and every later cold read replays the entire ledger again.
    #[test]
    fn anchor_cadence_survives_a_fresh_open_before_every_write() {
        let (_tmp, layout, key) = test_env();
        InvocationStore::create(&layout, &key).unwrap();

        const WRITES: u32 = 80;
        for i in 0..WRITES {
            let store = InvocationStore::open(&layout, &key).unwrap();
            let mut b = binding(i);
            b.identity.model_step_ordinal = i;
            store.reserve_batch(plan_key(i), &[b]).unwrap();
        }

        let reader = InvocationStore::open(&layout, &key).unwrap();
        let (tip, snapshot) = reader.snapshot().unwrap();
        assert_eq!(snapshot.invocations.len(), WRITES as usize);

        // Walk the ledger and locate every anchor.
        let mut commits = Vec::new();
        let mut cursor = tip;
        loop {
            commits.push(cursor);
            let parents = reader.repo.commit_parents(cursor).unwrap();
            match parents.first() {
                Some(parent) => cursor = *parent,
                None => break,
            }
        }
        commits.reverse();
        assert_eq!(
            commits.len(),
            1 + WRITES as usize,
            "genesis + one per write"
        );

        let anchors: Vec<usize> = commits
            .iter()
            .enumerate()
            .filter(|(_, commit)| reader.try_read_state_digest_at(**commit).is_some())
            .map(|(index, _)| index)
            .collect();

        assert!(
            anchors.len() >= 2,
            "a restarting writer must still anchor: {WRITES} writes produced only \
             {} anchor(s) {anchors:?}",
            anchors.len()
        );

        // The published checkpoint must have advanced past genesis, so a cold
        // reader replays a bounded delta rather than the whole ledger.
        let cp = assert_authenticated_checkpoint(&reader, tip);
        assert!(
            !cp.snapshot.plans.is_empty(),
            "the checkpoint must carry real state, not the genesis snapshot"
        );
        let delta = commits.len() - 1 - *anchors.last().unwrap();
        assert!(
            delta <= checkpoint_interval(snapshot.invocations.len()) as usize,
            "tip is {delta} commits past the newest anchor, which exceeds one interval"
        );
    }

    /// A reader that catches up with another writer must carry the ledger's
    /// anchor distance into a subsequent write.  Otherwise the reader's
    /// in-memory cache still starts at distance zero after applying the peer's
    /// suffix, and a later write silently loses the cadence derived from the
    /// durable ledger.
    #[test]
    fn reader_catch_up_preserves_anchor_cadence_for_a_later_write() {
        let (_tmp, layout, key) = test_env();
        let store_a = InvocationStore::create(&layout, &key).unwrap();

        // Populate A's cache at the genesis tip.
        let (genesis, genesis_snapshot) = store_a.snapshot().unwrap();
        assert!(genesis_snapshot.plans.is_empty());

        // A peer advances the authoritative ledger by twenty ordinary writes.
        for i in 0..20_u32 {
            let store_b = InvocationStore::open(&layout, &key).unwrap();
            let mut binding = binding(i);
            binding.identity.model_step_ordinal = i;
            store_b.reserve_batch(plan_key(i), &[binding]).unwrap();
        }

        // This public read makes A catch up through the peer suffix while it
        // still has the genesis snapshot cached.
        let (peer_tip, peer_snapshot) = store_a.snapshot().unwrap();
        assert_ne!(peer_tip, genesis);
        assert_eq!(peer_snapshot.plans.len(), 20);

        // Twelve writes reach the 32-commit minimum from the newest anchor.
        // The twelfth write must publish a new authenticated checkpoint even
        // though the same handle first became a reader of peer commits.
        for i in 20..32_u32 {
            let mut binding = binding(i);
            binding.identity.model_step_ordinal = i;
            store_a.reserve_batch(plan_key(i), &[binding]).unwrap();
        }

        let (tip, snapshot) = store_a.snapshot().unwrap();
        assert_eq!(snapshot.plans.len(), 32);

        let checkpoint = assert_authenticated_checkpoint(&store_a, tip);
        assert_eq!(checkpoint.snapshot.plans.len(), 32);
        assert_ne!(checkpoint.ledger_tip, genesis);
    }

    /// A right-length but wrong suffix digest must not be mistaken for a new
    /// anchor while a cached reader catches up.  The reader must fall back to
    /// the retained authenticated checkpoint, preserve the real ledger
    /// distance, and publish the next valid anchor at the expected boundary.
    #[test]
    fn corrupt_suffix_anchor_does_not_reset_reader_catch_up_distance() {
        let (_tmp, layout, key) = test_env();
        let store_a = InvocationStore::create(&layout, &key).unwrap();

        // Cross the minimum cadence and retain the valid checkpoint at the
        // first anchor.  A caches that anchor before the peer advances.
        for i in 0..32_u32 {
            let mut binding = binding(i);
            binding.identity.model_step_ordinal = i;
            store_a.reserve_batch(plan_key(i), &[binding]).unwrap();
        }
        let (anchor_tip, anchor_snapshot) = store_a.snapshot().unwrap();
        let retained_checkpoint = std::fs::read(store_a.checkpoint_path()).unwrap();
        assert_eq!(anchor_snapshot.plans.len(), 32);
        let retained = assert_authenticated_checkpoint(&store_a, anchor_tip);
        assert_eq!(retained.ledger_tip, anchor_tip);

        // A peer publishes exactly one more cadence interval, making its tip
        // an anchor.  Keep the original tip so only that final commit is
        // rewritten with a corrupt digest below.
        let store_b = InvocationStore::open(&layout, &key).unwrap();
        for i in 32..64_u32 {
            let mut binding = binding(i);
            binding.identity.model_step_ordinal = i;
            store_b.reserve_batch(plan_key(i), &[binding]).unwrap();
        }
        let (peer_tip, peer_snapshot) = store_b.snapshot().unwrap();
        assert_eq!(peer_snapshot.plans.len(), 64);
        assert_ne!(peer_tip, anchor_tip);
        assert!(store_b.try_read_state_digest_at(peer_tip).is_some());

        // Restore the older valid checkpoint and forge only the suffix anchor
        // with a right-length, incorrect digest.  The authoritative ledger
        // remains a linear history rooted at the retained checkpoint.
        std::fs::write(store_a.checkpoint_path(), retained_checkpoint).unwrap();
        let event_blob = store_a.repo.read_tree_blob(peer_tip, EVENT_BLOB).unwrap();
        let event_oid = store_a.repo.write_blob(&event_blob).unwrap();
        let forged_oid = store_a.repo.write_blob(&[0x5a_u8; 32]).unwrap();
        let mut tree = TreeSpec::new();
        tree.upsert(EVENT_BLOB, EntryKind::Blob, event_oid);
        tree.upsert(STATE_DIGEST_BLOB, EntryKind::Blob, forged_oid);
        let tree_oid = store_a.repo.write_tree(&tree).unwrap();
        let parent = store_a.repo.commit_parents(peer_tip).unwrap()[0];
        let forged_tip = store_a
            .repo
            .write_commit_at(tree_oid, &[parent], "forged suffix anchor", 0)
            .unwrap();
        replace_ledger_tip(&store_a, peer_tip, forged_tip, true);

        // A catches up through the corrupt suffix using the public snapshot
        // operation.  The implementation must reject the forged anchor and
        // rebuild from the retained valid checkpoint instead of resetting the
        // cadence at the corrupt commit.
        let (caught_tip, caught_snapshot) = store_a.snapshot().unwrap();
        assert_eq!(caught_tip, forged_tip);
        assert_eq!(caught_snapshot.plans.len(), 64);

        // One further write is exactly the first commit beyond the 32-commit
        // suffix from the retained anchor, so it must publish a valid anchor.
        let mut next = binding(64);
        next.identity.model_step_ordinal = 64;
        store_a.reserve_batch(plan_key(64), &[next]).unwrap();
        let (tip, snapshot) = store_a.snapshot().unwrap();
        assert_eq!(snapshot.plans.len(), 65);
        let checkpoint = assert_authenticated_checkpoint(&store_a, tip);
        assert_eq!(checkpoint.snapshot.plans.len(), 65);
        assert_ne!(checkpoint.ledger_tip, forged_tip);
    }

    /// A writer that already cached its own tip must fail closed, twice, when a
    /// peer suffix carries a malformed digest.  `write_state` applies that
    /// suffix in place before the digest check; if the error path leaves the
    /// cache advanced, the next write on the same handle replays the suffix
    /// against a snapshot that already contains the peer plan.
    #[test]
    fn writer_catch_up_failure_does_not_pollute_the_cached_snapshot() {
        let (_tmp, layout, key) = test_env();
        let store_a = InvocationStore::create(&layout, &key).unwrap();
        let mut first = binding(0);
        first.identity.model_step_ordinal = 0;
        store_a
            .reserve_batch(plan_key(0), std::slice::from_ref(&first))
            .unwrap();

        let store_b = InvocationStore::open(&layout, &key).unwrap();
        let mut peer = binding(1);
        peer.identity.model_step_ordinal = 1;
        store_b
            .reserve_batch(plan_key(1), std::slice::from_ref(&peer))
            .unwrap();
        let (peer_tip, _) = store_b.snapshot().unwrap();

        let malformed_tip =
            rewrite_commit_digest(&store_a, peer_tip, &[0x5a; 31], "malformed suffix digest");
        replace_ledger_tip(&store_a, peer_tip, malformed_tip, true);

        let mut next = binding(2);
        next.identity.model_step_ordinal = 2;
        let first_err = store_a
            .reserve_batch(plan_key(2), std::slice::from_ref(&next))
            .unwrap_err();
        let second_err = store_a
            .reserve_batch(plan_key(2), std::slice::from_ref(&next))
            .unwrap_err();
        assert_malformed_digest(&first_err);
        assert_malformed_digest(&second_err);
        assert_eq!(
            first_err.to_string(),
            second_err.to_string(),
            "retrying the same writer handle must fail closed the same way"
        );
        assert_eq!(store_a.ledger_tip().unwrap(), malformed_tip);
    }

    /// A genesis `state_digest` that cannot be read as 32 bytes is corruption,
    /// not an ordinary commit.  Treating it as absent would let a cold rebuild
    /// succeed on a ledger whose only anchor is unreadable.
    #[test]
    fn rebuild_fails_closed_on_a_malformed_genesis_digest() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        let (genesis, _) = store.snapshot().unwrap();
        let malformed =
            rewrite_commit_digest(&store, genesis, &[0x5a; 31], "malformed genesis digest");
        replace_ledger_tip(&store, genesis, malformed, true);
        std::fs::remove_file(store.checkpoint_path()).unwrap();
        drop(store);

        let fresh = InvocationStore::open(&layout, &key).unwrap();
        assert_malformed_digest(&fresh.snapshot().unwrap_err());
    }

    /// A periodic anchor with a malformed digest must fail closed during
    /// rebuild.  Swallowing it as "no digest" would skip the authenticating
    /// blob and treat the commit as ordinary.
    #[test]
    fn rebuild_fails_closed_on_a_malformed_periodic_anchor_digest() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        for i in 0..32_u32 {
            let mut reserved = binding(i);
            reserved.identity.model_step_ordinal = i;
            store.reserve_batch(plan_key(i), &[reserved]).unwrap();
        }
        let (tip, snapshot) = store.snapshot().unwrap();
        assert_eq!(snapshot.plans.len(), 32);
        assert!(
            store.try_read_state_digest_at(tip).is_some(),
            "the 32nd write must be a periodic anchor"
        );

        let malformed = rewrite_commit_digest(&store, tip, &[0x5a; 7], "malformed periodic digest");
        replace_ledger_tip(&store, tip, malformed, true);
        std::fs::remove_file(store.checkpoint_path()).unwrap();
        drop(store);

        let fresh = InvocationStore::open(&layout, &key).unwrap();
        assert_malformed_digest(&fresh.snapshot().unwrap_err());
    }

    /// A corrupt anchor digest of the correct length must never be selected as
    /// the rebuild anchor.
    ///
    /// Selecting it would write it into the checkpoint, and every later load
    /// would then reject that checkpoint, so each cold read would rebuild from
    /// genesis forever. The rebuild must instead fall back to an older anchor
    /// that actually verifies.
    #[test]
    fn a_corrupt_anchor_digest_is_not_selected_as_the_rebuild_anchor() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        // Enough writes to publish at least one anchor beyond genesis.
        const WRITES: u32 = 40;
        for i in 0..WRITES {
            let mut b = binding(i);
            b.identity.model_step_ordinal = i;
            store.reserve_batch(plan_key(i), &[b]).unwrap();
        }
        let (tip, snapshot) = store.snapshot().unwrap();

        let mut commits = Vec::new();
        let mut cursor = tip;
        loop {
            commits.push(cursor);
            match store.repo.commit_parents(cursor).unwrap().first() {
                Some(parent) => cursor = *parent,
                None => break,
            }
        }
        commits.reverse();
        let anchors: Vec<gix::ObjectId> = commits
            .iter()
            .copied()
            .filter(|commit| store.try_read_state_digest_at(*commit).is_some())
            .collect();
        assert!(anchors.len() >= 2, "need genesis plus a periodic anchor");
        let newest_anchor = *anchors.last().unwrap();

        // Forge a right-length but wrong digest at the newest anchor by
        // rewriting that commit's tree, then re-pointing the ledger at the
        // rebuilt suffix.
        let forged_digest = [0x5au8; 32];
        let event_blob = store
            .repo
            .read_tree_blob(newest_anchor, EVENT_BLOB)
            .unwrap();
        let event_oid = store.repo.write_blob(&event_blob).unwrap();
        let forged_oid = store.repo.write_blob(&forged_digest).unwrap();
        let mut tree = TreeSpec::new();
        tree.upsert(EVENT_BLOB, EntryKind::Blob, event_oid);
        tree.upsert(STATE_DIGEST_BLOB, EntryKind::Blob, forged_oid);
        let tree_oid = store.repo.write_tree(&tree).unwrap();
        let parent = store.repo.commit_parents(newest_anchor).unwrap()[0];
        let mut rewritten = store
            .repo
            .write_commit_at(tree_oid, &[parent], "forged anchor", 0)
            .unwrap();

        // Rebuild every descendant on top of the forged commit.
        let position = commits.iter().position(|c| *c == newest_anchor).unwrap();
        for original in &commits[position + 1..] {
            let files = store
                .repo
                .tree_files(store.repo.commit_tree(*original).unwrap())
                .unwrap();
            let mut spec = TreeSpec::new();
            for (name, bytes) in &files {
                let oid = store.repo.write_blob(bytes).unwrap();
                spec.upsert(name, EntryKind::Blob, oid);
            }
            let spec_oid = store.repo.write_tree(&spec).unwrap();
            let message = store.repo.commit_message(*original).unwrap();
            rewritten = store
                .repo
                .write_commit_at(spec_oid, &[rewritten], &message, 0)
                .unwrap();
        }
        replace_ledger_tip(&store, tip, rewritten, true);
        std::fs::remove_file(store.checkpoint_path()).unwrap();

        // A cold read must still produce the true state, and the checkpoint it
        // publishes must be one that authenticates — never the forged anchor.
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (fresh_tip, fresh_snapshot) = fresh.snapshot().unwrap();
        assert_eq!(fresh_tip, rewritten);
        assert_eq!(fresh_snapshot.invocations, snapshot.invocations);

        let cp = assert_authenticated_checkpoint(&fresh, rewritten);
        assert_ne!(
            cp.ledger_tip, rewritten,
            "the forged anchor's descendants are not anchors"
        );

        // The published checkpoint must be reloadable, not rejected on every
        // later open. A second cold read reuses it rather than rebuilding.
        let again = InvocationStore::open(&layout, &key).unwrap();
        assert!(
            again.load_derived_checkpoint(rewritten).is_some(),
            "the self-healed checkpoint must authenticate on reload"
        );
    }

    #[test]
    fn forged_empty_checkpoint_for_populated_ledger_is_rejected() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();
        store.reserve_batch(plan_key(0), &[binding(0)]).unwrap();
        let (tip, snap) = store.snapshot().unwrap();
        assert_eq!(snap.invocations.len(), 1);

        // Write a forged checkpoint that names the exact, reachable current tip OID,
        // but carries a shape-valid empty snapshot (P1-1 vulnerability).
        let forged = DerivedCheckpoint {
            schema_version: DERIVED_CHECKPOINT_SCHEMA_VERSION,
            ledger_tip: tip,
            snapshot: LedgerSnapshot::default(),
        };
        let bytes = lattice_store::canon::canonical_json_bytes(&forged).unwrap();
        std::fs::write(store.checkpoint_path(), &bytes).unwrap();

        // Cold open must NOT trust the forged snapshot just because the tip OID is reachable.
        // It must detect that the snapshot digest does not match the state_digest in tip's
        // commit tree, discard the checkpoint, and reconstruct the true state (1 invocation)
        // from the ledger.
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (fresh_tip, fresh_snap) = fresh.snapshot().unwrap();
        assert_eq!(fresh_tip, tip);
        assert_eq!(
            fresh_snap.invocations.len(),
            1,
            "forged empty checkpoint must be rejected and true state reconstructed"
        );
        assert_eq!(fresh_snap, snap);
    }

    #[test]
    fn lagging_checkpoint_replays_only_delta_events() {
        let (_tmp, layout, key) = test_env();
        let store = InvocationStore::create(&layout, &key).unwrap();

        // Step 1: reserve batch 0.
        let b0 = binding(0);
        store
            .reserve_batch(plan_key(0), std::slice::from_ref(&b0))
            .unwrap();
        let (tip_1, _snap_1) = store.snapshot().unwrap();

        // Save the checkpoint from step 1.
        let cp_step_1 = std::fs::read(store.checkpoint_path()).unwrap();

        // Step 2 & 3: dispatch and terminal.
        store.dispatch_started(b0.invocation_id).unwrap();
        store
            .publish_terminal(
                b0.invocation_id,
                TerminalOutcome::Succeeded,
                TerminalMaterial::Unavailable {
                    reason: UnavailableMaterialReason::DraftPublicationFailed,
                    digest: None,
                    byte_length: None,
                },
                terminal_evidence(),
            )
            .unwrap();
        let (tip_final, snap_final) = store.snapshot().unwrap();
        assert_ne!(tip_1, tip_final);

        // Roll back the checkpoint file to step 1 (a lagging but valid ancestor checkpoint).
        std::fs::write(store.checkpoint_path(), &cp_step_1).unwrap();

        // Cold open: must load step 1 checkpoint, replay delta events to tip_final,
        // and advance the checkpoint to tip_final.
        let fresh = InvocationStore::open(&layout, &key).unwrap();
        let (tip_res, snap_res) = fresh.snapshot().unwrap();
        assert_eq!(tip_res, tip_final);
        assert_eq!(snap_res, snap_final);

        // The rolled-back file is replaced by an authenticated anchor.
        assert_authenticated_checkpoint(&fresh, tip_final);
    }

    #[test]
    fn cold_first_writer_uses_checkpoint_under_write_lock() {
        let (_tmp, layout, key) = test_env();
        let store_a = InvocationStore::create(&layout, &key).unwrap();

        let b0 = binding(0);
        store_a
            .reserve_batch(plan_key(0), std::slice::from_ref(&b0))
            .unwrap();
        store_a.dispatch_started(b0.invocation_id).unwrap();
        let (tip_a, snap_a) = store_a.snapshot().unwrap();
        store_a.save_derived_checkpoint(tip_a, &snap_a).unwrap();

        // Store B is a fresh handle opened in a cold process (in-memory cache is None).
        let store_b = InvocationStore::open(&layout, &key).unwrap();

        // Store B performs a write operation under write_lock.
        // write_state() must load the verified checkpoint from disk instead of
        // replaying all commits from genesis (P1-3).
        let mut b1 = binding(1);
        b1.identity.model_step_ordinal = 1;
        store_b.reserve_batch(plan_key(1), &[b1.clone()]).unwrap();

        let (tip_b, snap_b) = store_b.snapshot().unwrap();
        assert_ne!(tip_b, tip_a);
        assert_eq!(snap_b.plans.len(), 2);
        assert_eq!(snap_b.invocations.len(), 2);
        assert!(snap_b.invocations.contains_key(&b0.invocation_id));
        assert!(snap_b.invocations.contains_key(&b1.invocation_id));

        // Corrupted checkpoint on cold write: write_state() must self-heal via genesis fallback.
        std::fs::write(store_b.checkpoint_path(), b"garbage bytes").unwrap();
        let store_c = InvocationStore::open(&layout, &key).unwrap();
        let mut b2 = binding(2);
        b2.identity.model_step_ordinal = 2;
        store_c.reserve_batch(plan_key(2), &[b2.clone()]).unwrap();
        let (_, snap_c) = store_c.snapshot().unwrap();
        assert_eq!(snap_c.plans.len(), 3);
        assert_eq!(snap_c.invocations.len(), 3);
    }
}
