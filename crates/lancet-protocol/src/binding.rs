// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Caller-supplied invocation binding facts (ADR 0005 §1/§11).
//!
//! A JSON-RPC request never allocates an invocation. It carries the exact
//! immutable binding that the Turn coordinator already registered as an
//! invocation slot of the current Step in the Session store (ADR 0006). Before
//! execution, LANCET checks that the request method and canonical arguments
//! match this binding and that the descriptor identity is in the frozen catalog.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    InvocationId, ModelMessageAddress, SessionId, ToolCallAddress, TurnId, canonical_json,
};

const ARGUMENTS_DOMAIN: &[u8] = b"lattice-tool-arguments-digest-v1\0";

/// Immutable non-secret identity of one registered Tool binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindingIdentity {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub model_step_ordinal: u32,
    pub part_ordinal: u32,
    /// Tool method selected by the model.
    pub method: String,
    /// Digest-pinned descriptor identity from the frozen Tool catalog.
    pub descriptor_identity: String,
    /// Canonical digest of the complete Tool argument object.
    #[serde(with = "hex_digest")]
    #[schemars(with = "String")]
    pub arguments_digest: [u8; 32],
}

impl BindingIdentity {
    /// Reconstruct the canonical structural ToolCall address.
    #[must_use]
    pub fn address(&self) -> ToolCallAddress {
        ToolCallAddress {
            message: ModelMessageAddress {
                session_id: self.session_id,
                turn_id: self.turn_id,
                model_step_ordinal: self.model_step_ordinal,
            },
            part_ordinal: self.part_ordinal,
        }
    }
}

/// Complete caller-supplied request identity and its durable binding facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvocationBinding {
    /// Canonical JSON-RPC request identity.
    pub invocation_id: InvocationId,
    /// Facts that must exactly match the durable invocation slot.
    pub identity: BindingIdentity,
}

impl InvocationBinding {
    /// Validate the request method and canonical arguments against this binding.
    pub fn matches_request(&self, method: &str, arguments: &Value) -> bool {
        self.identity.method == method
            && self.identity.arguments_digest == arguments_digest(arguments)
    }
}

/// SHA-256 over the fixed domain separator, explicit byte length, and canonical
/// JSON bytes of a Tool arguments object.
pub fn arguments_digest(arguments: &Value) -> [u8; 32] {
    let bytes = canonical_json(arguments);
    let mut hasher = Sha256::new();
    hasher.update(ARGUMENTS_DOMAIN);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes.as_bytes());
    hasher.finalize().into()
}

mod hex_digest {
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub fn serialize<S: Serializer>(digest: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        serializer.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw.len() != 64 || !raw.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(D::Error::custom(
                "arguments_digest must be exactly 64 hexadecimal characters",
            ));
        }
        let mut digest = [0u8; 32];
        for (index, slot) in digest.iter_mut().enumerate() {
            *slot =
                u8::from_str_radix(&raw[index * 2..index * 2 + 2], 16).map_err(D::Error::custom)?;
        }
        Ok(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding() -> InvocationBinding {
        let arguments = json!({"path": "a.txt"});
        InvocationBinding {
            invocation_id: InvocationId::parse("11111111-1111-4111-8111-111111111111")
                .expect("invocation"),
            identity: BindingIdentity {
                session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").expect("session"),
                turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").expect("turn"),
                model_step_ordinal: 2,
                part_ordinal: 1,
                method: "observe.workspace".to_string(),
                descriptor_identity: "d".repeat(64),
                arguments_digest: arguments_digest(&arguments),
            },
        }
    }

    #[test]
    fn binding_round_trips_and_validates_request() {
        let binding = binding();
        let json = serde_json::to_string(&binding).expect("serialize");
        let decoded: InvocationBinding = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, binding);
        assert_eq!(
            binding.identity.address(),
            ToolCallAddress {
                message: ModelMessageAddress {
                    session_id: binding.identity.session_id,
                    turn_id: binding.identity.turn_id,
                    model_step_ordinal: 2,
                },
                part_ordinal: 1,
            }
        );
        assert!(binding.matches_request("observe.workspace", &json!({"path": "a.txt"})));
        assert!(!binding.matches_request("observe.workspace", &json!({"path": "b.txt"})));
        assert!(!binding.matches_request("mutate.create", &json!({"path": "a.txt"})));
    }

    #[test]
    fn arguments_digest_preserves_existing_golden_construction() {
        let digest = arguments_digest(&json!({"path": "a.txt"}));
        let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(
            hex,
            "5fd582703776c899a2ecb12f3d699bd7244dff1f443ef5ae3f0067bcf3ce279e"
        );
    }
}
