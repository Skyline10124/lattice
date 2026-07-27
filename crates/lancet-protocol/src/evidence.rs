// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Cell admission evidence and the observed-effect ledger (#28 B5/A2, C1/C8).
//!
//! Two separate records per call: *why* it was authorized
//! (`AdmissionEvidence` — approved capability names) and *what happened*
//! (`ObservedEffectsRecord` — real cells with host-resolved scope and a
//! concrete target). A capability alias is admission evidence only; it is
//! never reported as an effect.
//!
//! These records are persisted inside audit `metadata_json` and attestation
//! predicates, so they follow the `provenance.rs` persisted-root convention:
//! no `deny_unknown_fields` anywhere, and readers check `version` before
//! interpreting the contents.

use crate::cell::{Cell, ChannelKind, Trust};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Current `AdmissionEvidence::version`.
pub const ADMISSION_EVIDENCE_VERSION: u32 = 1;

/// Current `ObservedEffectsRecord::version`.
pub const OBSERVED_EFFECTS_RECORD_VERSION: u32 = 1;

/// Current `DeniedOperationsRecord::version`.
pub const DENIED_OPERATIONS_RECORD_VERSION: u32 = 1;

/// The concrete runtime object an effect touched (C1). A scope name is open
/// vocabulary and cannot express a runtime boundary on its own, so every
/// effect also carries the target the host authorized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservedTarget {
    Path { path: String },
    Host { host: String },
    Session { turn_id: String },
    None,
}

/// Scope identity resolved by the host from the descriptor. Never
/// guest-supplied: trust comes from `ToolDescriptor::trust_of` (undeclared
/// scope == `ex`, B4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservedScope {
    /// Descriptor scope name (the scope id the cell roots in).
    pub scope: String,
    pub trust: Trust,
    pub channel: ChannelKind,
}

/// Which host seam produced an effect: a Tool Host import, or the call's
/// declared result cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectSource {
    Operation,
    Result,
}

/// One effect that actually happened (B5): the real cell, the host-resolved
/// scope, and the concrete target — never a capability alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservedEffect {
    /// Per-call total order; the record is sorted by it.
    pub seq: u32,
    pub cell: Cell,
    pub scope: ObservedScope,
    pub target: ObservedTarget,
    pub source: EffectSource,
    /// Host-computed context allocation in bytes, recorded on every result
    /// effect whatever `ctx_op` the guest declared (D12) and absent on
    /// operation effects, which return no value. Tool self-reported numbers
    /// are never trusted (A3); the charge/credit seam stays in #6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_measure: Option<u64>,
}

/// Versioned container for a call's effects (C1/C8): one call may produce
/// several effects, and they are never collapsed into a single cell-key
/// scalar. Producers keep `effects` sorted by `seq`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ObservedEffectsRecord {
    pub version: u32,
    pub effects: Vec<ObservedEffect>,
}

/// A refused Tool Host operation (B3): recorded evidence, kept apart from
/// actual effects because the host facility was never invoked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeniedOperation {
    /// Shares the call's `seq` sequence with `ObservedEffect`.
    pub seq: u32,
    pub cell: Cell,
    pub scope: ObservedScope,
    pub target: ObservedTarget,
    pub reason: String,
}

/// Versioned container for a call's refused operations. Every persisted
/// evidence root carries its own `version` so a reader can check it before
/// interpreting the contents; denials are no exception. Producers keep
/// `denied` sorted by `seq`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeniedOperationsRecord {
    pub version: u32,
    pub denied: Vec<DeniedOperation>,
}

/// Why a call was authorized (B5/A2/B6): the capability *names* the
/// admission stages approved, plus what each stage removed. Producers emit
/// every list sorted and deduplicated so audit and attestation bytes are
/// stable. A denied call still carries this record — with an empty or
/// partial approved set — so the denial keeps its "why".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AdmissionEvidence {
    pub version: u32,
    pub tool: String,
    pub approved_capabilities: Vec<String>,
    /// Removed by the profile deny list (stage 2).
    pub denied_capabilities: Vec<String>,
    /// Removed by the `max_boundary` predicate (stage 3).
    pub boundary_excluded: Vec<String>,
    pub max_boundary: Trust,
}

/// Operator/profile admission inputs (B1/B2), shared by profile translation,
/// the admission algorithm, and policy tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityAdmissionPolicy {
    pub enabled_tools: BTreeSet<String>,
    pub denied_capabilities: BTreeSet<String>,
    pub max_boundary: Trust,
}

impl Default for CapabilityAdmissionPolicy {
    /// Fail-closed: no tool is enabled and no capability may cross into the
    /// external world. Deliberately asymmetric with `Trust::default() == Ex`
    /// (an undeclared *scope* is fail-closed as `ex`; an unset *boundary* is
    /// fail-closed as `in`), which is why this impl is explicit — a derived
    /// `Default` would admit every `ex` capability.
    fn default() -> Self {
        Self {
            enabled_tools: BTreeSet::new(),
            denied_capabilities: BTreeSet::new(),
            max_boundary: Trust::In,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cell::{CtxOp, ExtImpact};
    use serde_json::json;

    fn session_scope() -> ObservedScope {
        ObservedScope {
            scope: "session".into(),
            trust: Trust::In,
            channel: ChannelKind::Session,
        }
    }

    fn effect(seq: u32, ctx_op: CtxOp, source: EffectSource) -> ObservedEffect {
        ObservedEffect {
            seq,
            cell: Cell {
                ctx_op,
                ext_impact: ExtImpact::None,
                scope: "session".into(),
            },
            scope: session_scope(),
            target: ObservedTarget::Session {
                turn_id: "turn-1".into(),
            },
            source,
            context_measure: match ctx_op {
                CtxOp::Alloc => Some(42),
                _ => None,
            },
        }
    }

    #[test]
    fn observed_effects_record_round_trips_in_seq_order() {
        let record = ObservedEffectsRecord {
            version: OBSERVED_EFFECTS_RECORD_VERSION,
            effects: vec![
                effect(0, CtxOp::Mutate, EffectSource::Operation),
                effect(1, CtxOp::Alloc, EffectSource::Result),
            ],
        };
        let serialized = serde_json::to_string(&record).unwrap();
        let parsed: ObservedEffectsRecord = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed, record);
        // C8: a multi-effect call stays an array, not a collapsed scalar.
        assert_eq!(parsed.effects.len(), 2);
        assert_eq!(parsed.effects[0].seq, 0);
        assert_eq!(parsed.effects[1].seq, 1);
        // D12: the measure rides only on the Alloc effect.
        assert_eq!(parsed.effects[0].context_measure, None);
        assert_eq!(parsed.effects[1].context_measure, Some(42));
    }

    #[test]
    fn observed_target_variants_are_tagged() {
        let targets = [
            (
                ObservedTarget::Path {
                    path: "src/lib.rs".into(),
                },
                "path",
            ),
            (
                ObservedTarget::Host {
                    host: "example.com".into(),
                },
                "host",
            ),
            (
                ObservedTarget::Session {
                    turn_id: "turn-1".into(),
                },
                "session",
            ),
            (ObservedTarget::None, "none"),
        ];
        for (target, kind) in targets {
            let value = serde_json::to_value(&target).unwrap();
            assert_eq!(value["kind"], kind);
            let parsed: ObservedTarget = serde_json::from_value(value).unwrap();
            assert_eq!(parsed, target);
        }
    }

    #[test]
    fn admission_and_denial_records_round_trip() {
        let admission = AdmissionEvidence {
            version: ADMISSION_EVIDENCE_VERSION,
            tool: "noop.echo".into(),
            approved_capabilities: vec!["session.result".into()],
            denied_capabilities: vec!["session.events".into()],
            boundary_excluded: vec!["net.fetch".into()],
            max_boundary: Trust::In,
        };
        let parsed: AdmissionEvidence =
            serde_json::from_str(&serde_json::to_string(&admission).unwrap()).unwrap();
        assert_eq!(parsed, admission);

        let denied = DeniedOperation {
            seq: 3,
            cell: Cell {
                ctx_op: CtxOp::Mutate,
                ext_impact: ExtImpact::None,
                scope: "session".into(),
            },
            scope: session_scope(),
            target: ObservedTarget::Session {
                turn_id: "turn-1".into(),
            },
            reason: "capability not admitted".into(),
        };
        let record = DeniedOperationsRecord {
            version: DENIED_OPERATIONS_RECORD_VERSION,
            denied: vec![denied],
        };
        let parsed: DeniedOperationsRecord =
            serde_json::from_str(&serde_json::to_string(&record).unwrap()).unwrap();
        assert_eq!(parsed, record);
    }

    /// Every persisted evidence root is a versioned object, so a reader can
    /// gate on `version` before interpreting any of them (design §3). A bare
    /// array of denials is not a readable root.
    #[test]
    fn every_persisted_evidence_root_carries_a_version() {
        for value in [
            serde_json::to_value(ObservedEffectsRecord {
                version: OBSERVED_EFFECTS_RECORD_VERSION,
                effects: Vec::new(),
            })
            .unwrap(),
            serde_json::to_value(DeniedOperationsRecord {
                version: DENIED_OPERATIONS_RECORD_VERSION,
                denied: Vec::new(),
            })
            .unwrap(),
            serde_json::to_value(AdmissionEvidence {
                version: ADMISSION_EVIDENCE_VERSION,
                tool: "noop.echo".into(),
                approved_capabilities: Vec::new(),
                denied_capabilities: Vec::new(),
                boundary_excluded: Vec::new(),
                max_boundary: Trust::In,
            })
            .unwrap(),
        ] {
            assert_eq!(
                value.get("version").and_then(serde_json::Value::as_u64),
                Some(1),
                "persisted root must expose a numeric version: {value}"
            );
        }
    }

    #[test]
    fn persisted_records_tolerate_unknown_fields_on_read() {
        // Persisted-root convention: a newer producer's extra fields must not
        // break an older reader (no `deny_unknown_fields`).
        let effects: ObservedEffectsRecord = serde_json::from_value(json!({
            "version": 1,
            "future_field": {"any": "shape"},
            "effects": [{
                "seq": 0,
                "cell": {"ctx_op": "alloc", "ext_impact": "none", "scope": "session"},
                "scope": {"scope": "session", "trust": "in", "channel": "session"},
                "target": {"kind": "session", "turn_id": "turn-1"},
                "source": "result",
                "future_effect_field": 7
            }]
        }))
        .unwrap();
        assert_eq!(effects.effects.len(), 1);
        // Absent optional measure reads as `None`.
        assert_eq!(effects.effects[0].context_measure, None);

        let admission: AdmissionEvidence = serde_json::from_value(json!({
            "version": 1,
            "tool": "noop.echo",
            "approved_capabilities": ["session.result"],
            "denied_capabilities": [],
            "boundary_excluded": [],
            "max_boundary": "in",
            "future_field": true
        }))
        .unwrap();
        assert_eq!(admission.approved_capabilities.len(), 1);

        let denied: DeniedOperationsRecord = serde_json::from_value(json!({
            "version": 1,
            "future_field": 0,
            "denied": [{
                "seq": 1,
                "cell": {"ctx_op": "mutate", "ext_impact": "none", "scope": "session"},
                "scope": {"scope": "session", "trust": "in", "channel": "session"},
                "target": {"kind": "none"},
                "reason": "uncovered operation",
                "future_field": []
            }]
        }))
        .unwrap();
        assert_eq!(denied.denied[0].target, ObservedTarget::None);
    }

    #[test]
    fn admission_policy_default_is_fail_closed() {
        let policy = CapabilityAdmissionPolicy::default();
        assert!(
            policy.enabled_tools.is_empty(),
            "no tool enabled by default"
        );
        assert!(policy.denied_capabilities.is_empty());
        // Boundary fail-closed is `in`, the opposite of scope-trust default.
        assert_eq!(policy.max_boundary, Trust::In);
        assert_eq!(Trust::default(), Trust::Ex);
    }

    #[test]
    fn admission_policy_round_trips() {
        let policy = CapabilityAdmissionPolicy {
            enabled_tools: BTreeSet::from(["noop.echo".to_string()]),
            denied_capabilities: BTreeSet::from(["session.events".to_string()]),
            max_boundary: Trust::Ex,
        };
        let parsed: CapabilityAdmissionPolicy =
            serde_json::from_str(&serde_json::to_string(&policy).unwrap()).unwrap();
        assert_eq!(parsed, policy);
    }
}
