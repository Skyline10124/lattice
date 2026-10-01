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
//! call keeps its per-call total order into audit and evidence storage.
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
    EffectSource, MutationEvidence, NetworkEvidence, OBSERVED_EFFECTS_RECORD_VERSION,
    ObservedEffect, ObservedEffectsRecord, ObservedScope, ObservedTarget,
    WorkspaceVisibilityEvidence,
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
            truncated: None,
            visibility: None,
            mutation: None,
            network: None,
        });
    }

    /// Record a bounded domain operation's effect (#30).
    ///
    /// Distinct from [`Self::record_effect`] because an observation carries two
    /// facts that operation effects generally do not: the bytes it returned to
    /// the guest (the context allocation the criterion asks for) and whether the
    /// host had to cut the result to stay inside a bound. Both are host-computed;
    /// a guest cannot reach this function or the values it writes.
    pub fn record_observation_effect(
        &self,
        cell: Cell,
        scope: ObservedScope,
        target: ObservedTarget,
        bytes_returned: u64,
        truncated: bool,
        visibility: WorkspaceVisibilityEvidence,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.seq;
        inner.seq += 1;
        inner.effects.push(ObservedEffect {
            seq,
            cell,
            scope,
            target,
            source: EffectSource::Operation,
            context_measure: Some(bytes_returned),
            truncated: Some(truncated),
            visibility: Some(visibility),
            mutation: None,
            network: None,
        });
    }

    /// Record one network hop after outbound I/O began.
    pub fn record_network_effect(
        &self,
        cell: Cell,
        scope: ObservedScope,
        target: ObservedTarget,
        context_measure: Option<u64>,
        truncated: Option<bool>,
        network: NetworkEvidence,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.seq;
        inner.seq += 1;
        inner.effects.push(ObservedEffect {
            seq,
            cell,
            scope,
            target,
            source: EffectSource::Operation,
            context_measure,
            truncated,
            visibility: None,
            mutation: None,
            network: Some(network),
        });
    }

    /// Record a successful Structured Workspace Mutation (#31).
    pub fn record_mutation_effect(
        &self,
        cell: Cell,
        scope: ObservedScope,
        target: ObservedTarget,
        mutation: MutationEvidence,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.seq;
        inner.seq += 1;
        inner.effects.push(ObservedEffect {
            seq,
            cell,
            scope,
            target,
            source: EffectSource::Operation,
            context_measure: None,
            truncated: None,
            visibility: None,
            mutation: Some(mutation),
            network: None,
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
        self.record_denied_with_mutation(cell, scope, target, reason, None);
    }

    /// Record a refused mutation import with explicit revision / precondition
    /// facts (#31).
    pub fn record_denied_with_mutation(
        &self,
        cell: Cell,
        scope: ObservedScope,
        target: ObservedTarget,
        reason: impl Into<String>,
        mutation: Option<MutationEvidence>,
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
            mutation,
            network: None,
        });
    }

    /// Record a network-related refusal with the hop facts gathered so far.
    pub fn record_network_denied(
        &self,
        cell: Cell,
        scope: ObservedScope,
        target: ObservedTarget,
        reason: impl Into<String>,
        network: NetworkEvidence,
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
            mutation: None,
            network: Some(network),
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
}
