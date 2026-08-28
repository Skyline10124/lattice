// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Internal Cell-evidence receipt triple (ADR 0005 §11/§12, GitHub #139-B).
//!
//! Data-only bundle of the three versioned Cell evidence roots returned to
//! the Turn coordinator after a bound Tool execution. This is an internal
//! composition receipt, not a provider-facing Tool result field.

use serde::{Deserialize, Serialize};

use crate::evidence::{AdmissionEvidence, DeniedOperationsRecord, ObservedEffectsRecord};

/// Content-free Cell evidence published with an invocation terminal.
///
/// Mirrors the three roots of [`crate::evidence`] so higher layers can pin
/// a terminal without reading SQLite or inventing empty placeholders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellEvidenceTriple {
    /// Why the call was authorized (or refused).
    pub admission: AdmissionEvidence,
    /// What happened (host-observed effects only).
    pub effects: ObservedEffectsRecord,
    /// What was refused before a host facility ran.
    pub denied: DeniedOperationsRecord,
}

impl CellEvidenceTriple {
    /// Build from the three versioned roots.
    pub fn new(
        admission: AdmissionEvidence,
        effects: ObservedEffectsRecord,
        denied: DeniedOperationsRecord,
    ) -> Self {
        Self {
            admission,
            effects,
            denied,
        }
    }

    /// Minimal typed non-secret evidence for a call refused before descriptor
    /// admission. Carries only the Tool name and empty versioned roots.
    pub fn refused_before_admission(tool: impl Into<String>) -> Self {
        Self {
            admission: AdmissionEvidence {
                version: crate::evidence::ADMISSION_EVIDENCE_VERSION,
                tool: tool.into(),
                approved_capabilities: Vec::new(),
                denied_capabilities: Vec::new(),
                boundary_excluded: Vec::new(),
                max_boundary: crate::cell::Trust::In,
            },
            effects: ObservedEffectsRecord {
                version: crate::evidence::OBSERVED_EFFECTS_RECORD_VERSION,
                effects: Vec::new(),
            },
            denied: DeniedOperationsRecord {
                version: crate::evidence::DENIED_OPERATIONS_RECORD_VERSION,
                denied: Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_admission_refusal_triple_is_content_free_and_round_trips() {
        let triple = CellEvidenceTriple::refused_before_admission("unknown.example");
        assert_eq!(triple.admission.tool, "unknown.example");
        assert!(triple.effects.effects.is_empty());
        assert!(triple.denied.denied.is_empty());
        let json = serde_json::to_string(&triple).expect("serialize");
        let back: CellEvidenceTriple = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, triple);
    }
}
