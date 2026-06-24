// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Resume eligibility classification, cold-restore/pinned-retry services,
//! and explicit manual snapshot pruning.
//!
//! This module implements the resume and prune API.
//!
//! # Resume
//!
//! Only three pending dispositions are eligible for resume:
//! - `PendingWorktree` and `PendingVerification` → cold snapshot restore.
//! - `PendingPromotion` → pinned candidate retry (no snapshot required).
//!
//! All other dispositions (terminal and `PendingConflict`) reject resume.
//!
//! # Prune
//!
//! Explicit manual snapshot pruning scoped to a single Session.
//! No automatic, TTL, size-based, or cross-Session garbage collection.

use std::sync::Arc;

use lattice_session::{TurnCommit, TurnCommitInput, TurnLifecycleEvidence, prepare_turn_commit};
use lattice_workspace::Workspace;

use crate::session::{ApiError, SessionApi};

// ─── Resume Types ──────────────────────────────────────────────────────────

/// How a pending turn should be resumed.
///
/// Classified from the current disposition and lifecycle evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeMode {
    /// Cold snapshot restore: `PendingWorktree` or `PendingVerification`.
    RestoreSnapshot { snapshot_name: String },
    /// Pinned promotion retry: `PendingPromotion`.
    RetryPinnedPromotion {
        candidate_oid: String,
        candidate_ref: String,
    },
}

/// Resume eligibility failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeError {
    /// The disposition does not support resume.
    IneligibleDisposition { disposition: String, detail: String },
    /// Restore mode requires a snapshot reference but none is recorded.
    SnapshotRequired,
    /// The recorded snapshot is operationally lost.
    SnapshotLost { name: String },
    /// Promotion retry requires a candidate reference but none is recorded.
    CandidateRequired,
}

impl std::fmt::Display for ResumeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IneligibleDisposition {
                disposition,
                detail,
            } => {
                write!(
                    f,
                    "resume ineligible for disposition '{disposition}': {detail}"
                )
            }
            Self::SnapshotRequired => {
                write!(f, "resume requires a recorded snapshot reference")
            }
            Self::SnapshotLost { name } => {
                write!(f, "recorded snapshot '{name}' is operationally lost")
            }
            Self::CandidateRequired => {
                write!(f, "resume requires a recorded candidate reference")
            }
        }
    }
}

impl std::error::Error for ResumeError {}

/// Resume turn request.
///
/// `workspace_override` allows tests to inject a deterministic adapter;
/// production uses the API-owned Microsandbox workspace.
#[derive(Clone)]
pub struct ResumeTurnRequest {
    pub session_id: String,
    pub loaded_profile: Option<String>,
    pub cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    pub workspace_override: Option<Arc<dyn Workspace>>,
}

impl std::fmt::Debug for ResumeTurnRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumeTurnRequest")
            .field("session_id", &self.session_id)
            .field("loaded_profile", &self.loaded_profile)
            .field("cancel_flag", &self.cancel_flag)
            .field("workspace_override", &self.workspace_override.is_some())
            .finish()
    }
}

// ─── Prune Types ───────────────────────────────────────────────────────────

/// Manual snapshot prune request.
///
/// `workspace_override` allows tests to inject a deterministic adapter;
/// production uses the API-owned Microsandbox workspace.
#[derive(Clone)]
pub struct PruneSnapshotsRequest {
    pub session_id: String,
    pub workspace_override: Option<Arc<dyn Workspace>>,
}

impl std::fmt::Debug for PruneSnapshotsRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PruneSnapshotsRequest")
            .field("session_id", &self.session_id)
            .field("workspace_override", &self.workspace_override.is_some())
            .finish()
    }
}

/// Deterministic prune result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPruneResult {
    /// Snapshots successfully released.
    pub released: Vec<String>,
    /// Snapshots skipped (active/pending referenced).
    pub skipped: Vec<String>,
    /// Snapshots already missing at call time.
    pub already_missing: Vec<String>,
}

// ─── Functions ─────────────────────────────────────────────────────────────

/// Classify resume eligibility from a turn commit's disposition and lifecycle
/// evidence.
///
/// Returns the [`ResumeMode`] for eligible pending dispositions, or a typed
/// [`ResumeError`] for ineligible ones. Never mutates the commit.
///
/// # Classification rules
///
/// | Disposition            | Mode                    | Requires                        |
/// |------------------------|-------------------------|---------------------------------|
/// | `PendingWorktree`      | `RestoreSnapshot`       | `lifecycle.snapshot_ref`        |
/// | `PendingVerification`  | `RestoreSnapshot`       | `lifecycle.snapshot_ref`        |
/// | `PendingPromotion`     | `RetryPinnedPromotion`  | `lifecycle.candidate_oid+ref`   |
/// | `PendingConflict`      | *rejected*              | —                               |
/// | All terminal           | *rejected*              | —                               |
pub fn resolve_resume_mode(turn: &TurnCommit) -> Result<ResumeMode, ResumeError> {
    use lattice_session::WorkspaceDisposition;

    match &turn.disposition {
        // ── Restore snapshot modes ────────────────────────────────────────
        WorkspaceDisposition::PendingWorktree | WorkspaceDisposition::PendingVerification => {
            let lifecycle = turn.lifecycle.as_ref();

            // Require snapshot_ref in lifecycle evidence.
            let snapshot_name = lifecycle
                .and_then(|lc| lc.snapshot_ref.as_ref())
                .map(|sr| sr.name.clone())
                .ok_or(ResumeError::SnapshotRequired)?;

            Ok(ResumeMode::RestoreSnapshot { snapshot_name })
        }

        // ── Pinned promotion retry ───────────────────────────────────────
        WorkspaceDisposition::PendingPromotion => {
            let lifecycle = turn.lifecycle.as_ref();

            // Require candidate_oid AND candidate_ref.
            let (candidate_oid, candidate_ref) = lifecycle
                .and_then(|lc| {
                    let oid = lc.candidate_oid.as_deref()?;
                    let cref = lc.candidate_ref.as_deref()?;
                    if oid.is_empty() || cref.is_empty() {
                        None
                    } else {
                        Some((oid.to_string(), cref.to_string()))
                    }
                })
                .ok_or(ResumeError::CandidateRequired)?;

            Ok(ResumeMode::RetryPinnedPromotion {
                candidate_oid,
                candidate_ref,
            })
        }

        // ── Rejected dispositions ─────────────────────────────────────────
        // each ineligible variant is matched explicitly (no catch-all),
        // so adding a new WorkspaceDisposition variant forces a compile error
        // here and a deliberate resume-eligibility decision instead of
        // silently matching the new variant as "unsupported".
        WorkspaceDisposition::PendingConflict => Err(ResumeError::IneligibleDisposition {
            disposition: WorkspaceDisposition::PendingConflict.as_str().to_string(),
            detail: "conflict requires user resolution before resume".to_string(),
        }),
        WorkspaceDisposition::Promoted => Err(ResumeError::IneligibleDisposition {
            disposition: WorkspaceDisposition::Promoted.as_str().to_string(),
            detail: "turn already promoted".to_string(),
        }),
        WorkspaceDisposition::Rejected => Err(ResumeError::IneligibleDisposition {
            disposition: WorkspaceDisposition::Rejected.as_str().to_string(),
            detail: "turn was rejected; start a new turn".to_string(),
        }),
        WorkspaceDisposition::Aborted => Err(ResumeError::IneligibleDisposition {
            disposition: WorkspaceDisposition::Aborted.as_str().to_string(),
            detail: "turn was aborted; start a new turn".to_string(),
        }),
        WorkspaceDisposition::NotApplicable => Err(ResumeError::IneligibleDisposition {
            disposition: WorkspaceDisposition::NotApplicable.as_str().to_string(),
            detail: "no workspace state to resume".to_string(),
        }),
    }
}

/// Format current time as RFC3339 UTC string.
fn format_rfc3339_utc() -> String {
    // delegate to chrono instead of hand-rolling Gregorian calendar
    // arithmetic from epoch seconds. `SecondsFormat::Secs` + `use_z = true`
    // reproduces the previous `YYYY-MM-DDTHH:MM:SSZ` output exactly.
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Resume a pending turn as a new immutable linked turn.
///
/// Opens the Session, looks up the current HEAD turn commit, classifies
/// eligibility via [`resolve_resume_mode`], and creates a new terminal commit
/// with `resumes_turn_oid` set to the source SQLite commit ID.
///
/// For restore modes: the cold snapshot restore and LANCET runtime rebuild
/// happen at the `stream_turn` integration level. This function creates the
/// immutable linkage commit.
///
/// For promotion retry: the pinned candidate is re-validated and retried
/// through the promotion path. The resulting commit carries the resume
/// linkage.
///
/// The prior commit is never modified.
pub async fn resume_turn(
    api: &SessionApi,
    request: ResumeTurnRequest,
) -> Result<TurnCommit, ApiError> {
    // 1. Open session and get current HEAD commit.
    let session = api.open_session(crate::session::OpenSessionRequest {
        id: request.session_id.clone(),
    })?;

    let status = api.status(&session.session_id)?;
    let head_commit_id = status
        .current_commit_id
        .ok_or_else(|| ApiError::BadRequest("session has no commits to resume".into()))?;

    let source_turn = api
        .get_turn_commit(&session.session_id, &head_commit_id)?
        .ok_or_else(|| ApiError::Internal(format!("HEAD commit '{head_commit_id}' not found")))?;

    // reject resume of a commit that is itself a resume linkage commit.
    // The source's lifecycle carries `resumes_turn_oid` only on commits produced
    // by `resume_turn` itself; resuming such a commit would create an unbounded
    // resume→resume chain where each new commit reuses the pending disposition.
    if source_turn
        .lifecycle
        .as_ref()
        .and_then(|lc| lc.resumes_turn_oid.as_ref())
        .is_some()
    {
        return Err(ApiError::BadRequest(
            "cannot resume a turn that is itself a resume linkage commit; resume the original pending turn instead".into(),
        ));
    }

    // 2. Check snapshot_lost for restore modes (operational metadata).
    if api
        .get_snapshot_lost(&source_turn.commit_id)
        .map_err(|e| ApiError::Internal(format!("snapshot_lost lookup: {e}")))?
        .is_some_and(|lost| lost)
    {
        let name = source_turn
            .lifecycle
            .as_ref()
            .and_then(|lc| lc.snapshot_ref.as_ref())
            .map(|sr| sr.name.clone())
            .unwrap_or_default();
        return Err(ApiError::BadRequest(format!(
            "snapshot '{}' is operationally lost; cannot resume",
            name
        )));
    }

    // 3. Classify eligibility.
    let _mode = resolve_resume_mode(&source_turn).map_err(|e| match e {
        ResumeError::IneligibleDisposition { detail, .. } => ApiError::BadRequest(detail),
        ResumeError::SnapshotRequired => {
            ApiError::BadRequest("no snapshot recorded for this turn".into())
        }
        ResumeError::SnapshotLost { name } => {
            ApiError::BadRequest(format!("snapshot '{name}' is operationally lost"))
        }
        ResumeError::CandidateRequired => {
            ApiError::BadRequest("no candidate recorded for promotion retry".into())
        }
    })?;

    // 4. Build new lifecycle evidence with resumes_turn_oid.
    let source_lifecycle = source_turn
        .lifecycle
        .clone()
        .ok_or_else(|| ApiError::Internal("source turn has no lifecycle evidence".into()))?;

    let new_lifecycle = TurnLifecycleEvidence {
        target_project_ref: source_lifecycle.target_project_ref.clone(),
        baseline_oid: source_lifecycle.baseline_oid.clone(),
        candidate_oid: source_lifecycle.candidate_oid.clone(),
        candidate_ref: source_lifecycle.candidate_ref.clone(),
        verification: source_lifecycle.verification.clone(),
        snapshot_ref: source_lifecycle.snapshot_ref.clone(),
        resumes_turn_oid: Some(source_turn.commit_id.clone()),
    };

    // 5. Create the new turn commit parented on the current HEAD.
    let input = TurnCommitInput {
        session_id: session.session_id.clone(),
        parent_ids: vec![head_commit_id],
        messages: vec![],
        agent_name: source_turn.agent_name.clone(),
        profile_snapshot_hash: source_turn.profile_snapshot_hash.clone(),
        profile_snapshot_json: source_turn.profile_snapshot_json.clone(),
        model: source_turn.model.clone(),
        provider: source_turn.provider.clone(),
        status: source_turn.status.clone(),
        disposition: source_turn.disposition.clone(),
        lifecycle: Some(new_lifecycle),
        commit_message: format!(
            "Resume from {}",
            &source_turn.commit_id[..8.min(source_turn.commit_id.len())]
        ),
        usage: None,
        terminal_reason: None,
    };

    let created_at = format_rfc3339_utc();
    let prepared = prepare_turn_commit(&input, &created_at);

    api.commit_prepared_turn(prepared)
}

/// Prune retained snapshots for a single Session.
///
/// Lists only the addressed Session's derived snapshot prefix, preserves
/// active/pending referenced snapshots, releases unreferenced ones
/// idempotently. Never modifies immutable commit evidence.
///
/// # Scope fence
///
/// Only releases snapshots matching `lattice-{session_id}-*`. No automatic
/// timer, TTL, size-based, or cross-Session garbage collection is performed.
pub async fn prune_session_snapshots(
    api: &SessionApi,
    request: PruneSnapshotsRequest,
) -> Result<SnapshotPruneResult, ApiError> {
    // Validate session exists.
    let _session = api.open_session(crate::session::OpenSessionRequest {
        id: request.session_id.clone(),
    })?;

    let workspace = request
        .workspace_override
        .ok_or_else(|| ApiError::Internal("no workspace configured for snapshot pruning".into()))?;

    // Collect the snapshot names still referenced by pending (resumable) turn
    // commits in this session. Pruning must preserve these so pending turns
    // (PendingWorktree / PendingVerification) remain resumable — releasing a
    // snapshot a pending turn depends on is a data-loss risk.
    let log = api
        .log(&request.session_id)
        .map_err(|e| ApiError::Internal(format!("log: {e}")))?;
    let pending_snapshot_names: std::collections::HashSet<String> = log
        .iter()
        .filter(|entry| {
            matches!(
                entry.disposition,
                lattice_session::WorkspaceDisposition::PendingWorktree
                    | lattice_session::WorkspaceDisposition::PendingVerification
            )
        })
        .filter_map(|entry| {
            api.get_turn_commit(&request.session_id, &entry.commit_id)
                .ok()
                .flatten()
                .and_then(|c| c.lifecycle)
                .and_then(|lc| lc.snapshot_ref)
                .map(|sr| sr.name)
        })
        .collect();

    // List retained snapshots scoped to this session.
    let names = workspace
        .list_retained_snapshots(&request.session_id)
        .await
        .map_err(|e| ApiError::Internal(format!("list snapshots: {e}")))?;

    let mut released = Vec::new();
    let mut skipped = Vec::new();
    let mut already_missing = Vec::new();

    for name in names {
        let snapshot_ref = lattice_workspace::SnapshotRef {
            backend: lattice_workspace::Backend::Deterministic,
            name: name.clone(),
        };

        // Preserve snapshots still referenced by pending turns.
        if pending_snapshot_names.contains(&name) {
            skipped.push(name);
            continue;
        }

        // Check existence before release for deterministic result.
        let exists = workspace
            .retained_snapshot_exists(&snapshot_ref)
            .await
            .map_err(|e| ApiError::Internal(format!("check snapshot: {e}")))?;

        if exists {
            workspace
                .release_retained_snapshot(&snapshot_ref)
                .await
                .map_err(|e| ApiError::Internal(format!("release snapshot: {e}")))?;
            released.push(name);
        } else {
            already_missing.push(name);
        }
    }

    Ok(SnapshotPruneResult {
        released,
        skipped,
        already_missing,
    })
}
