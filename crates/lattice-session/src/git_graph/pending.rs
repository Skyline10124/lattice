// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! PendingTurn journal persistence over the `lattice-store` kernel
//! (ADR 0005 §9/§12, GitHub #144).
//!
//! ```text
//! refs/lattice/sessions/<SessionId>/active-turn    -> current snapshot commit
//! refs/lattice/sessions/<SessionId>/turns/<TurnId> -> TurnCommit conversation commit
//! ```
//!
//! Each snapshot is a commit with canonical `pending.json` whose git parent
//! is the predecessor snapshot; the `TurnStarted` snapshot's parent is the
//! frozen parent conversation tip, grafting the chain onto the conversation
//! graph at acceptance. Stage progression is one CAS per snapshot after
//! complete edge authentication. `Committed` is constructed with the pinned
//! TurnCommit candidate and published only by the final ref transaction.

use std::collections::BTreeSet;

use lattice_store::{RefExpectation, RefTxnEdit, TreeSpec};

use gix::objs::tree::EntryKind;
use lancet_protocol::InvocationId;

use super::GitSessionGraphStore;
use super::commit::turn_commit_from_manifest_and_oid;
use super::helpers::{
    build_agent_json, build_root_tree, build_turn_tree, messages_to_jsonl, parse_oid,
    serialize_manifest,
};
use super::session::resolve_head_oid;
use super::state::{self, SessionHead};
use super::validation;
use crate::graph::prepare_turn_commit;
use crate::pending::contract::{
    CommittedFacts, PENDING_SNAPSHOT_SCHEMA_VERSION, PendingSnapshot, PendingStage,
    TurnClosingFacts, TurnCommitCandidate, is_candidate_revision_of, next_post_user_transcript,
    validate_snapshot_transition,
};
use crate::pending::draft::{DraftClassification, DraftPublication, ResultDraft};
use crate::pending::{
    BindingPlan, CandidateRevisionRequest, MAX_MODEL_ATTEMPTS, MAX_MODEL_STEPS, TurnStartedFacts,
};
use crate::{Result, SessionError};

/// File inside a journal snapshot commit's tree carrying the payload.
pub(crate) const PENDING_TREE_FILE: &str = "pending.json";

const MAX_CANDIDATE_REVISION_SNAPSHOTS: usize = 128;

/// Maximum snapshots in one PendingTurn lineage.
pub(crate) const MAX_PENDING_TURN_LINEAGE_DEPTH: usize = 1
    + MAX_MODEL_STEPS as usize
        * (2 * MAX_MODEL_ATTEMPTS as usize
            + 3
            + 2 * lattice_conversation::MAX_TOOL_CALLS_PER_BATCH)
    + MAX_CANDIDATE_REVISION_SNAPSHOTS
    + 2;

/// The direct active-Turn authority ref.
pub(crate) fn active_turn_ref_name(session_id: &str) -> Result<String> {
    state::validate_session_ref_id(session_id)?;
    Ok(format!("refs/lattice/sessions/{session_id}/active-turn"))
}

/// The create-once permanent pin for one committed Turn.
pub(crate) fn turn_pin_ref_name(session_id: &str, turn_id: &str) -> Result<String> {
    state::validate_session_ref_id(session_id)?;
    state::validate_turn_ref_id(turn_id)?;
    Ok(format!(
        "refs/lattice/sessions/{session_id}/turns/{turn_id}"
    ))
}

/// One published journal tip.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTurnTip {
    /// Journal commit OID of this snapshot.
    pub oid: gix::ObjectId,
    /// The parsed snapshot.
    pub snapshot: PendingSnapshot,
    /// Validated root-through-tip snapshot count. Not persisted.
    pub(crate) lineage_depth: usize,
}

fn completed_attempt_usage(
    stage: &PendingStage,
) -> Option<Option<lattice_conversation::TokenUsage>> {
    match stage {
        PendingStage::ModelAttemptFailed { usage, .. }
        | PendingStage::ModelAttemptInterrupted { usage, .. }
        | PendingStage::ModelAttemptRejected { usage, .. }
        | PendingStage::MessageComplete { usage, .. }
        | PendingStage::BindingPlanned { usage, .. } => Some(*usage),
        _ => None,
    }
}

fn ensure_lineage_can_advance(tip: &PendingTurnTip) -> Result<()> {
    if tip.lineage_depth >= MAX_PENDING_TURN_LINEAGE_DEPTH {
        return Err(SessionError::JournalCorruption {
            detail: format!(
                "pending lineage reached the code-defined depth limit {MAX_PENDING_TURN_LINEAGE_DEPTH}"
            ),
        });
    }
    Ok(())
}

fn stage_turn_id(stage: &PendingStage) -> Option<lancet_protocol::TurnId> {
    match stage {
        PendingStage::TurnStarted(facts) => Some(facts.turn_id),
        PendingStage::ModelAttemptStarted { key }
        | PendingStage::ModelAttemptFailed { key, .. }
        | PendingStage::ModelAttemptInterrupted { key, .. }
        | PendingStage::ModelAttemptRejected { key, .. }
        | PendingStage::MessageComplete { key, .. } => Some(key.turn_id),
        PendingStage::BindingPlanned { plan, .. } | PendingStage::Bound { plan } => {
            Some(plan.plan_key.turn_id)
        }
        PendingStage::StepClosing(inner) => Some(inner.plan_key.turn_id),
        PendingStage::StepClosed { .. } => None,
        PendingStage::TurnClosing(facts) => Some(facts.turn_id),
        PendingStage::Committed(facts) => Some(facts.turn_id),
    }
}

impl GitSessionGraphStore {
    // ─── Reads (lock-free: pin the ref, read immutable objects) ────────

    /// Read the current PendingTurn tip.
    pub fn read_active_turn(&self, session_id: &str) -> Result<Option<PendingTurnTip>> {
        let ref_name = active_turn_ref_name(session_id)?;
        let Some(oid) = self.kernel().ref_target(&ref_name)? else {
            return Ok(None);
        };
        let tip = self.read_pending_snapshot_at(oid)?;
        self.validate_pending_project_binding(session_id, &tip)?;
        Ok(Some(tip))
    }

    fn validate_pending_project_binding(
        &self,
        session_id: &str,
        tip: &PendingTurnTip,
    ) -> Result<()> {
        let (_, session_state) = state::read_state(self.kernel(), session_id)?;
        let session_key = lattice_store::ProjectStoreKey::parse(&session_state.project_store_key)
            .map_err(|err| SessionError::JournalCorruption {
            detail: format!("session '{session_id}' has an invalid project-store binding: {err}"),
        })?;
        let started = match &tip.snapshot.stage {
            PendingStage::TurnStarted(facts) => facts,
            _ => tip
                .snapshot
                .root
                .as_ref()
                .ok_or_else(|| SessionError::JournalCorruption {
                    detail: format!(
                        "pending Turn {} has no TurnStarted root facts",
                        tip.snapshot.turn_id
                    ),
                })?,
        };
        if started.project_store_key != session_key {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "pending Turn {} is bound to project key {}, but session '{session_id}' is bound to {session_key}",
                    tip.snapshot.turn_id, started.project_store_key
                ),
            });
        }
        Ok(())
    }

    /// Read the permanent Turn pin of one committed Turn, when present.
    pub fn read_turn_pin(&self, session_id: &str, turn_id: &str) -> Result<Option<String>> {
        let ref_name = turn_pin_ref_name(session_id, turn_id)?;
        Ok(self
            .kernel()
            .ref_target(&ref_name)?
            .map(|oid| oid.to_string()))
    }

    pub(crate) fn read_pending_snapshot_at(&self, oid: gix::ObjectId) -> Result<PendingTurnTip> {
        let mut reverse_oids = Vec::new();
        let mut cursor = oid;
        loop {
            if reverse_oids.len() == MAX_PENDING_TURN_LINEAGE_DEPTH {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "pending lineage exceeds the code-defined depth limit {MAX_PENDING_TURN_LINEAGE_DEPTH}"
                    ),
                });
            }
            let recorded_at = self.kernel().commit_time(cursor)?;
            let tip = self.read_snapshot_raw(cursor, recorded_at)?;
            let predecessor = tip.snapshot.predecessor.clone();
            let is_root = matches!(tip.snapshot.stage, PendingStage::TurnStarted(_));
            reverse_oids.push(cursor);
            if is_root {
                break;
            }
            cursor = parse_oid(predecessor.as_deref().ok_or_else(|| {
                SessionError::JournalCorruption {
                    detail: "pending lineage reached a non-TurnStarted root".into(),
                }
            })?)?;
        }

        let lineage_depth = reverse_oids.len();
        let mut root = None;
        let mut attempt_usages = Vec::new();
        let mut previous: Option<PendingTurnTip> = None;
        for (index, cursor) in reverse_oids.into_iter().rev().enumerate() {
            let recorded_at = self.kernel().commit_time(cursor)?;
            let mut current = self.read_snapshot_raw(cursor, recorded_at)?;
            if index == 0 {
                let PendingStage::TurnStarted(facts) = &current.snapshot.stage else {
                    return Err(SessionError::JournalCorruption {
                        detail: "pending lineage has no TurnStarted root".into(),
                    });
                };
                root = Some(facts.clone());
            } else {
                current.snapshot.root = root.clone();
            }

            if matches!(
                current.snapshot.stage,
                PendingStage::ModelAttemptStarted { .. }
            ) {
                attempt_usages.push(None);
            } else if previous.as_ref().is_some_and(|prior| {
                matches!(
                    prior.snapshot.stage,
                    PendingStage::ModelAttemptStarted { .. }
                )
            }) && let Some(usage) = completed_attempt_usage(&current.snapshot.stage)
            {
                let slot =
                    attempt_usages
                        .last_mut()
                        .ok_or_else(|| SessionError::JournalCorruption {
                            detail: "attempt outcome has no started usage slot".into(),
                        })?;
                *slot = usage;
            }
            current.snapshot.attempt_usages = attempt_usages.clone();
            current.snapshot.validate()?;
            if let Some(prior) = &previous {
                validate_snapshot_transition(&prior.snapshot, &current.snapshot)?;
            }
            current.lineage_depth = index + 1;
            previous = Some(current);
        }

        let mut tip = previous.ok_or_else(|| SessionError::JournalCorruption {
            detail: "pending lineage is empty".into(),
        })?;
        tip.lineage_depth = lineage_depth;
        Ok(tip)
    }

    fn read_snapshot_raw(
        &self,
        oid: gix::ObjectId,
        recorded_at: Option<i64>,
    ) -> Result<PendingTurnTip> {
        let bytes = self
            .kernel()
            .read_tree_blob(oid, PENDING_TREE_FILE)
            .map_err(|_| SessionError::JournalCorruption {
                detail: format!("journal commit {oid} has no {PENDING_TREE_FILE}"),
            })?;
        let snapshot: PendingSnapshot =
            serde_json::from_slice(&bytes).map_err(|e| SessionError::JournalCorruption {
                detail: format!("parse pending.json at {oid}: {e}"),
            })?;
        let snapshot = PendingSnapshot {
            recorded_at,
            ..snapshot
        };
        snapshot.validate()?;
        self.validate_journal_lineage(oid, &snapshot)?;
        Ok(PendingTurnTip {
            oid,
            snapshot,
            lineage_depth: 0,
        })
    }

    fn validate_journal_lineage(
        &self,
        oid: gix::ObjectId,
        snapshot: &PendingSnapshot,
    ) -> Result<()> {
        let parents = self.kernel().commit_parents(oid)?;
        let first = parents.first().map(|parent| parent.to_string());
        let expected = match (&snapshot.predecessor, &snapshot.stage) {
            (Some(predecessor), _) => Some(predecessor.clone()),
            (None, PendingStage::TurnStarted(facts)) => facts.parent_conversation_tip.clone(),
            (None, other) => {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "journal commit {oid} records stage {} with no predecessor",
                        other.stage_name()
                    ),
                });
            }
        };
        if first != expected || parents.len() > 1 {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "journal commit {oid} records predecessor {expected:?} but its git parents are {:?}",
                    parents
                        .iter()
                        .map(|parent| parent.to_string())
                        .collect::<Vec<_>>()
                ),
            });
        }
        Ok(())
    }

    /// Walk the predecessor chain back to `TurnStarted`.
    pub fn resolve_turn_started(&self, tip: &PendingTurnTip) -> Result<PendingTurnTip> {
        let mut cursor = tip.clone();
        for _ in 0..MAX_PENDING_TURN_LINEAGE_DEPTH {
            if matches!(cursor.snapshot.stage, PendingStage::TurnStarted { .. }) {
                return Ok(cursor);
            }
            let Some(predecessor) = cursor.snapshot.predecessor.as_deref() else {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "journal chain of turn {} reached a non-TurnStarted root",
                        tip.snapshot.turn_id
                    ),
                });
            };
            let oid = parse_oid(predecessor)?;
            let recorded_at = self.kernel().commit_time(oid)?;
            cursor = self.read_snapshot_raw(oid, recorded_at)?;
        }
        Err(SessionError::JournalCorruption {
            detail: format!(
                "pending lineage exceeds the code-defined depth limit {MAX_PENDING_TURN_LINEAGE_DEPTH}"
            ),
        })
    }

    /// Classify a pinned draft candidate against the object database.
    pub fn classify_draft(&self, candidate: &ResultDraft) -> Result<DraftClassification> {
        let bytes = canonical_draft_bytes(candidate)?;
        let oid = self.kernel().blob_id(&bytes)?;
        if !self.kernel().has_object(oid)? {
            return Ok(DraftClassification::CleanAbsent);
        }
        match self.kernel().read_blob(oid) {
            Ok(found) if found == bytes => Ok(DraftClassification::Durable),
            Ok(_) => Err(SessionError::JournalCorruption {
                detail: format!(
                    "object {oid} does not carry the canonical bytes of the draft candidate"
                ),
            }),
            Err(e) => Err(SessionError::Store(e)),
        }
    }

    // ─── Publications (execution lock + repository write lock) ─────────

    /// Publish the first journal snapshot of a Turn.
    pub fn begin_pending_turn(
        &self,
        facts: TurnStartedFacts,
        execution_lock: &lattice_store::FlockGuard,
    ) -> Result<PendingTurnTip> {
        let session_id = facts.session_id.as_str().to_string();
        self.validate_execution_lock(&session_id, execution_lock)?;
        facts.validate()?;
        let _write = self.kernel().write_lock()?;

        let (_, session_state) = state::read_state(self.kernel(), &session_id)?;
        let session_key = lattice_store::ProjectStoreKey::parse(&session_state.project_store_key)
            .map_err(|err| SessionError::JournalCorruption {
            detail: format!("session '{session_id}' has an invalid project-store binding: {err}"),
        })?;
        if facts.project_store_key != session_key {
            return Err(SessionError::InvalidInput(format!(
                "TurnStarted project key {} does not match session '{session_id}' binding {session_key}",
                facts.project_store_key
            )));
        }
        let pin_ref = turn_pin_ref_name(&session_id, &facts.turn_id.to_string())?;
        if self.kernel().ref_target(&pin_ref)?.is_some() {
            return Err(SessionError::Conflict(format!(
                "Turn {} has already been committed",
                facts.turn_id
            )));
        }
        if let Some(tip) = self.read_active_turn(&session_id)? {
            let started = self.resolve_turn_started(&tip)?;
            if started.snapshot.stage == PendingStage::TurnStarted(facts.clone()) {
                // 精确重试只确认原候选，不能用新时钟或配置重建已公开的接受事实。
                let active_ref = active_turn_ref_name(&session_id)?;
                let projection_ref = format!("refs/lattice/session-validation/{session_id}");
                self.kernel()
                    .fsync_pending(&[&active_ref, &projection_ref])?;
                return Ok(tip);
            }
            return Err(SessionError::Conflict(format!(
                "session '{session_id}' already has a different active Turn"
            )));
        }
        let head_oid = resolve_head_oid(self.kernel(), &session_state, &session_id)?;
        match (&facts.parent_conversation_tip, head_oid) {
            (Some(frozen), Some(actual)) if frozen == &actual.to_string() => {}
            (None, None) => {}
            (frozen, actual) => {
                return Err(SessionError::Conflict(format!(
                    "TurnStarted parent tip {frozen:?} does not match the current head {actual:?}"
                )));
            }
        }

        let snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: facts.session_id,
            turn_id: facts.turn_id,
            predecessor: None,
            post_user_transcript: Vec::new(),
            stage: PendingStage::TurnStarted(facts),
            recorded_at: None,
            root: None,
            attempt_usages: Vec::new(),
        };
        snapshot.validate()?;
        let timestamp = parse_rfc3339_to_unix(
            match &snapshot.stage {
                PendingStage::TurnStarted(facts) => &facts.accepted_at,
                _ => unreachable!(),
            },
            "accepted_at",
        )?;
        let oid = self.write_pending_commit(&snapshot, head_oid, timestamp)?;
        let ref_name = active_turn_ref_name(&session_id)?;
        validation::publish_session_edits(
            self.kernel(),
            &session_id,
            vec![RefTxnEdit {
                name: ref_name,
                expect: RefExpectation::MustNotExist,
                new: Some(oid),
            }],
        )
        .map_err(state::map_cas_conflict)?;
        self.read_pending_snapshot_at(oid)
    }

    /// Advance the journal by one fully authenticated CAS snapshot.
    pub fn advance_pending_turn(
        &self,
        session_id: &str,
        next_stage: PendingStage,
        recorded_at_unix: i64,
        execution_lock: &lattice_store::FlockGuard,
    ) -> Result<PendingTurnTip> {
        self.validate_execution_lock(session_id, execution_lock)?;
        next_stage.validate()?;
        let _write = self.kernel().write_lock()?;

        let ref_name = active_turn_ref_name(session_id)?;
        let tip_oid = self.kernel().ref_target(&ref_name)?.ok_or_else(|| {
            SessionError::Conflict(format!(
                "session '{session_id}' has no active Turn to advance"
            ))
        })?;
        let tip = self.read_pending_snapshot_at(tip_oid)?;
        if tip.snapshot.session_id.as_str() != session_id {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "active-turn of '{session_id}' names session '{}'",
                    tip.snapshot.session_id
                ),
            });
        }
        let turn_id = stage_turn_id(&next_stage).unwrap_or(tip.snapshot.turn_id);
        if let Some(stage_turn) = stage_turn_id(&next_stage)
            && stage_turn != tip.snapshot.turn_id
        {
            return Err(SessionError::InvalidInput(format!(
                "stage belongs to turn {stage_turn}, but the active turn is {}",
                tip.snapshot.turn_id
            )));
        }

        if tip.snapshot.stage == next_stage
            && tip.snapshot.recorded_at == Some(recorded_at_unix)
            && let Some(predecessor) = tip.snapshot.predecessor.clone()
        {
            let candidate = PendingSnapshot {
                schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
                session_id: tip.snapshot.session_id,
                turn_id,
                predecessor: Some(predecessor.clone()),
                post_user_transcript: tip.snapshot.post_user_transcript.clone(),
                stage: next_stage.clone(),
                recorded_at: None,
                root: None,
                attempt_usages: Vec::new(),
            };
            let candidate_oid = self.write_pending_commit(
                &candidate,
                Some(parse_oid(&predecessor)?),
                recorded_at_unix,
            )?;
            if candidate_oid == tip.oid {
                self.kernel().fsync_pending(&[ref_name.as_str()])?;
                return Ok(tip);
            }
        }

        ensure_lineage_can_advance(&tip)?;
        let post_user_transcript = next_post_user_transcript(&tip.snapshot, &next_stage)
            .map_err(|e| SessionError::Conflict(e.to_string()))?;
        let snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: tip.snapshot.session_id,
            turn_id,
            predecessor: Some(tip.oid.to_string()),
            post_user_transcript,
            stage: next_stage,
            recorded_at: None,
            root: tip.snapshot.root.clone(),
            attempt_usages: tip.snapshot.attempt_usages.clone(),
        };
        snapshot.validate()?;
        validate_snapshot_transition(&tip.snapshot, &snapshot)
            .map_err(|e| SessionError::Conflict(e.to_string()))?;
        let oid = self.write_pending_commit(&snapshot, Some(tip_oid), recorded_at_unix)?;
        validation::publish_session_edits(
            self.kernel(),
            session_id,
            vec![RefTxnEdit {
                name: ref_name,
                expect: RefExpectation::MustMatch(tip_oid),
                new: Some(oid),
            }],
        )
        .map_err(state::map_cas_conflict)?;
        self.read_pending_snapshot_at(oid)
    }

    /// Execute one candidate revision against the exact pinned tip.
    pub fn execute_candidate_revision(
        &self,
        request: &CandidateRevisionRequest,
        tip: &PendingTurnTip,
        recorded_at_unix: i64,
        execution_lock: &lattice_store::FlockGuard,
        mut next_invocation_id: impl FnMut() -> InvocationId,
    ) -> Result<PendingTurnTip> {
        if tip.snapshot.session_id != request.session_id || tip.snapshot.turn_id != request.turn_id
        {
            return Err(SessionError::Conflict(
                "candidate revision request does not describe the pinned active Turn".to_string(),
            ));
        }
        let PendingStage::BindingPlanned { plan, .. } = &tip.snapshot.stage else {
            return Err(SessionError::Conflict(
                "candidate revision requires BindingPlanned".to_string(),
            ));
        };
        if plan != &request.plan {
            return Err(SessionError::Conflict(
                "candidate revision request does not describe the durable binding plan".to_string(),
            ));
        }

        if let Some(published) = self.published_candidate_revision(request, tip)? {
            self.validate_execution_lock(&request.session_id.to_string(), execution_lock)?;
            let ref_name = active_turn_ref_name(&request.session_id.to_string())?;
            self.kernel().fsync_pending(&[ref_name.as_str()])?;
            return Ok(published);
        }

        let mut revised = request.plan.clone();
        let original: BTreeSet<_> = revised
            .bindings
            .iter()
            .map(|binding| binding.invocation_id)
            .collect();
        let mut generated = BTreeSet::new();
        for binding in &mut revised.bindings {
            let invocation_id = next_invocation_id();
            if original.contains(&invocation_id) || !generated.insert(invocation_id) {
                return Err(SessionError::Conflict(
                    "candidate revision generated a reused InvocationId".to_string(),
                ));
            }
            binding.invocation_id = invocation_id;
            binding.binding_digest = crate::pending::binding_digest(
                &invocation_id,
                &binding.address,
                &binding.method,
                &binding.descriptor_identity,
                &binding.arguments_digest,
            );
        }

        self.revise_binding_candidates(
            &request.session_id.to_string(),
            tip.oid,
            revised,
            recorded_at_unix,
            execution_lock,
        )
    }

    /// Recognize an already-published revision using the same complete edge
    /// predicate as strict reopen and first publication.
    fn published_candidate_revision(
        &self,
        request: &CandidateRevisionRequest,
        expected_tip: &PendingTurnTip,
    ) -> Result<Option<PendingTurnTip>> {
        let session_id = request.session_id.to_string();
        let Some(current) = self.read_active_turn(&session_id)? else {
            return Ok(None);
        };
        if current.oid == expected_tip.oid
            || current.snapshot.predecessor.as_deref()
                != Some(expected_tip.oid.to_string().as_str())
            || current.snapshot.session_id != request.session_id
            || current.snapshot.turn_id != request.turn_id
            || !is_candidate_revision_of(&expected_tip.snapshot.stage, &current.snapshot.stage)
        {
            return Ok(None);
        }
        Ok(Some(current))
    }

    /// Replace every candidate UUID in the current `BindingPlanned` stage.
    pub fn revise_binding_candidates(
        &self,
        session_id: &str,
        expected_tip_oid: gix::ObjectId,
        revised_plan: BindingPlan,
        recorded_at_unix: i64,
        execution_lock: &lattice_store::FlockGuard,
    ) -> Result<PendingTurnTip> {
        self.validate_execution_lock(session_id, execution_lock)?;
        revised_plan.validate()?;
        let _write = self.kernel().write_lock()?;
        let ref_name = active_turn_ref_name(session_id)?;
        let tip_oid = self.kernel().ref_target(&ref_name)?.ok_or_else(|| {
            SessionError::Conflict(format!(
                "session '{session_id}' has no active Turn to revise"
            ))
        })?;
        let tip = self.read_pending_snapshot_at(tip_oid)?;

        if tip_oid != expected_tip_oid {
            let expected = self.read_pending_snapshot_at(expected_tip_oid)?;
            if tip.snapshot.recorded_at == Some(recorded_at_unix)
                && tip.snapshot.predecessor.as_deref()
                    == Some(expected_tip_oid.to_string().as_str())
                && let PendingStage::BindingPlanned { plan, .. } = &tip.snapshot.stage
                && plan == &revised_plan
                && is_candidate_revision_of(&expected.snapshot.stage, &tip.snapshot.stage)
            {
                let candidate = PendingSnapshot {
                    schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
                    session_id: tip.snapshot.session_id,
                    turn_id: tip.snapshot.turn_id,
                    predecessor: Some(expected_tip_oid.to_string()),
                    post_user_transcript: tip.snapshot.post_user_transcript.clone(),
                    stage: tip.snapshot.stage.clone(),
                    recorded_at: None,
                    root: None,
                    attempt_usages: Vec::new(),
                };
                let candidate_oid = self.write_pending_commit(
                    &candidate,
                    Some(expected_tip_oid),
                    recorded_at_unix,
                )?;
                if candidate_oid == tip.oid {
                    self.kernel().fsync_pending(&[ref_name.as_str()])?;
                    return Ok(tip);
                }
            }
            return Err(SessionError::Conflict(format!(
                "active Turn tip changed from expected {expected_tip_oid} to {tip_oid}"
            )));
        }

        let PendingStage::BindingPlanned { usage, .. } = &tip.snapshot.stage else {
            return Err(SessionError::Conflict(
                "candidate revision requires BindingPlanned".to_string(),
            ));
        };
        ensure_lineage_can_advance(&tip)?;
        let next_stage = PendingStage::BindingPlanned {
            plan: revised_plan,
            usage: *usage,
        };
        if !is_candidate_revision_of(&tip.snapshot.stage, &next_stage) {
            return Err(SessionError::Conflict(
                "candidate revision must replace only every invocation UUID and binding digest"
                    .to_string(),
            ));
        }

        let mut post_user_transcript = tip.snapshot.post_user_transcript.clone();
        let last =
            post_user_transcript
                .last_mut()
                .ok_or_else(|| SessionError::JournalCorruption {
                    detail: "BindingPlanned candidate revision has no bound assistant".into(),
                })?;
        *last = match &next_stage {
            PendingStage::BindingPlanned { plan, .. } => plan.bound_message()?,
            _ => unreachable!(),
        };
        let snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: tip.snapshot.session_id,
            turn_id: tip.snapshot.turn_id,
            predecessor: Some(tip_oid.to_string()),
            post_user_transcript,
            stage: next_stage,
            recorded_at: None,
            root: tip.snapshot.root.clone(),
            attempt_usages: tip.snapshot.attempt_usages.clone(),
        };
        snapshot.validate()?;
        validate_snapshot_transition(&tip.snapshot, &snapshot)?;
        let oid = self.write_pending_commit(&snapshot, Some(tip_oid), recorded_at_unix)?;
        validation::publish_session_edits(
            self.kernel(),
            session_id,
            vec![RefTxnEdit {
                name: ref_name,
                expect: RefExpectation::MustMatch(expected_tip_oid),
                new: Some(oid),
            }],
        )
        .map_err(state::map_cas_conflict)?;
        self.read_pending_snapshot_at(oid)
    }

    /// Publish one `ResultDraft` as a content-addressed blob and classify it.
    pub fn publish_draft(
        &self,
        candidate: &ResultDraft,
        execution_lock: &lattice_store::FlockGuard,
    ) -> Result<DraftPublication> {
        candidate.validate()?;
        let session_id = candidate.message.session_id.as_str().to_string();
        self.validate_execution_lock(&session_id, execution_lock)?;
        let _write = self.kernel().write_lock()?;

        let bytes = canonical_draft_bytes(candidate)?;
        let oid = self.kernel().write_blob(&bytes)?;
        match self.kernel().fsync_pending(&[]) {
            Ok(()) => Ok(DraftPublication::Durable {
                draft_oid: oid.to_string(),
            }),
            Err(e @ lattice_store::StoreError::Durability { .. }) => {
                let _ = e;
                Ok(DraftPublication::Uncertain {
                    candidate: candidate.clone(),
                })
            }
            Err(other) => Err(SessionError::Store(other)),
        }
    }

    /// Publish the final conversation commit and delete `active-turn` in one
    /// transaction.
    pub fn finalize_turn(
        &self,
        session_id: &str,
        turn_id: &lancet_protocol::TurnId,
        execution_lock: &lattice_store::FlockGuard,
    ) -> Result<crate::graph::TurnCommit> {
        self.validate_execution_lock(session_id, execution_lock)?;
        let turn_id_str = turn_id.to_string();
        let pin_ref = turn_pin_ref_name(session_id, &turn_id_str)?;

        let _write = self.kernel().write_lock()?;
        if let Some(pinned) = self.read_turn_pin(session_id, &turn_id_str)? {
            return self.confirm_pinned_turn(session_id, turn_id, &pinned);
        }
        let ref_name = active_turn_ref_name(session_id)?;
        let tip_oid = self.kernel().ref_target(&ref_name)?.ok_or_else(|| {
            SessionError::Conflict(format!(
                "session '{session_id}' has no active Turn to finalize"
            ))
        })?;
        let tip = self.read_pending_snapshot_at(tip_oid)?;
        if tip.snapshot.turn_id != *turn_id {
            return Err(SessionError::Conflict(format!(
                "active turn {} is not the requested turn {turn_id}",
                tip.snapshot.turn_id
            )));
        }
        let candidate = match &tip.snapshot.stage {
            PendingStage::TurnClosing(TurnClosingFacts { candidate, .. }) => candidate.clone(),
            other => {
                return Err(SessionError::InvalidInput(format!(
                    "finalize requires a TurnClosing tip, found {}",
                    other.stage_name()
                )));
            }
        };
        ensure_lineage_can_advance(&tip)?;

        let turn_started = self.resolve_turn_started(&tip)?;
        let PendingStage::TurnStarted(facts) = &turn_started.snapshot.stage else {
            return Err(SessionError::JournalCorruption {
                detail: "resolved PendingTurn root is not TurnStarted".into(),
            });
        };
        let frozen_tip = facts.parent_conversation_tip.clone();
        let (_, session_state) = state::read_state(self.kernel(), session_id)?;
        let head_oid = resolve_head_oid(self.kernel(), &session_state, session_id)?;
        match (&frozen_tip, head_oid) {
            (Some(frozen), Some(actual)) if frozen == &actual.to_string() => {}
            (None, None) => {}
            (frozen, actual) => {
                return Err(SessionError::Conflict(format!(
                    "conversation head moved during the Turn: frozen {frozen:?}, head {actual:?}"
                )));
            }
        }

        let input = candidate_turn_commit_input(session_id, &candidate, &frozen_tip)?;
        let prepared = prepare_turn_commit(&input, &candidate.created_at);
        let manifest_bytes = serialize_manifest(&candidate_manifest(&prepared, &candidate))?;
        let messages_jsonl = messages_to_jsonl(&prepared.messages)?;
        let turn_tree = build_turn_tree(
            self.kernel(),
            &manifest_bytes,
            &messages_jsonl,
            &build_agent_json(&candidate.agent_name),
            &candidate.profile_snapshot_json,
        )?;
        let root_tree = build_root_tree(self.kernel(), turn_tree)?;

        let committed_payload = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: tip.snapshot.session_id,
            turn_id: tip.snapshot.turn_id,
            predecessor: Some(tip_oid.to_string()),
            post_user_transcript: tip.snapshot.post_user_transcript.clone(),
            stage: PendingStage::Committed(CommittedFacts {
                turn_id: tip.snapshot.turn_id,
                candidate: candidate.clone(),
            }),
            recorded_at: None,
            root: tip.snapshot.root.clone(),
            attempt_usages: tip.snapshot.attempt_usages.clone(),
        };
        validate_snapshot_transition(&tip.snapshot, &committed_payload)?;
        let committed_bytes = canonical_snapshot_bytes(&committed_payload)?;
        let committed_blob = self.kernel().write_blob(&committed_bytes)?;
        let committed_tree = self.kernel().write_tree(TreeSpec::new().upsert(
            PENDING_TREE_FILE,
            EntryKind::Blob,
            committed_blob,
        ))?;
        let commit_ts = parse_rfc3339_to_unix(&candidate.created_at, "candidate.created_at")?;
        let committed_oid = self.kernel().write_commit_at(
            committed_tree,
            &[tip_oid],
            &format!("lattice pending turn committed {}", tip.snapshot.turn_id),
            commit_ts,
        )?;

        let mut parent_oids = vec![committed_oid];
        for extra in &candidate.side_parent_ids {
            parent_oids.push(parse_oid(extra)?);
        }
        let turn_commit_oid = self.kernel().write_commit_at(
            root_tree,
            &parent_oids,
            &candidate.commit_message,
            commit_ts,
        )?;

        let (state_oid, _) = state::read_state(self.kernel(), session_id)?;
        let mut new_state = session_state.clone();
        new_state.updated_at = candidate.created_at.clone();
        if let SessionHead::Detached { commit_id } = &mut new_state.head {
            *commit_id = turn_commit_oid.to_string();
        }
        let new_state_oid = state::write_state_commit(self.kernel(), Some(state_oid), &new_state)?;

        let mut edits = Vec::with_capacity(4);
        if let SessionHead::Attached { branch } = &session_state.head {
            let head_ref = state::head_ref_name(session_id, branch)?;
            edits.push(RefTxnEdit {
                name: head_ref,
                expect: match head_oid {
                    Some(expected) => RefExpectation::MustMatch(expected),
                    None => RefExpectation::MustNotExist,
                },
                new: Some(turn_commit_oid),
            });
        }
        edits.push(RefTxnEdit {
            name: pin_ref.clone(),
            expect: RefExpectation::MustNotExist,
            new: Some(turn_commit_oid),
        });
        edits.push(RefTxnEdit {
            name: state::state_ref_name(session_id)?,
            expect: RefExpectation::MustMatch(state_oid),
            new: Some(new_state_oid),
        });
        edits.push(RefTxnEdit {
            name: ref_name.clone(),
            expect: RefExpectation::MustMatch(tip_oid),
            new: None,
        });

        validation::publish_session_edits(self.kernel(), session_id, edits).map_err(
            |err| match err {
                lattice_store::StoreError::CasConflict { detail, .. } => {
                    SessionError::Conflict(format!("final turn publication conflict: {detail}"))
                }
                other => SessionError::Store(other),
            },
        )?;

        let manifest = candidate_manifest(&prepared, &candidate);
        let oid_str = turn_commit_oid.to_string();
        let message_ids: Vec<String> = (0..prepared.messages.len())
            .map(|i| format!("{oid_str}:{i}"))
            .collect();
        let mut turn_commit = turn_commit_from_manifest_and_oid(turn_commit_oid, &manifest);
        turn_commit.profile_snapshot_json = candidate.profile_snapshot_json.clone();
        turn_commit.message_ids = message_ids;
        Ok(turn_commit)
    }

    /// 核验一个固定 pin 的 TurnCommit 并补足其 pending 引用屏障。
    /// 调用方必须已持有仓库写锁：本函数只读取不可变对象并 fsync 引用，
    /// 不发布引用、不生成候选、不关闭 pending Turn。固定结果在进入本函数
    /// 前已经关闭，因此确认它不取得会话执行锁，也不等待另一个 Turn 的生命周期。
    fn confirm_pinned_turn(
        &self,
        session_id: &str,
        turn_id: &lancet_protocol::TurnId,
        pinned: &str,
    ) -> Result<crate::graph::TurnCommit> {
        let commit = self.read_finalized_turn(session_id, turn_id, pinned)?;
        if self
            .read_active_turn(session_id)?
            .is_some_and(|tip| tip.snapshot.turn_id == *turn_id)
        {
            return Err(SessionError::JournalCorruption {
                detail: "committed Turn still has an active-turn ref".into(),
            });
        }
        // 最终事务可能已经可见而后置屏障失败；固定引用后确认，不能仅凭 pin 返回成功。
        let mut refs: Vec<String> = self
            .kernel()
            .enumerate_refs(&format!("refs/lattice/sessions/{session_id}/"))?
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        refs.push(active_turn_ref_name(session_id)?);
        refs.push(format!("refs/lattice/session-validation/{session_id}"));
        let names: Vec<&str> = refs.iter().map(String::as_str).collect();
        self.kernel().fsync_pending(&names)?;
        Ok(commit)
    }

    /// 观察端专用确认入口：读取固定 pin，核验 TurnCommit 与日志身份，拒绝同
    /// Turn 的 active-turn 异常，并在同一个短仓库写锁内完成真实的 pending
    /// 引用 fsync 屏障。
    ///
    /// 与 [`Self::finalize_turn`] 不同，本入口不取得会话执行锁，也不能关闭
    /// pending Turn 或发布引用；没有固定已关闭结果的 Turn 在此不可确认，
    /// pending 仍只经显式恢复关闭。
    pub fn confirm_committed_turn(
        &self,
        session_id: &str,
        turn_id: &lancet_protocol::TurnId,
    ) -> Result<crate::graph::TurnCommit> {
        let turn_id_str = turn_id.to_string();
        let _write = self.kernel().write_lock()?;
        let Some(pinned) = self.read_turn_pin(session_id, &turn_id_str)? else {
            return Err(SessionError::NotFound(turn_id_str));
        };
        self.confirm_pinned_turn(session_id, turn_id, &pinned)
    }

    pub(super) fn read_finalized_turn(
        &self,
        session_id: &str,
        expected_turn_id: &lancet_protocol::TurnId,
        commit_oid_hex: &str,
    ) -> Result<crate::graph::TurnCommit> {
        let oid = parse_oid(commit_oid_hex)?;
        let manifest_bytes = self.kernel().read_tree_blob(oid, "turn/manifest.json")?;
        let manifest = super::helpers::parse_manifest(&manifest_bytes)?;
        if manifest.session_id != session_id {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "turn pin of '{session_id}' points at a commit of session '{}'",
                    manifest.session_id
                ),
            });
        }
        if manifest.turn_id != *expected_turn_id {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "turn pin for '{expected_turn_id}' points at turn '{}'",
                    manifest.turn_id
                ),
            });
        }
        let parents = self.kernel().commit_parents(oid)?;
        let journal = parents
            .first()
            .copied()
            .ok_or_else(|| SessionError::JournalCorruption {
                detail: "pinned Turn has no committed journal".into(),
            })?;
        if !is_journal_commit(self.kernel(), journal)? {
            return Err(SessionError::JournalCorruption {
                detail: "pinned Turn parent is not a journal".into(),
            });
        }
        let tip = self.read_pending_snapshot_at(journal)?;
        self.validate_pending_project_binding(session_id, &tip)?;
        let PendingStage::Committed(facts) = &tip.snapshot.stage else {
            return Err(SessionError::JournalCorruption {
                detail: "pinned Turn parent is not Committed".into(),
            });
        };
        if tip.snapshot.session_id.as_str() != session_id || facts.turn_id != *expected_turn_id {
            return Err(SessionError::JournalCorruption {
                detail: "pinned Turn journal identity differs".into(),
            });
        }
        let root = self.resolve_turn_started(&tip)?;
        let PendingStage::TurnStarted(started) = &root.snapshot.stage else {
            unreachable!()
        };
        let input = candidate_turn_commit_input(
            session_id,
            &facts.candidate,
            &started.parent_conversation_tip,
        )?;
        let prepared = prepare_turn_commit(&input, &facts.candidate.created_at);
        if manifest_bytes != serialize_manifest(&candidate_manifest(&prepared, &facts.candidate))? {
            return Err(SessionError::JournalCorruption {
                detail: "pinned Turn manifest differs from final candidate".into(),
            });
        }
        let messages_bytes = self.kernel().read_tree_blob(oid, "turn/messages.jsonl")?;
        if messages_bytes != messages_to_jsonl(&prepared.messages)?.as_bytes()
            || self.kernel().commit_message(oid)? != facts.candidate.commit_message
            || self.kernel().read_tree_blob(oid, "turn/agent.json")?
                != build_agent_json(&facts.candidate.agent_name).as_bytes()
            || self.kernel().read_tree_blob(oid, "turn/profile.json")?
                != facts.candidate.profile_snapshot_json.as_bytes()
            || parents[1..]
                != facts
                    .candidate
                    .side_parent_ids
                    .iter()
                    .map(|id| parse_oid(id))
                    .collect::<Result<Vec<_>>>()?
            || self.kernel().commit_time(oid)?
                != Some(parse_rfc3339_to_unix(
                    &facts.candidate.created_at,
                    "candidate.created_at",
                )?)
        {
            return Err(SessionError::JournalCorruption {
                detail: "pinned Turn content or parents differ from final candidate".into(),
            });
        }
        let mut turn_commit = turn_commit_from_manifest_and_oid(oid, &manifest);
        turn_commit.profile_snapshot_json = facts.candidate.profile_snapshot_json.clone();
        turn_commit.message_ids = (0..prepared.messages.len())
            .map(|i| format!("{commit_oid_hex}:{i}"))
            .collect();
        Ok(turn_commit)
    }

    fn write_pending_commit(
        &self,
        snapshot: &PendingSnapshot,
        parent: Option<gix::ObjectId>,
        timestamp: i64,
    ) -> Result<gix::ObjectId> {
        let bytes = canonical_snapshot_bytes(snapshot)?;
        let blob = self.kernel().write_blob(&bytes)?;
        let tree = self.kernel().write_tree(TreeSpec::new().upsert(
            PENDING_TREE_FILE,
            EntryKind::Blob,
            blob,
        ))?;
        let parents: Vec<gix::ObjectId> = parent.into_iter().collect();
        Ok(self.kernel().write_commit_at(
            tree,
            &parents,
            &format!(
                "lattice pending turn {} {}",
                snapshot.turn_id,
                snapshot.stage.stage_name()
            ),
            timestamp,
        )?)
    }
}

/// Canonical payload bytes of one journal snapshot.
pub(crate) fn canonical_snapshot_bytes(snapshot: &PendingSnapshot) -> Result<Vec<u8>> {
    let value = serde_json::to_value(snapshot)
        .map_err(|e| SessionError::InvalidInput(format!("serialize pending snapshot: {e}")))?;
    Ok(crate::graph::canonical_json_bytes(&value))
}

pub(crate) fn canonical_draft_bytes(draft: &ResultDraft) -> Result<Vec<u8>> {
    let value = serde_json::to_value(draft)
        .map_err(|e| SessionError::InvalidInput(format!("serialize result draft: {e}")))?;
    Ok(crate::graph::canonical_json_bytes(&value))
}

fn parse_rfc3339_to_unix(value: &str, field: &str) -> Result<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.timestamp())
        .map_err(|e| SessionError::InvalidInput(format!("{field} is not RFC 3339: {e}")))
}

fn candidate_turn_commit_input(
    session_id: &str,
    candidate: &TurnCommitCandidate,
    frozen_tip: &Option<String>,
) -> Result<crate::graph::TurnCommitInput> {
    use crate::graph::TurnCommitInput;
    let mut parent_ids = Vec::with_capacity(candidate.side_parent_ids.len() + 1);
    if let Some(tip) = frozen_tip {
        parent_ids.push(tip.clone());
    }
    parent_ids.extend(candidate.side_parent_ids.iter().cloned());
    Ok(TurnCommitInput {
        session_id: session_id.to_string(),
        turn_id: candidate.turn_id,
        parent_ids,
        messages: candidate.messages.clone(),
        agent_name: candidate.agent_name.clone(),
        profile_snapshot_hash: candidate.profile_snapshot_hash.clone(),
        profile_snapshot_json: candidate.profile_snapshot_json.clone(),
        model: candidate.model.clone(),
        provider: candidate.provider.clone(),
        outcome: candidate.outcome.clone(),
        origin: candidate.origin,
        commit_message: candidate.commit_message.clone(),
        usage: candidate.usage_aggregate.map(|aggregate| aggregate.usage()),
        workspace_history_checkpoint: candidate.workspace_history_checkpoint.clone(),
    })
}

fn candidate_manifest(
    prepared: &crate::graph::PreparedTurnCommit,
    candidate: &TurnCommitCandidate,
) -> super::helpers::GitSessionManifest {
    let manifest = super::commit::build_manifest_from_prepared(prepared);
    debug_assert_eq!(manifest.turn_id, candidate.turn_id);
    manifest
}

/// Whether one commit in a conversation-history walk is a PendingTurn journal
/// snapshot.
pub(crate) fn is_journal_commit(
    store: &lattice_store::StoreRepo,
    oid: gix::ObjectId,
) -> Result<bool> {
    let tree = store.commit_tree(oid)?;
    Ok(store.read_tree_path(tree, PENDING_TREE_FILE)?.is_some())
}
