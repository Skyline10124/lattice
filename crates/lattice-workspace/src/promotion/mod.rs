// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Host-owned promotion module — the only code path that writes Lower.
//!
//! Managed Lower provisioning, cleanliness gates, exclusive locking, ref
//! transactions, journaled materialization, and the public prepare/commit/recover
//! orchestration live here.

mod candidate;
mod journal;
mod lock;
mod lower;
mod materialize;
mod refs;
mod service;

pub use candidate::{
    CandidateHazard, CandidateReviewFact, PinnedCandidate, PromotionLimits, ValidatedCandidate,
    VerificationResult, load_pinned_candidate, pin_candidate, prepare_pinned, validate_candidate,
};
pub use journal::{
    FaultPoint, JournalHeader, JournalRecord, ParsedJournal, PromotionJournal, RecoveryDirection,
    RecoveryOutcome, parse_journal, recover_journal, resolve_recovery_direction,
    run_materialization,
};
pub use lock::{
    DurabilityRecorder, LockMetadata, LockOwnerIdentity, LowerLockGuard, ProcIdentityReader,
    ProcessIdentityReader, StaleLockDisposition, TestIdentityReader, recover_stale_lock,
};
pub use lower::{LowerBaseline, LowerDirtyReason, ManagedLower, PromotionError, PromotionPaths};
pub use materialize::VecDurableRecorder;
pub use materialize::{
    DurableFs, DurableFsRecorder, MaterializationPlan, MaterializeEntry, PathClassification,
    PathState, build_materialization_plan, classify_path,
};
pub use refs::{
    PromotionRefs, RefComponent, RefTransactionError, RefTransactionOutcome, RefTransactionRequest,
    ValidatedRefName, build_update_ref_z_payload, run_ref_transaction,
};
pub use service::{
    PreparedPromotion, PromotionOutcome, PromotionRequest, commit, commit_with_reader, prepare,
    prepare_from_validated, recover, recover_with_reader,
};
