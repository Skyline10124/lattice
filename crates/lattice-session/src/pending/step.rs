// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Ordered step closing (ADR 0005 §9, GitHub #144).
//!
//! `StepClosing` is one stage represented by successive immutable snapshots:
//! the complete ordered bindings plus a monotonic closure cursor. Only the
//! lowest unclosed ordinal may advance, the current binding's exact
//! `ResultDraft` candidate is recorded before draft publication, and later
//! bindings that will not run receive ordered `NotExecuted` closures.
//! `StepClosed` is published only when every binding is closed.

use lancet_protocol::PlanKey;
use lattice_conversation::{NativeMessage, ToolResultOutcome, UnboundToolCall};
use serde::{Deserialize, Serialize};

use super::binding::PlannedBinding;
use super::draft::ResultDraft;
use crate::{Result, SessionError};

/// Why model-visible result material is available or unavailable
/// (ADR 0005 §11/§12): execution outcome and result material are
/// independent — a successful execution can have unavailable material
/// without being relabeled as an execution failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialAvailability {
    /// Durable material exists, identified by digest and length.
    Available {
        /// Digest of the canonical ToolContent.
        #[serde(with = "super::hex_digest")]
        digest: [u8; 32],
        /// Canonical byte length.
        byte_length: u64,
    },
    /// No model-visible material; the outcome remains the true execution
    /// outcome.
    Unavailable {
        /// Typed unavailability reason.
        reason: super::facts::UnavailableMaterialReason,
        /// Digest of the full content, when it was computed.
        #[serde(default, with = "super::hex_digest_option")]
        digest: Option<[u8; 32]>,
        /// Full content byte length, when known.
        #[serde(default)]
        byte_length: Option<u64>,
    },
}

/// One closed binding: the binding, its conversation closure outcome, and
/// its material availability. `StepClosed` has no independent cumulative
/// usage authority (ADR 0005 §10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClosedBinding {
    /// The closed binding.
    pub binding: PlannedBinding,
    /// The conversation closure outcome for the ToolResult part.
    pub outcome: ToolResultOutcome,
    /// Material availability for the model-visible content.
    pub material: MaterialAvailability,
}

/// The `StepClosing` snapshot state (ADR 0005 §9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepClosingState {
    /// The plan being closed.
    pub plan_key: PlanKey,
    /// The complete ordered bindings of the plan.
    pub bindings: Vec<PlannedBinding>,
    /// Monotonic closure cursor: bindings at ordinals `< closure_cursor`
    /// are durably closed; `closures.len() == closure_cursor`.
    pub closure_cursor: u32,
    /// Closures in ordinal order, one per closed binding.
    pub closures: Vec<ClosedBinding>,
    /// The current (lowest unclosed) binding's exact `ResultDraft`
    /// candidate, pinned before draft publication.
    pub current_candidate: Option<ResultDraft>,
}

impl StepClosingState {
    /// Validate the cursor state: plan consistency, closure count, closure
    /// ordinals in order, candidate points at the unclosed binding, and
    /// every closure's binding matches the plan's binding at that ordinal.
    pub fn validate(&self) -> Result<()> {
        if self.closures.len() != self.closure_cursor as usize {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "closure cursor {} does not match {} recorded closures",
                    self.closure_cursor,
                    self.closures.len()
                ),
            });
        }
        if self.closure_cursor > self.bindings.len() as u32 {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "closure cursor {} exceeds the {} bindings of the plan",
                    self.closure_cursor,
                    self.bindings.len()
                ),
            });
        }
        for (index, closure) in self.closures.iter().enumerate() {
            if closure.binding != self.bindings[index] {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "closure {index} does not close the plan binding at that ordinal"
                    ),
                });
            }
            closure.binding.validate()?;
        }
        if let Some(candidate) = &self.current_candidate {
            let index = self.closure_cursor as usize;
            let Some(binding) = self.bindings.get(index) else {
                return Err(SessionError::JournalCorruption {
                    detail: "candidate pinned with no unclosed binding remaining".to_string(),
                });
            };
            if candidate.invocation_id != binding.invocation_id {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "candidate invocation {} is not the unclosed binding {}",
                        candidate.invocation_id, binding.invocation_id
                    ),
                });
            }
            candidate.validate()?;
        }
        Ok(())
    }

    /// Whether every binding is closed (the precondition for `StepClosed`).
    pub fn all_closed(&self) -> bool {
        self.closure_cursor == self.bindings.len() as u32
    }
}

/// The closed `StepClosed` sum (ADR 0005 §14). Only a live owner may advance
/// `ContinueModel` to another provider attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepClosedKind {
    /// A complete zero-Tool message ends the Turn using the already durable
    /// message.
    FinishTurn {
        /// The durable complete final message.
        final_message: NativeMessage<UnboundToolCall>,
    },
    /// The step's Tool results ask for another model step. At reopen this
        /// never continues: the Turn closes interrupted.
    ContinueModel {
        /// Ordered closures of the step's bindings.
        ordered_tool_results: Vec<ClosedBinding>,
        /// Semantic digest of the step's conversation extension.
        #[serde(with = "super::hex_digest")]
        conversation_digest: [u8; 32],
    },
    /// The step (and Turn) failed with a typed reason.
    FailTurn {
        /// Typed failure reason.
        reason: super::outcome::TurnFailure,
    },
    /// The step (and Turn) was cancelled with a typed reason.
    CancelTurn {
        /// Typed cancellation reason.
        reason: super::outcome::TurnCancellation,
    },
    /// The step (and Turn) was interrupted with a typed reason.
    InterruptTurn {
        /// Typed interruption reason.
        reason: super::outcome::TurnInterruption,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::binding::PlannedBinding;
    use lancet_protocol::{ModelMessageAddress, SessionId, TurnId};
    use lattice_conversation::arguments_digest;

    fn turn_id() -> TurnId {
        TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap()
    }

    fn binding(ordinal: u32) -> PlannedBinding {
        let invocation =
            lancet_protocol::InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
                .unwrap();
        let address = lancet_protocol::ToolCallAddress {
            message: ModelMessageAddress {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            part_ordinal: ordinal,
        };
        let args = serde_json::json!({});
        let args_digest = arguments_digest(&args);
        PlannedBinding {
            binding_digest: PlannedBinding::compute_binding_digest(
                &invocation,
                &address,
                "t",
                "d",
                &args_digest,
            ),
            invocation_id: invocation,
            address,
            method: "t".to_string(),
            descriptor_identity: "d".to_string(),
            arguments_digest: args_digest,
        }
    }

    fn closure(binding: &PlannedBinding) -> ClosedBinding {
        ClosedBinding {
            binding: binding.clone(),
            outcome: ToolResultOutcome::NotExecuted,
            material: MaterialAvailability::Unavailable {
                reason: super::super::facts::UnavailableMaterialReason::DraftPublicationFailed,
                digest: None,
                byte_length: None,
            },
        }
    }

    fn candidate(invocation: lancet_protocol::InvocationId) -> ResultDraft {
        ResultDraft::new(
            invocation,
            binding(0).binding_digest,
            turn_id(),
            ModelMessageAddress {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            0,
            "{}".to_string(),
        )
        .expect("candidate")
    }

    #[test]
    fn cursor_state_validates_ordered_closures() {
        let bindings = vec![binding(0), binding(1)];
        let mut state = StepClosingState {
            plan_key: PlanKey {
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            bindings: bindings.clone(),
            closure_cursor: 1,
            closures: vec![closure(&bindings[0])],
            current_candidate: None,
        };
        state.validate().expect("one closed in order");
        assert!(!state.all_closed());

        state.closures = vec![closure(&bindings[1])];
        assert!(state.validate().is_err(), "closure must match the ordinal");

        state.closures = vec![closure(&bindings[0])];
        state.closure_cursor = 2;
        assert!(state.validate().is_err(), "cursor must equal closure count");

        state.closure_cursor = 2;
        state.closures = vec![closure(&bindings[0]), closure(&bindings[1])];
        state.validate().expect("all closed");
        assert!(state.all_closed());
    }

    #[test]
    fn candidate_must_point_at_the_unclosed_binding() {
        let bindings = vec![binding(0), binding(1)];
        let mut state = StepClosingState {
            plan_key: PlanKey {
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            bindings: bindings.clone(),
            closure_cursor: 0,
            closures: vec![],
            current_candidate: Some(candidate(bindings[1].invocation_id)),
        };
        assert!(
            state.validate().is_err(),
            "candidate must be the lowest unclosed binding"
        );
        state.current_candidate = Some(candidate(bindings[0].invocation_id));
        state.validate().expect("candidate matches binding 0");

        state.closure_cursor = 2;
        state.closures = vec![closure(&bindings[0]), closure(&bindings[1])];
        state.current_candidate = Some(candidate(bindings[0].invocation_id));
        assert!(
            state.validate().is_err(),
            "no candidate may remain when every binding is closed"
        );
    }
}
