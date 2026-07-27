// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Cell evidence ledger — one call's authorization evidence and effect
//! record (#28 B5/A2, C1/C8).
//!
//! Three separate records travel together but never merge: the
//! [`AdmissionEvidence`] that says *why* the call was authorized (capability
//! names), the [`ObservedEffect`]s that say *what happened* (real cells with
//! host-resolved scope and a concrete target), and the [`DeniedOperation`]s
//! that were refused before touching a host facility. A capability alias is
//! admission evidence only and never appears as an effect.
//!
//! One `seq` counter orders effects and denials together, so a multi-effect
//! call keeps its per-call total order into audit and attestation.
//!
//! The ledger is backed by `Arc<Mutex<...>>` so a clone taken before the call
//! shares the same records: the call moves one clone into the wasmtime
//! `Store` (dropped on return), while the other clone retains everything
//! recorded during execution. The transport reads [`snapshot`] from the
//! surviving clone after the call returns.
//!
//! [`snapshot`]: CellEvidenceLedger::snapshot

use std::sync::{Arc, Mutex};

use lancet_protocol::cell::Cell;
use lancet_protocol::evidence::{
    AdmissionEvidence, DENIED_OPERATIONS_RECORD_VERSION, DeniedOperation, DeniedOperationsRecord,
    EffectSource, OBSERVED_EFFECTS_RECORD_VERSION, ObservedEffect, ObservedEffectsRecord,
    ObservedScope, ObservedTarget,
};

#[derive(Debug, Default)]
struct EvidenceInner {
    /// Next `seq`, shared by effects and denials.
    seq: u32,
    admission: Option<AdmissionEvidence>,
    effects: Vec<ObservedEffect>,
    denied: Vec<DeniedOperation>,
}

/// In-memory recorder for one call's Cell evidence. Clones share the
/// underlying records, so the caller can clone before passing into a moved
/// context and read the records back after the call returns.
#[derive(Debug, Default, Clone)]
pub struct CellEvidenceLedger {
    inner: Arc<Mutex<EvidenceInner>>,
}

impl CellEvidenceLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the call's admission evidence. Written once per call, before
    /// the runtime is entered; a denied call records it too.
    pub fn set_admission(&self, admission: AdmissionEvidence) {
        self.inner.lock().unwrap().admission = Some(admission);
    }

    /// Record an effect that actually happened. The ledger assigns `seq`;
    /// `context_measure` is host-computed, never guest-supplied (D12).
    pub fn record_effect(
        &self,
        cell: Cell,
        scope: ObservedScope,
        target: ObservedTarget,
        source: EffectSource,
        context_measure: Option<u64>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.seq;
        inner.seq += 1;
        inner.effects.push(ObservedEffect {
            seq,
            cell,
            scope,
            target,
            source,
            context_measure,
        });
    }

    /// Record a refused operation. The host facility was never invoked, so
    /// this is evidence, not an effect (B3).
    pub fn record_denied(
        &self,
        cell: Cell,
        scope: ObservedScope,
        target: ObservedTarget,
        reason: impl Into<String>,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.seq;
        inner.seq += 1;
        inner.denied.push(DeniedOperation {
            seq,
            cell,
            scope,
            target,
            reason: reason.into(),
        });
    }

    /// The call's evidence triple: why it was authorized, what happened, and
    /// what was refused. Admission is absent only for a call the transport
    /// never admitted; the other two are versioned roots so a persisted
    /// reader can gate on `version` before interpreting either.
    pub fn snapshot(
        &self,
    ) -> (
        Option<AdmissionEvidence>,
        ObservedEffectsRecord,
        DeniedOperationsRecord,
    ) {
        let inner = self.inner.lock().unwrap();
        (
            inner.admission.clone(),
            ObservedEffectsRecord {
                version: OBSERVED_EFFECTS_RECORD_VERSION,
                effects: inner.effects.clone(),
            },
            DeniedOperationsRecord {
                version: DENIED_OPERATIONS_RECORD_VERSION,
                denied: inner.denied.clone(),
            },
        )
    }

    /// The 4-segment audit keys (`ctx:ext:scope:trust`) of the observed
    /// effects, in `seq` order. Derived from the same effects the ledger
    /// reports, so the legacy `component_cells` key can never disagree with
    /// the versioned record (C8).
    pub fn snapshot_keys(&self) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .effects
            .iter()
            .map(|effect| effect.cell.audit_key(effect.scope.trust))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use lancet_protocol::cell::{ChannelKind, CtxOp, ExtImpact, Trust};

    fn scope() -> ObservedScope {
        ObservedScope {
            scope: "session".into(),
            trust: Trust::In,
            channel: ChannelKind::Session,
        }
    }

    fn cell(ctx_op: CtxOp) -> Cell {
        Cell {
            ctx_op,
            ext_impact: ExtImpact::None,
            scope: "session".into(),
        }
    }

    fn target() -> ObservedTarget {
        ObservedTarget::Session {
            turn_id: "turn-1".into(),
        }
    }

    /// Effects and denials share one `seq` sequence, so the call's total
    /// order survives even though they are reported in separate lists.
    #[test]
    fn effects_and_denials_share_one_sequence() {
        let ledger = CellEvidenceLedger::new();
        ledger.record_effect(
            cell(CtxOp::Mutate),
            scope(),
            target(),
            EffectSource::Operation,
            None,
        );
        ledger.record_denied(cell(CtxOp::Alloc), scope(), target(), "uncovered");
        ledger.record_effect(
            cell(CtxOp::Alloc),
            scope(),
            target(),
            EffectSource::Result,
            Some(17),
        );

        let (admission, effects, denied) = ledger.snapshot();
        assert!(admission.is_none(), "no admission recorded");
        assert_eq!(effects.version, OBSERVED_EFFECTS_RECORD_VERSION);
        assert_eq!(
            effects.effects.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert_eq!(effects.effects[1].context_measure, Some(17));
        assert_eq!(denied.version, DENIED_OPERATIONS_RECORD_VERSION);
        assert_eq!(denied.denied.len(), 1);
        assert_eq!(denied.denied[0].seq, 1);
    }

    /// A clone taken before the call sees everything the moved clone records.
    #[test]
    fn clones_share_the_underlying_records() {
        let ledger = CellEvidenceLedger::new();
        let moved = ledger.clone();
        moved.record_effect(
            cell(CtxOp::Alloc),
            scope(),
            target(),
            EffectSource::Result,
            Some(2),
        );
        drop(moved);
        assert_eq!(ledger.snapshot().1.effects.len(), 1);
    }

    /// `snapshot_keys` is derived from the effects, so a denial contributes
    /// no cell key and the trust segment comes from the observed scope.
    #[test]
    fn snapshot_keys_derive_from_effects_only() {
        let ledger = CellEvidenceLedger::new();
        assert!(ledger.snapshot_keys().is_empty());

        ledger.record_effect(
            cell(CtxOp::Mutate),
            scope(),
            target(),
            EffectSource::Operation,
            None,
        );
        ledger.record_denied(cell(CtxOp::Alloc), scope(), target(), "uncovered");
        ledger.record_effect(
            cell(CtxOp::Alloc),
            scope(),
            target(),
            EffectSource::Result,
            Some(4),
        );

        assert_eq!(
            ledger.snapshot_keys(),
            vec![
                "mutate:none:session:in".to_string(),
                "alloc:none:session:in".to_string(),
            ]
        );
    }

    #[test]
    fn admission_evidence_round_trips_through_the_ledger() {
        use lancet_protocol::evidence::ADMISSION_EVIDENCE_VERSION;

        let ledger = CellEvidenceLedger::new();
        ledger.set_admission(AdmissionEvidence {
            version: ADMISSION_EVIDENCE_VERSION,
            tool: "noop.echo".into(),
            approved_capabilities: vec!["session.result".into()],
            denied_capabilities: Vec::new(),
            boundary_excluded: Vec::new(),
            max_boundary: Trust::In,
        });
        let (admission, _, _) = ledger.snapshot();
        assert_eq!(
            admission.expect("admission recorded").approved_capabilities,
            vec!["session.result".to_string()]
        );
    }
}
