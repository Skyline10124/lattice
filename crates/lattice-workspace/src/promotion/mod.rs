// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Host-owned promotion module — the only code path that writes Lower.
//!
//! Managed Lower provisioning, cleanliness gates, exclusive locking, ref
//! transactions, journaled materialization, and the public prepare/commit/recover
//! orchestration live here.

mod lower;
mod lock;
mod candidate;
mod refs;
mod materialize;
mod journal;
mod service;

pub use lower::{
    LowerBaseline, LowerDirtyReason, ManagedLower, PromotionError, PromotionPaths,
};
pub use refs::{
    PromotionRefs, RefComponent, RefTransactionError, RefTransactionOutcome,
    RefTransactionRequest, ValidatedRefName, build_update_ref_z_payload, run_ref_transaction,
};
pub use lock::{
    recover_stale_lock, DurabilityRecorder, LowerLockGuard, LockMetadata,
    LockOwnerIdentity, ProcessIdentityReader, ProcIdentityReader, StaleLockDisposition,
    TestIdentityReader,
};
pub use candidate::{
    validate_candidate, CandidateHazard, CandidateReviewFact, PromotionLimits,
    ValidatedCandidate, VerificationResult,
};
pub use materialize::{
    build_materialization_plan, classify_path, DurableFs, DurableFsRecorder, MaterializationPlan,
    MaterializeEntry, PathClassification, PathState,
};
pub use journal::{
    parse_journal, recover_journal, resolve_recovery_direction, run_materialization, FaultPoint,
    JournalHeader, JournalRecord, ParsedJournal, PromotionJournal, RecoveryDirection,
    RecoveryOutcome,
};
pub use service::{
    commit, commit_with_reader, prepare, recover, recover_with_reader, PreparedPromotion,
    PromotionOutcome, PromotionRequest,
};
pub use materialize::VecDurableRecorder;
