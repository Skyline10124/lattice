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
//! pure [`validate_transition`]. `Committed` is constructed with the pinned
//! TurnCommit candidate and published only by the final ref transaction,
//! together with the branch/`state` advance and the `active-turn` deletion —
//! so the journal chain stays reachable through `turns/<TurnId>` ->
//! TurnCommit -> Committed -> predecessor after `active-turn` disappears.
//!
//! Locking: every journal publication holds the session execution lock and
//! then the repository write lock, in that order (ADR 0005 §5). Reads pin
//! the `active-turn` OID and read immutable objects without a lock.

use std::collections::BTreeSet;

use chrono::Utc;
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
    TurnClosingFacts, TurnCommitCandidate, validate_transition,
};
use crate::pending::draft::{DraftClassification, DraftPublication, ResultDraft};
use crate::pending::{BindingPlan, CandidateRevisionRequest, TurnStartedFacts};
use crate::{Result, SessionError};

/// File inside a journal snapshot commit's tree carrying the payload.
pub(crate) const PENDING_TREE_FILE: &str = "pending.json";

/// The direct active-Turn authority ref. Existence and target are the only
/// authority for "a Turn is pending"; there is no second pending ref and no
/// active-Turn field in `SessionState`.
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

/// One published journal tip: the snapshot commit OID and the parsed
/// snapshot (with `recorded_at` filled from the commit's committer time).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTurnTip {
    /// Journal commit OID of this snapshot.
    pub oid: gix::ObjectId,
    /// The parsed snapshot.
    pub snapshot: PendingSnapshot,
}

/// Extract the Turn identity a stage carries, when it names one. `None`
/// stages (`StepClosed`) inherit the predecessor tip's Turn.
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

/// Whether `revised` is exactly a candidate revision of `original`.
///
/// One definition serves both directions: `revise_binding_candidates` admits a
/// publication only when this holds, and `published_candidate_revision`
/// recognizes an already-published revision by the same rule. Everything but
/// the candidate ids and their derived binding digests must be byte-equal, and
/// every candidate must be new and distinct.
fn is_candidate_revision_of(original: &BindingPlan, revised: &BindingPlan) -> bool {
    if original.plan_key != revised.plan_key
        || original.message != revised.message
        || original.bindings.len() != revised.bindings.len()
    {
        return false;
    }
    let original_ids: BTreeSet<_> = original
        .bindings
        .iter()
        .map(|binding| binding.invocation_id)
        .collect();
    let mut revised_ids = BTreeSet::new();
    for (before, after) in original.bindings.iter().zip(&revised.bindings) {
        if original_ids.contains(&after.invocation_id)
            || before.address != after.address
            || before.method != after.method
            || before.descriptor_identity != after.descriptor_identity
            || before.arguments_digest != after.arguments_digest
            || !revised_ids.insert(after.invocation_id)
        {
            return false;
        }
    }
    true
}

impl GitSessionGraphStore {
    // ─── Reads (lock-free: pin the ref, read immutable objects) ────────

    /// Read the current PendingTurn tip. `Ok(None)` when no Turn is
    /// pending. A malformed or inconsistent snapshot is typed
    /// [`SessionError::JournalCorruption`], never repaired.
    pub fn read_active_turn(&self, session_id: &str) -> Result<Option<PendingTurnTip>> {
        let ref_name = active_turn_ref_name(session_id)?;
        let Some(oid) = self.kernel().ref_target(&ref_name)? else {
            return Ok(None);
        };
        Ok(Some(self.read_pending_snapshot_at(oid)?))
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
        let recorded_at = self.kernel().commit_time(oid)?;
        let mut tip = self.read_snapshot_raw(oid, recorded_at)?;
        // Fill the chain-root facts for every non-root tip: reconciliation
        // needs the frozen user message, parent tip, and limits, and the
        // journal chain is their only authority.
        if !matches!(tip.snapshot.stage, PendingStage::TurnStarted { .. }) {
            let root = self.resolve_turn_started(&tip)?;
            if let PendingStage::TurnStarted(facts) = root.snapshot.stage {
                tip.snapshot.root = Some(facts);
            }
        }
        tip.snapshot.validate()?;
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
        Ok(PendingTurnTip { oid, snapshot })
    }

    /// The payload's `predecessor` and the commit's git parent are two
    /// recordings of the same lineage fact; a chain where they disagree is
    /// typed corruption, never a readable journal. The chain root has no
    /// payload predecessor: its single git parent is the frozen parent
    /// conversation tip it grafts onto (ADR 0005 §9), and a root frozen on
    /// an empty branch has no parent at all.
    fn validate_journal_lineage(
        &self,
        oid: gix::ObjectId,
        snapshot: &PendingSnapshot,
    ) -> Result<()> {
        let parents = self.kernel().commit_parents(oid)?;
        let first = parents.first().map(|parent| parent.to_string());
        let expected = match (&snapshot.predecessor, &snapshot.stage) {
            (Some(predecessor), _) => Some(predecessor.clone()),
            // `PendingSnapshot::validate` already rejects a non-`TurnStarted`
            // stage without a predecessor, so this is the chain root.
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
                    "journal commit {oid} records predecessor {expected:?} but its git \
                     parents are {:?}",
                    parents
                        .iter()
                        .map(|parent| parent.to_string())
                        .collect::<Vec<_>>()
                ),
            });
        }
        Ok(())
    }

    /// Walk the predecessor chain back to the `TurnStarted` snapshot of the
    /// pending Turn. The chain root carries the frozen parent conversation
    /// tip the final transaction CASes against.
    pub fn resolve_turn_started(&self, tip: &PendingTurnTip) -> Result<PendingTurnTip> {
        let mut cursor = tip.clone();
        loop {
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
    }

    /// Classify a pinned draft candidate against the object database
    /// (reopen path, ADR 0005 §12): the content-addressed object either
    /// survived (`Durable`, and its bytes must equal the candidate's
    /// canonical form) or was never durably written (`CleanAbsent`). The
    /// lookup is read-only — classification must never materialize the
    /// object it is asking about.
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

    /// Publish the first journal snapshot of a Turn: `TurnStarted`
    /// (ADR 0005 §8/§9). The frozen parent conversation tip must equal the
    /// session's current head under the held locks; the snapshot commit's
    /// git parent is that tip, grafting the journal chain onto the
    /// conversation graph.
    pub fn begin_pending_turn(
        &self,
        facts: TurnStartedFacts,
        execution_lock: &lattice_store::FlockGuard,
    ) -> Result<PendingTurnTip> {
        let session_id = facts.session_id.as_str().to_string();
        self.validate_execution_lock(&session_id, execution_lock)?;
        facts.validate()?;
        let _write = self.kernel().write_lock()?;

        // The session must exist and be quiet: no active Turn may exist and
        // the frozen tip must equal the resolved head.
        let (_, session_state) = state::read_state(self.kernel(), &session_id)?;
        let head_oid = resolve_head_oid(self.kernel(), &session_state, &session_id)?;
        match (&facts.parent_conversation_tip, head_oid) {
            (Some(frozen), Some(actual)) if frozen == &actual.to_string() => {}
            (None, None) => {}
            (frozen, actual) => {
                return Err(SessionError::Conflict(format!(
                    "TurnStarted parent tip {:?} does not match the current head {:?}",
                    frozen, actual
                )));
            }
        }

        if self
            .kernel()
            .ref_target(&active_turn_ref_name(&session_id)?)?
            .is_some()
        {
            return Err(SessionError::Conflict(format!(
                "session '{session_id}' already has an active Turn"
            )));
        }

        let snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: facts.session_id,
            turn_id: facts.turn_id,
            predecessor: None,
            stage: PendingStage::TurnStarted(facts),
            recorded_at: None,
            root: None,
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

    /// Advance the journal by one validated CAS snapshot. The transition
    /// must satisfy [`validate_transition`] against the current tip, the
    /// stage must belong to the same Turn, and `Committed` is rejected as
    /// an `active-turn` target (the final ref transaction is its only
    /// publication path). `recorded_at_unix` is the snapshot commit's
    /// committer time; reconciliation callers pass the durable tip's own
    /// time so recovery introduces no new clock.
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

        // Idempotent exact retry. `cas_ref` is the publication authority and
        // already runs the post-publication ref barrier, so a barrier
        // failure returns `Durability` on an already-published snapshot.
        // The byte-identical retry of that exact transition must recognize
        // its own snapshot instead of failing as an illegal edge, and must
        // then re-attempt the barrier via `fsync_pending` (it does not call
        // `cas_ref`). Confirmation is exact — the tip has to be the very
        // commit this call would write, computed from the tip's own recorded
        // predecessor — so a different candidate or a CAS race is never
        // mistaken for success.
        if tip.snapshot.stage == next_stage
            && tip.snapshot.recorded_at == Some(recorded_at_unix)
            && let Some(predecessor) = tip.snapshot.predecessor.clone()
        {
            let candidate = PendingSnapshot {
                schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
                session_id: tip.snapshot.session_id,
                turn_id,
                predecessor: Some(predecessor.clone()),
                stage: next_stage.clone(),
                recorded_at: None,
                root: None,
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

        validate_transition(&tip.snapshot.stage, &next_stage)
            .map_err(|e| SessionError::Conflict(e.to_string()))?;

        let snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: tip.snapshot.session_id,
            turn_id,
            predecessor: Some(tip.oid.to_string()),
            stage: next_stage,
            recorded_at: None,
            root: None,
        };
        snapshot.validate()?;
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

    /// Execute one [`CandidateRevisionRequest`] against the exact tip the
    /// reducer's facts were pinned to (ADR 0005 §9, #139-A decision D-A2).
    ///
    /// The reducer performs no I/O, clock, store, or UUID work, so UUID
    /// generation is injected here. The request is verified against the
    /// pinned tip before any write: same session, same Turn, same durable
    /// `BindingPlanned` plan. The CAS then pins `tip.oid`, so a concurrent
    /// advance leaves the original `BindingPlanned` untouched.
    ///
    /// **Post-CAS recovery.** A revision may be CAS-published and then fail
    /// its durability barrier. The generator is not required to be
    /// reproducible, so recovery never depends on re-minting the same UUIDs:
    /// this call first re-reads the durable `active-turn` and, if it already
    /// holds a candidate revision of exactly `request.plan` grafted on
    /// `tip.oid`, repeats that publication's durability barrier and returns
    /// it without generating anything. Only a genuinely absent revision mints
    /// fresh candidates.
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

        // Recover a published-but-undurable revision before minting anything.
        if let Some(published) = self.published_candidate_revision(request, tip.oid)? {
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

    /// The already-published candidate revision of `request.plan` grafted
    /// directly on `expected_tip_oid`, if one exists.
    ///
    /// Recognition is structural, not by candidate value: the durable tip
    /// must be `BindingPlanned`, name `expected_tip_oid` as its predecessor,
    /// and carry exactly the revision shape `revise_binding_candidates`
    /// admits — same plan key, same message, same binding count and order,
    /// same address/method/descriptor/arguments per binding, every candidate
    /// replaced by a distinct new id, and every binding digest recomputed.
    /// Nothing else can be mistaken for this Turn's revision.
    fn published_candidate_revision(
        &self,
        request: &CandidateRevisionRequest,
        expected_tip_oid: gix::ObjectId,
    ) -> Result<Option<PendingTurnTip>> {
        let session_id = request.session_id.to_string();
        let Some(current) = self.read_active_turn(&session_id)? else {
            return Ok(None);
        };
        if current.oid == expected_tip_oid
            || current.snapshot.predecessor.as_deref()
                != Some(expected_tip_oid.to_string().as_str())
            || current.snapshot.session_id != request.session_id
            || current.snapshot.turn_id != request.turn_id
        {
            return Ok(None);
        }
        let PendingStage::BindingPlanned { plan, .. } = &current.snapshot.stage else {
            return Ok(None);
        };
        if is_candidate_revision_of(&request.plan, plan) {
            Ok(Some(current))
        } else {
            Ok(None)
        }
    }

    /// Replace every candidate UUID in the current `BindingPlanned` stage.
    ///
    /// This is the only legal same-stage journal revision. It preserves the
    /// complete message, structural plan, binding order, Tool identities, and
    /// arguments while CAS-publishing one full replacement plan.
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
            if tip.snapshot.recorded_at == Some(recorded_at_unix)
                && tip.snapshot.predecessor.as_deref()
                    == Some(expected_tip_oid.to_string().as_str())
                && let PendingStage::BindingPlanned { plan, usage } = &tip.snapshot.stage
                && plan == &revised_plan
            {
                let candidate = PendingSnapshot {
                    schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
                    session_id: tip.snapshot.session_id,
                    turn_id: tip.snapshot.turn_id,
                    predecessor: Some(expected_tip_oid.to_string()),
                    stage: PendingStage::BindingPlanned {
                        plan: revised_plan.clone(),
                        usage: *usage,
                    },
                    recorded_at: None,
                    root: None,
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

        let PendingStage::BindingPlanned { plan, usage } = &tip.snapshot.stage else {
            return Err(SessionError::Conflict(
                "candidate revision requires BindingPlanned".to_string(),
            ));
        };
        if !is_candidate_revision_of(plan, &revised_plan) {
            return Err(SessionError::Conflict(
                "candidate revision must replace only every invocation UUID and binding digest"
                    .to_string(),
            ));
        }

        let next_stage = PendingStage::BindingPlanned {
            plan: revised_plan,
            usage: *usage,
        };
        let snapshot = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: tip.snapshot.session_id,
            turn_id: tip.snapshot.turn_id,
            predecessor: Some(tip_oid.to_string()),
            stage: next_stage,
            recorded_at: None,
            root: None,
        };
        snapshot.validate()?;
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

    /// Publish one `ResultDraft` as a content-addressed blob and classify
    /// the publication (ADR 0005 §12). Success is `Durable`; a durability
    /// barrier failure classifies as `Uncertain` carrying the exact
    /// candidate — the live owner must revalidate during its finite
    /// FinalizeBudget, and a mismatch is corruption. The draft carries no
    /// outcome and can never upgrade itself into one.
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
                // The write was attempted; durability is unproven.
                let _ = e;
                Ok(DraftPublication::Uncertain {
                    candidate: candidate.clone(),
                })
            }
            Err(other) => Err(SessionError::Store(other)),
        }
    }

    /// The one final SessionStore ref transaction (ADR 0005 §6/§9):
    /// advance the conversation branch (or detached state), create the
    /// `turns/<TurnId>` pin, update `state`, and delete `active-turn` —
    /// all-or-nothing. The `Committed` journal object (parent = the
    /// `TurnClosing` tip) and the conversation TurnCommit (parents =
    /// `[Committed] ++ candidate.parent_ids[1..]`) are constructed from the
    /// pinned candidate so a retry after any failure republishes
    /// byte-identical objects. Idempotent: if the transaction already
    /// completed, the pinned TurnCommit is returned unchanged.
    pub fn finalize_turn(
        &self,
        session_id: &str,
        turn_id: &lancet_protocol::TurnId,
        execution_lock: &lattice_store::FlockGuard,
    ) -> Result<crate::graph::TurnCommit> {
        self.validate_execution_lock(session_id, execution_lock)?;
        let turn_id_str = turn_id.to_string();
        let pin_ref = turn_pin_ref_name(session_id, &turn_id_str)?;

        // Idempotent completion: the pin exists exactly when the
        // transaction committed (ref transactions are atomic).
        if let Some(pinned) = self.read_turn_pin(session_id, &turn_id_str)? {
            return self.read_finalized_turn(session_id, turn_id, &pinned);
        }

        let _write = self.kernel().write_lock()?;
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

        // The frozen parent tip must still be the conversation head.
        let turn_started = self.resolve_turn_started(&tip)?;
        let frozen_tip = match &turn_started.snapshot.stage {
            PendingStage::TurnStarted(facts) => facts.parent_conversation_tip.clone(),
            _ => unreachable!("resolve_turn_started returns a TurnStarted root"),
        };
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

        // Build the conversation TurnCommit from the pinned candidate: the
        // deterministic identity machinery runs over pinned inputs plus the
        // durable frozen tip (journal authority), never mutable input.
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

        // The Committed journal object: parent is the TurnClosing tip.
        let committed_payload = PendingSnapshot {
            schema_version: PENDING_SNAPSHOT_SCHEMA_VERSION,
            session_id: tip.snapshot.session_id,
            turn_id: tip.snapshot.turn_id,
            predecessor: Some(tip_oid.to_string()),
            stage: PendingStage::Committed(CommittedFacts {
                turn_id: tip.snapshot.turn_id,
                candidate: candidate.clone(),
            }),
            recorded_at: None,
            root: None,
        };
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

        // Conversation parents: the frozen tip stays reachable through the
        // journal chain root; merge side-parents keep their place.
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

        // New session state (timestamp; detached head follows the new tip).
        let (state_oid, _) = state::read_state(self.kernel(), session_id)?;
        let mut new_state = session_state.clone();
        new_state.updated_at = Utc::now().to_rfc3339();
        if let SessionHead::Detached { commit_id } = &mut new_state.head {
            *commit_id = turn_commit_oid.to_string();
        }
        let new_state_oid = state::write_state_commit(self.kernel(), Some(state_oid), &new_state)?;

        let mut edits = Vec::with_capacity(4);
        if let SessionHead::Attached { branch } = &session_state.head {
            let head_ref = state::head_ref_name(session_id, branch)?;
            edits.push(RefTxnEdit {
                name: head_ref.clone(),
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

        // Any CAS failure leaves the whole transaction unpublished.
        validation::publish_session_edits(self.kernel(), session_id, edits).map_err(
            |err| match err {
                lattice_store::StoreError::CasConflict { detail, .. } => {
                    SessionError::Conflict(format!("final turn publication conflict: {detail}"))
                }
                other => SessionError::Store(other),
            },
        )?;

        let mut published: Vec<String> = vec![pin_ref.clone(), ref_name.clone()];
        if let SessionHead::Attached { branch } = &session_state.head {
            published.push(state::head_ref_name(session_id, branch)?);
        }
        published.push(state::state_ref_name(session_id)?);
        let published: Vec<&str> = published.iter().map(String::as_str).collect();
        self.kernel().fsync_pending(&published)?;

        let manifest = candidate_manifest(&prepared, &candidate);
        let oid_str = turn_commit_oid.to_string();
        let message_ids: Vec<String> = (0..prepared.messages.len())
            .map(|i| format!("{oid_str}:{i}"))
            .collect();
        let mut turn_commit = turn_commit_from_manifest_and_oid(turn_commit_oid, &manifest);
        turn_commit.message_ids = message_ids;
        Ok(turn_commit)
    }

    fn read_finalized_turn(
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
        let messages_bytes = self.kernel().read_tree_blob(oid, "turn/messages.jsonl")?;
        let count = String::from_utf8(messages_bytes)
            .map(|jsonl| jsonl.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0);
        let mut turn_commit = turn_commit_from_manifest_and_oid(oid, &manifest);
        turn_commit.message_ids = (0..count)
            .map(|i| format!("{commit_oid_hex}:{i}"))
            .collect();
        Ok(turn_commit)
    }

    /// Write one journal snapshot commit. Caller holds the execution lock
    /// and the repository write lock.
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

/// Project the pinned candidate onto the typed conversation-commit input.
fn candidate_turn_commit_input(
    session_id: &str,
    candidate: &TurnCommitCandidate,
    frozen_tip: &Option<String>,
) -> Result<crate::graph::TurnCommitInput> {
    use crate::graph::TurnCommitInput;
    // Logical lineage: the frozen tip (journal authority) plus the
    // candidate's durable side-parents.
    let mut parent_ids = Vec::with_capacity(candidate.side_parent_ids.len() + 1);
    if let Some(tip) = frozen_tip {
        parent_ids.push(tip.clone());
    }
    parent_ids.extend(candidate.side_parent_ids.iter().cloned());
    Ok(TurnCommitInput {
        session_id: session_id.to_string(),
        // The journal's explicit TurnId is the commit's canonical identity
        // (ADR 0005 §1); including it in the identity payloads is the
        // M-GH100-01 sibling-uniqueness fix.
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

/// The typed conversation manifest of a journal-finalized Turn.
fn candidate_manifest(
    prepared: &crate::graph::PreparedTurnCommit,
    candidate: &TurnCommitCandidate,
) -> super::helpers::GitSessionManifest {
    let manifest = super::commit::build_manifest_from_prepared(prepared);
    debug_assert_eq!(manifest.turn_id, candidate.turn_id);
    manifest
}

/// Whether one commit in a conversation-history walk is a PendingTurn
/// journal snapshot (carries `pending.json` instead of the `turn/` tree).
/// Transcript reconstruction skips these; the journal chain is reachable
/// but is not conversation content.
pub(crate) fn is_journal_commit(
    store: &lattice_store::StoreRepo,
    oid: gix::ObjectId,
) -> Result<bool> {
    let tree = store.commit_tree(oid)?;
    Ok(store.read_tree_path(tree, PENDING_TREE_FILE)?.is_some())
}
