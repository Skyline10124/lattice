// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn execution and streaming API.
//!
//! [`stream_turn`] is the single production execution core: it runs the agent
//! loop live, emits public [`TurnEvent`] values through a bounded channel, and
//! commits exactly one turn. [`run_turn`] is a synchronous facade that drains
//! the same live stream via [`collector::RunTurnCollector`] and returns
//! [`RunTurnResult`].

mod cep;
mod collector;
mod execute;
pub mod lifecycle;
mod live;
mod llm;
mod mapping;
pub mod promotion;
pub mod resume;
pub mod snapshot_journal;
mod types;
pub mod upper;
pub mod workspace_backend;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod run_tests;

#[cfg(test)]
mod stream_tests;

#[cfg(test)]
mod types_tests;

#[cfg(test)]
mod cep_tests;

#[cfg(test)]
mod live_tests;

pub use collector::collect_turn_result_from_events;
pub use execute::{run_turn, stream_turn};
pub use lifecycle::{
    LifecycleFailureKind, ResourceAction, TurnLifecycleOutcome, UnfinalizedTurn,
    WorkspaceDirtyHook, classify_lifecycle_failure, commit_promoted_with_retry, finalize_turn,
    outcome_promoted, outcome_workspace_less, run_terminal_verification,
};
pub use promotion::{
    PromotionLifecycleError, PromotionSessionMetadata, PromotionTurnTarget, TurnTargetError,
    prepare_promotion_target, promote_preexported_candidate, resolve_turn_target,
    retry_pinned_candidate,
};
pub use resume::{
    PruneSnapshotsRequest, ResumeError, ResumeMode, ResumeTurnRequest, SnapshotPruneResult,
    prune_session_snapshots, resolve_resume_mode, resume_turn,
};
pub use types::{
    RunTurnRequest, RunTurnResult, RunTurnStage, RunTurnStatus, ToolResultSummary, TurnEvent,
    TurnEventStream, TurnUsageSummary,
};

use lattice_workspace::promotion::{PromotionLimits, PromotionOutcome, VerificationResult};
use lattice_workspace::{UpperHandle, Workspace};

/// Phase 35 compatibility: export from the Upper workspace, then promote.
///
/// Phase 36 lifecycle orchestration should export the candidate once before
/// verification and call [`promote_preexported_candidate`] directly (D-02/D-05).
pub async fn promote_verified_candidate(
    workspace: &dyn Workspace,
    upper: &UpperHandle,
    target: &PromotionTurnTarget,
    metadata: &PromotionSessionMetadata,
    verification: VerificationResult,
    limits: PromotionLimits,
) -> Result<PromotionOutcome, crate::session::ApiError> {
    let export = workspace
        .export_candidate(upper, &target.baseline_oid, limits.max_artifact_bytes)
        .await
        .map_err(promotion::map_workspace_error)?;

    promote_preexported_candidate(target, metadata, export, verification, limits).await
}

#[doc(hidden)]
pub use cep::CepExecutorAdapter;
