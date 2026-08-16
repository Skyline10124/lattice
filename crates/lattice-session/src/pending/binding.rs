// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! The exact ordered binding plan published atomically as `BindingPlanned`
//! (ADR 0005 §9, GitHub #144).
//!
//! A valid Tool-bearing model output is written durably in one step as
//! `BindingPlanned` together with the embedded complete unbound message:
//! there is no durable window containing a Tool-bearing `MessageComplete`
//! without a binding plan. `PlanKey` is structural (`{TurnId,
//! model_step_ordinal}`); there is no random `PlanId` and no second
//! aggregate batch digest — the exact ordered bindings are the idempotency
//! input. Provider origin, provider call IDs, and opaque replay state are
//! conversation metadata inside the embedded message and never enter a
//! binding tuple.

use sha2::{Digest, Sha256};

use lancet_protocol::{InvocationId, PlanKey, ToolCallAddress};
use lattice_conversation::{NativeMessage, UnboundToolCall, arguments_digest};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{Result, SessionError};

const BINDING_DOMAIN: &[u8] = b"lattice-invocation-binding-digest-v1\0";

/// One planned binding: the exact reservation tuple of one invocation
/// (ADR 0005 §11): identity, structural address, method, descriptor
/// identity, and canonical arguments digest — plus the per-binding digest
/// over that tuple. No provider metadata, no workspace-history OIDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedBinding {
    /// Candidate invocation identity generated after the valid complete
    /// message and before reservation.
    pub invocation_id: InvocationId,
    /// Structural pre-commit address of this call's part.
    pub address: ToolCallAddress,
    /// Tool method name.
    pub method: String,
    /// Descriptor identity (digest hex) of the Tool.
    pub descriptor_identity: String,
    /// SHA-256 over the domain separator, explicit byte length, and the
    /// canonical argument bytes (recomputed from the embedded message's
    /// ToolCall part on validation).
    #[serde(with = "super::hex_digest")]
    pub arguments_digest: [u8; 32],
    /// SHA-256 over the exact binding tuple; the per-binding idempotency
    /// input. There is no aggregate batch digest.
    #[serde(with = "super::hex_digest")]
    pub binding_digest: [u8; 32],
}

/// Compute the per-binding digest over the exact reservation tuple
/// (ADR 0005 §11): identity, structural address, method, descriptor
/// identity, and canonical arguments digest.
pub fn binding_digest(
    invocation_id: &InvocationId,
    address: &ToolCallAddress,
    method: &str,
    descriptor_identity: &str,
    arguments_digest: &[u8; 32],
) -> [u8; 32] {
    let digest_hex: String = arguments_digest.iter().map(|b| format!("{b:02x}")).collect();
    let payload = json!({
        "invocation_id": invocation_id.to_string(),
        "session_id": address.message.session_id.to_string(),
        "turn_id": address.message.turn_id.to_string(),
        "model_step_ordinal": address.message.model_step_ordinal,
        "part_ordinal": address.part_ordinal,
        "method": method,
        "descriptor_identity": descriptor_identity,
        "arguments_digest": digest_hex,
    });
    let canonical = lancet_protocol::canonical_json(&payload);
    let mut hasher = Sha256::new();
    hasher.update(BINDING_DOMAIN);
    hasher.update((canonical.len() as u64).to_be_bytes());
    hasher.update(canonical.as_bytes());
    hasher.finalize().into()
}

impl PlannedBinding {
    /// Compute the per-binding digest over the exact reservation tuple.
    pub fn compute_binding_digest(
        invocation_id: &InvocationId,
        address: &ToolCallAddress,
        method: &str,
        descriptor_identity: &str,
        arguments_digest: &[u8; 32],
    ) -> [u8; 32] {
        binding_digest(
            invocation_id,
            address,
            method,
            descriptor_identity,
            arguments_digest,
        )
    }

    /// Validate that the recorded digest describes this exact tuple.
    pub fn validate(&self) -> Result<()> {
        let expected = Self::compute_binding_digest(
            &self.invocation_id,
            &self.address,
            &self.method,
            &self.descriptor_identity,
            &self.arguments_digest,
        );
        if expected != self.binding_digest {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "binding digest does not describe the binding tuple of invocation {}",
                    self.invocation_id
                ),
            });
        }
        Ok(())
    }
}

/// The binding plan of one model step of one Turn: structural `PlanKey`, the
/// embedded complete unbound message, and the ordered bindings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BindingPlan {
    /// Structural plan identity; no random `PlanId`.
    pub plan_key: PlanKey,
    /// The complete valid Tool-bearing message the plan was generated from.
    pub message: NativeMessage<UnboundToolCall>,
    /// Ordered bindings; `bindings[i].address.part_ordinal` is strictly
    /// ascending and every binding matches the ToolCall part at its ordinal.
    pub bindings: Vec<PlannedBinding>,
}

impl BindingPlan {
    /// Validate the plan against its own embedded message:
    ///
    /// - bindings are non-empty and ordered by strictly ascending part
    ///   ordinal;
    /// - every address belongs to the plan's session/turn/step;
    /// - every binding matches the ToolCall part at its ordinal (method and
    ///   canonical arguments digest);
    /// - every ToolCall part of the message has exactly one binding (a
    ///   Tool-bearing plan binds the whole batch);
    /// - every per-binding digest describes its tuple.
    pub fn validate(&self) -> Result<()> {
        if self.bindings.is_empty() {
            return Err(SessionError::JournalCorruption {
                detail: "binding plan must carry at least one binding".to_string(),
            });
        }
        let mut last_ordinal = None;
        for binding in &self.bindings {
            let ordinal = binding.address.part_ordinal;
            if let Some(previous) = last_ordinal && ordinal <= previous {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "binding part ordinals must strictly ascend: {ordinal} after {previous}"
                    ),
                });
            }
            last_ordinal = Some(ordinal);
            if binding.address.message.turn_id != self.plan_key.turn_id
                || binding.address.message.model_step_ordinal != self.plan_key.model_step_ordinal
            {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "binding address does not belong to plan {}:{}",
                        self.plan_key.turn_id, self.plan_key.model_step_ordinal
                    ),
                });
            }
            binding.validate()?;
            let part = self
                .message
                .parts
                .get(ordinal as usize)
                .ok_or_else(|| SessionError::JournalCorruption {
                    detail: format!("binding points past the message parts: {ordinal}"),
                })?;
            let lattice_conversation::MessageSemantic::ToolCall { call } = &part.semantic else {
                return Err(SessionError::JournalCorruption {
                    detail: format!("binding points at a non-ToolCall part: {ordinal}"),
                });
            };
            if call.name != binding.method {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "binding method '{}' differs from part method '{}'",
                        binding.method, call.name
                    ),
                });
            }
            if arguments_digest(&call.arguments) != binding.arguments_digest {
                return Err(SessionError::JournalCorruption {
                    detail: format!(
                        "binding arguments digest does not describe part {ordinal}"
                    ),
                });
            }
        }
        let call_count = self.message.tool_calls().count();
        if call_count != self.bindings.len() {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "plan binds {} of {} ToolCall parts; a Tool-bearing plan binds the whole batch",
                    self.bindings.len(),
                    call_count
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::{ModelMessageAddress, SessionId, TurnId};
    use lattice_conversation::{
        MessagePart, MessageRole, MessageSemantic, ProviderOrigin,
    };

    fn turn_id() -> TurnId {
        TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap()
    }

    fn session_id() -> SessionId {
        SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap()
    }

    fn invocation_id(n: u8) -> InvocationId {
        InvocationId::parse(&format!(
            "00000000-0000-4000-8000-{:012x}",
            n as u64
        ))
        .unwrap()
    }

    fn tool_message(calls: &[(&str, serde_json::Value)]) -> NativeMessage<UnboundToolCall> {
        NativeMessage::new(
            MessageRole::Assistant,
            Some(ProviderOrigin::Anthropic),
            calls
                .iter()
                .map(|(name, arguments)| {
                    MessagePart::semantic_only(MessageSemantic::ToolCall {
                        call: UnboundToolCall {
                            provider_call_id: Some(format!("call-{}", name)),
                            name: name.to_string(),
                            arguments: arguments.clone(),
                        },
                    })
                })
                .collect(),
        )
    }

    fn binding(part_ordinal: u32, name: &str, arguments: &serde_json::Value) -> PlannedBinding {
        let invocation = invocation_id(part_ordinal as u8);
        let address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id: session_id(),
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            part_ordinal,
        };
        let args_digest = arguments_digest(arguments);
        PlannedBinding {
            invocation_id: invocation,
            binding_digest: PlannedBinding::compute_binding_digest(
                &invocation,
                &address,
                name,
                "descriptor-identity",
                &args_digest,
            ),
            address,
            method: name.to_string(),
            descriptor_identity: "descriptor-identity".to_string(),
            arguments_digest: args_digest,
        }
    }

    fn plan(bindings: Vec<PlannedBinding>, message: NativeMessage<UnboundToolCall>) -> BindingPlan {
        BindingPlan {
            plan_key: PlanKey {
                turn_id: turn_id(),
                model_step_ordinal: 0,
            },
            message,
            bindings,
        }
    }

    #[test]
    fn exact_plan_validates_and_round_trips() {
        let arguments = serde_json::json!({"path": "src/lib.rs"});
        let message = tool_message(&[("mutate.edit", arguments.clone())]);
        let exact = plan(vec![binding(0, "mutate.edit", &arguments)], message);
        exact.validate().expect("exact plan");
        let back: BindingPlan =
            serde_json::from_value(serde_json::to_value(&exact).unwrap()).unwrap();
        assert_eq!(back, exact);
        back.validate().expect("round-tripped plan");
    }

    #[test]
    fn plan_rejects_partial_binding_of_the_batch() {
        let a = serde_json::json!({"a": 1});
        let b = serde_json::json!({"b": 2});
        let message = tool_message(&[("t.one", a.clone()), ("t.two", b.clone())]);
        let partial = plan(
            vec![binding(0, "t.one", &a)],
            message,
        );
        assert!(matches!(
            partial.validate(),
            Err(SessionError::JournalCorruption { .. })
        ));
    }

    #[test]
    fn plan_rejects_mismatched_arguments_digest() {
        let stored = serde_json::json!({"path": "a"});
        let actual = serde_json::json!({"path": "b"});
        let message = tool_message(&[("mutate.edit", actual)]);
        let mut lying = binding(0, "mutate.edit", &stored);
        // Recompute the tuple digest so only the arguments digest lies.
        lying.binding_digest = PlannedBinding::compute_binding_digest(
            &lying.invocation_id,
            &lying.address,
            &lying.method,
            &lying.descriptor_identity,
            &lying.arguments_digest,
        );
        let bad = plan(vec![lying], message);
        assert!(matches!(
            bad.validate(),
            Err(SessionError::JournalCorruption { .. })
        ));
    }

    #[test]
    fn plan_rejects_non_ascending_or_wrong_step_addresses() {
        let arguments = serde_json::json!({});
        let message = tool_message(&[("t", arguments.clone()), ("t2", arguments.clone())]);
        let unordered = plan(
            vec![
                binding(1, "t2", &arguments),
                binding(0, "t", &arguments),
            ],
            message.clone(),
        );
        assert!(unordered.validate().is_err());

        let mut wrong_step = binding(0, "t", &arguments);
        wrong_step.address.message.model_step_ordinal = 7;
        wrong_step.binding_digest = PlannedBinding::compute_binding_digest(
            &wrong_step.invocation_id,
            &wrong_step.address,
            &wrong_step.method,
            &wrong_step.descriptor_identity,
            &wrong_step.arguments_digest,
        );
        let cross_step = plan(vec![wrong_step], message);
        assert!(cross_step.validate().is_err());
    }

    #[test]
    fn binding_digest_is_deterministic_and_input_sensitive() {
        let invocation = invocation_id(0);
        let address = ToolCallAddress {
            message: ModelMessageAddress {
                session_id: session_id(),
                turn_id: turn_id(),
                model_step_ordinal: 2,
            },
            part_ordinal: 3,
        };
        let digest = |method: &str| {
            PlannedBinding::compute_binding_digest(
                &invocation,
                &address,
                method,
                "d",
                &[1u8; 32],
            )
        };
        assert_eq!(digest("a"), digest("a"));
        assert_ne!(digest("a"), digest("b"));
    }
}
