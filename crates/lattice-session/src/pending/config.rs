// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Frozen request-acceptance facts published by `TurnStarted`
//! (ADR 0005 §8, GitHub #144).
//!
//! Once `TurnStarted` is published the request is accepted: every later
//! failure, cancellation, or crash closes the Turn rather than erasing it.
//! The snapshot freezes the parent conversation tip, the user message, the
//! explicit `TurnId`, the effective profile/policy/model/system/catalog
//! selection, and the execution limits. It structurally contains no
//! credentials: the frozen types have no credential-bearing fields, and the
//! canonical snapshot JSON is additionally checked against a deterministic
//! exact-key denylist so an operator putting a secret under a recognized
//! credential key fails closed at publication, not at first leak.

use sha2::{Digest, Sha256};

use lancet_protocol::{SessionId, TurnId};
use lattice_conversation::{NativeMessage, UnboundToolCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, SessionError};

/// Code-defined model-step safety limit; not profile-configurable
/// (ADR 0005 §10).
pub const MAX_MODEL_STEPS: u32 = 32;

/// Canonical catalog snapshot byte limit (1 MiB, ADR 0005 §8). A snapshot of
/// exactly this length is legal; one byte more fails closed.
pub const MAX_CATALOG_SNAPSHOT_BYTES: usize = 1024 * 1024;

/// User input byte bound, aligned with the assistant content bound of one
/// attempt (`lattice-conversation` limits, ADR 0005 §10). Counted as UTF-8
/// bytes of the user message's text parts.
pub const MAX_USER_INPUT_BYTES: usize = 2 * 1024 * 1024;

/// Exact credential key denylist checked against every object key of the
/// canonical profile/policy snapshot, at any depth. Exact lowercase match
/// only; values are never inspected (a value that merely looks like a secret
/// is not a credential key).
const CREDENTIAL_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "secret",
    "secret_key",
    "password",
    "token",
    "access_token",
    "refresh_token",
    "credential",
    "credentials",
    "private_key",
];

const CATALOG_DOMAIN: &[u8] = b"lattice-tool-catalog-snapshot-v1\0";
const PROFILE_DOMAIN: &[u8] = b"lattice-frozen-config-digest-v1\0";

fn digest_with_domain(domain: &[u8], canonical_bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update((canonical_bytes.len() as u64).to_be_bytes());
    hasher.update(canonical_bytes);
    hasher.finalize().into()
}

/// Frozen execution limits (ADR 0005 §8/§10). The Turn deadline itself is a
/// live monotonic construction over `timeout_secs` and is never persisted;
/// reopen does not reconstruct remaining time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnExecutionLimits {
    /// Frozen provider/Tool deadline in seconds.
    pub timeout_secs: u64,
    /// Recorded model-step limit; must equal [`MAX_MODEL_STEPS`] (it is
    /// code-defined, not profile-configurable).
    pub max_model_steps: u32,
    /// Frozen per-step provider attempt (retry) budget.
    pub max_model_attempts: u32,
}

impl TurnExecutionLimits {
    /// Validate the frozen limits: the model-step limit is the code-defined
    /// constant and the attempt budget is at least one.
    pub fn validate(&self) -> Result<()> {
        if self.max_model_steps != MAX_MODEL_STEPS {
            return Err(SessionError::InvalidInput(format!(
                "max_model_steps must be the code-defined {MAX_MODEL_STEPS}, got {}",
                self.max_model_steps
            )));
        }
        if self.max_model_attempts == 0 {
            return Err(SessionError::InvalidInput(
                "max_model_attempts must be at least 1".to_string(),
            ));
        }
        Ok(())
    }
}

/// One enabled Tool in the frozen catalog snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCatalogEntry {
    /// Tool method name.
    pub name: String,
    /// Descriptor identity (digest hex) of the embedded component.
    pub descriptor_identity: String,
    /// Provider-facing description.
    pub description: String,
    /// Canonical JSON input schema.
    pub input_schema: Value,
}

/// Content-addressed provider-neutral snapshot of the enabled Tool catalog
/// (ADR 0005 §8): ordered names, descriptor identities, descriptions,
/// canonical input schemas, and the runtime build identity, plus its digest.
/// Provider wire projection is not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCatalogSnapshot {
    /// Runtime build identity of the closed Tool catalog.
    pub runtime_build_id: String,
    /// Ordered entries.
    pub entries: Vec<ToolCatalogEntry>,
    /// SHA-256 over the domain separator, explicit byte length, and the
    /// canonical JSON bytes of the entry list.
    #[serde(with = "super::hex_digest")]
    pub digest: [u8; 32],
}

impl ToolCatalogSnapshot {
    /// Build and validate a snapshot: names must be non-empty and unique,
    /// and the canonical bytes must fit the 1 MiB protocol limit (exact
    /// boundary legal, plus one rejected).
    pub fn new(runtime_build_id: String, mut entries: Vec<ToolCatalogEntry>) -> Result<Self> {
        for entry in &entries {
            if entry.name.is_empty() {
                return Err(SessionError::InvalidInput(
                    "catalog entry name must not be empty".to_string(),
                ));
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        if entries.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(SessionError::InvalidInput(
                "catalog entry names must be unique".to_string(),
            ));
        }
        let (canonical, digest) = Self::canonicalize(&runtime_build_id, &entries)?;
        if canonical.len() > MAX_CATALOG_SNAPSHOT_BYTES {
            return Err(SessionError::InvalidInput(format!(
                "catalog snapshot canonical bytes exceed the 1 MiB limit: {} > {}",
                canonical.len(),
                MAX_CATALOG_SNAPSHOT_BYTES
            )));
        }
        Ok(Self {
            runtime_build_id,
            entries,
            digest,
        })
    }

    /// Validate a deserialized snapshot: the recorded digest must describe
    /// the recorded entries and the byte limit must hold.
    pub fn validate(&self) -> Result<()> {
        let (canonical, digest) = Self::canonicalize(&self.runtime_build_id, &self.entries)?;
        if canonical.len() > MAX_CATALOG_SNAPSHOT_BYTES {
            return Err(SessionError::JournalCorruption {
                detail: format!(
                    "catalog snapshot canonical bytes exceed the 1 MiB limit: {}",
                    canonical.len()
                ),
            });
        }
        if digest != self.digest {
            return Err(SessionError::JournalCorruption {
                detail: "catalog snapshot digest does not describe its entries".to_string(),
            });
        }
        Ok(())
    }

    fn canonicalize(
        runtime_build_id: &str,
        entries: &[ToolCatalogEntry],
    ) -> Result<(String, [u8; 32])> {
        let value = serde_json::to_value((runtime_build_id, entries))
            .map_err(|e| SessionError::InvalidInput(format!("serialize catalog: {e}")))?;
        let canonical = lancet_protocol::canonical_json(&value);
        let digest = digest_with_domain(CATALOG_DOMAIN, canonical.as_bytes());
        Ok((canonical, digest))
    }
}

/// The complete frozen effective configuration one accepted Turn runs under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenTurnConfig {
    /// Agent display name for the TurnCommit projection.
    pub agent_name: String,
    /// Effective model id.
    pub model: String,
    /// Effective provider id.
    pub provider: String,
    /// Effective system prompt, when one is configured.
    pub system_prompt: Option<String>,
    /// Canonical JSON of the frozen effective profile snapshot; carries no
    /// credential keys (checked at construction).
    pub profile_snapshot_json: String,
    /// Canonical JSON of the frozen effective policy snapshot; carries no
    /// credential keys (checked at construction).
    pub policy_snapshot_json: String,
    /// Frozen enabled Tool catalog.
    pub catalog: ToolCatalogSnapshot,
    /// Frozen execution limits.
    pub limits: TurnExecutionLimits,
}

impl FrozenTurnConfig {
    /// Digest over the canonical bytes of the whole frozen configuration
    /// (profile, policy, model selection, system prompt, catalog, limits).
    pub fn digest(&self) -> Result<[u8; 32]> {
        let value = serde_json::to_value(self)
            .map_err(|e| SessionError::InvalidInput(format!("serialize frozen config: {e}")))?;
        let canonical = lancet_protocol::canonical_json(&value);
        Ok(digest_with_domain(PROFILE_DOMAIN, canonical.as_bytes()))
    }

    /// Validate a deserialized configuration: limits match the code-defined
    /// bound, the catalog snapshot is self-consistent, the profile/policy
    /// snapshots are canonical JSON without credential keys, and every
    /// string field is non-empty where required.
    pub fn validate(&self) -> Result<()> {
        if self.agent_name.is_empty() || self.model.is_empty() || self.provider.is_empty() {
            return Err(SessionError::JournalCorruption {
                detail: "frozen config agent/model/provider must not be empty".to_string(),
            });
        }
        self.limits.validate().map_err(|e| {
            SessionError::JournalCorruption {
                detail: format!("frozen limits invalid: {e}"),
            }
        })?;
        self.catalog.validate()?;
        check_no_credential_keys(&self.profile_snapshot_json, "profile")?;
        check_no_credential_keys(&self.policy_snapshot_json, "policy")?;
        Ok(())
    }
}

/// Parse canonical JSON and fail closed on any exact credential key at any
/// object depth.
fn check_no_credential_keys(json: &str, kind: &str) -> Result<Value> {
    let value: Value = serde_json::from_str(json).map_err(|e| {
        SessionError::InvalidInput(format!("{kind} snapshot is not valid JSON: {e}"))
    })?;
    if let Some(key) = find_credential_key(&value) {
        return Err(SessionError::InvalidInput(format!(
            "{kind} snapshot carries credential key '{key}'"
        )));
    }
    Ok(value)
}

fn find_credential_key(value: &Value) -> Option<&'static str> {
    match value {
        Value::Object(map) => map.iter().find_map(|(key, inner)| {
            CREDENTIAL_KEYS
                .iter()
                .find(|candidate| key.eq_ignore_ascii_case(candidate))
                .map(|candidate| {
                    let _ = key;
                    *candidate
                })
                .or_else(|| find_credential_key(inner))
        }),
        Value::Array(items) => items.iter().find_map(find_credential_key),
        _ => None,
    }
}

/// The complete facts published atomically by `TurnStarted`
/// (ADR 0005 §8/§9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnStartedFacts {
    /// The accepted Turn's explicit identity; never derived late.
    pub turn_id: TurnId,
    /// Owning session identity.
    pub session_id: SessionId,
    /// Conversation tip (commit oid hex) the Turn started from; `None` for
    /// the first Turn of an empty branch. The final publication transaction
    /// CASes the conversation head against exactly this value.
    pub parent_conversation_tip: Option<String>,
    /// The accepted user message. Role must be `User` with no ToolCall or
    /// ToolResult parts and bounded text.
    pub user_message: NativeMessage<UnboundToolCall>,
    /// Frozen effective configuration.
    pub config: FrozenTurnConfig,
}

impl TurnStartedFacts {
    /// Validate the frozen facts before publication: identity presence,
    /// parent tip shape, user message role/shape/size, and the whole frozen
    /// configuration.
    pub fn validate(&self) -> Result<()> {
        if self.parent_conversation_tip.as_deref() == Some("") {
            return Err(SessionError::InvalidInput(
                "parent_conversation_tip must be an oid or absent".to_string(),
            ));
        }
        if self.user_message.role != lattice_conversation::MessageRole::User {
            return Err(SessionError::InvalidInput(
                "user message role must be 'user'".to_string(),
            ));
        }
        let mut text_bytes = 0usize;
        for part in &self.user_message.parts {
            use lattice_conversation::MessageSemantic;
            match &part.semantic {
                MessageSemantic::Text { text } => text_bytes += text.len(),
                MessageSemantic::Reasoning { .. } => {
                    return Err(SessionError::InvalidInput(
                        "user message must not carry reasoning parts".to_string(),
                    ));
                }
                MessageSemantic::ToolCall { .. } => {
                    return Err(SessionError::InvalidInput(
                        "user message must not carry tool call parts".to_string(),
                    ));
                }
                MessageSemantic::ToolResult { .. } => {
                    return Err(SessionError::InvalidInput(
                        "user message must not carry tool result parts".to_string(),
                    ));
                }
            }
        }
        if text_bytes > MAX_USER_INPUT_BYTES {
            return Err(SessionError::InvalidInput(format!(
                "user input exceeds the byte bound: {text_bytes} > {MAX_USER_INPUT_BYTES}"
            )));
        }
        self.config.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_conversation::{MessagePart, MessageRole, MessageSemantic};

    fn catalog() -> ToolCatalogSnapshot {
        ToolCatalogSnapshot::new(
            "build-1".to_string(),
            vec![ToolCatalogEntry {
                name: "observe.workspace".to_string(),
                descriptor_identity: "a".repeat(64),
                description: "observe".to_string(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
        )
        .expect("catalog")
    }

    fn limits() -> TurnExecutionLimits {
        TurnExecutionLimits {
            timeout_secs: 120,
            max_model_steps: MAX_MODEL_STEPS,
            max_model_attempts: 3,
        }
    }

    fn config() -> FrozenTurnConfig {
        FrozenTurnConfig {
            agent_name: "coder".to_string(),
            model: "m".to_string(),
            provider: "p".to_string(),
            system_prompt: None,
            profile_snapshot_json: "{\"model\":\"m\"}".to_string(),
            policy_snapshot_json: "{}".to_string(),
            catalog: catalog(),
            limits: limits(),
        }
    }

    fn user_message(text: &str) -> NativeMessage<UnboundToolCall> {
        NativeMessage::new(
            MessageRole::User,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: text.to_string(),
            })],
        )
    }

    #[test]
    fn catalog_limit_is_exact_boundary_legal_plus_one_rejected() {
        let build = |description_len: usize| {
            ToolCatalogSnapshot::new(
                "b".to_string(),
                vec![ToolCatalogEntry {
                    name: "t".to_string(),
                    descriptor_identity: "d".to_string(),
                    description: "x".repeat(description_len),
                    input_schema: serde_json::json!({}),
                }],
            )
        };
        // Find the largest description length whose canonical snapshot fits.
        let mut lo = 0usize;
        let mut hi = MAX_CATALOG_SNAPSHOT_BYTES;
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if build(mid).is_ok() {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let boundary = build(lo).expect("largest fitting snapshot");
        boundary.validate().expect("exact boundary validates");
        assert!(
            matches!(build(lo + 1), Err(SessionError::InvalidInput { .. })),
            "plus one byte must fail closed"
        );
    }

    #[test]
    fn catalog_digest_is_self_describing() {
        let snapshot = catalog();
        snapshot.validate().expect("valid");
        let mut tampered = snapshot.clone();
        tampered.entries[0].description = "changed".to_string();
        assert!(matches!(
            tampered.validate(),
            Err(SessionError::JournalCorruption { .. })
        ));
    }

    #[test]
    fn frozen_config_rejects_credential_keys_at_any_depth() {
        let mut bad = config();
        bad.profile_snapshot_json =
            "{\"lancet\":{\"net\":{\"api_key\":\"sk-1\"}}}".to_string();
        assert!(matches!(
            bad.validate(),
            Err(SessionError::InvalidInput { .. })
        ));
        let mut nested = config();
        nested.policy_snapshot_json = "{\"a\":[{\"PASSWORD\":\"x\"}]}".to_string();
        assert!(matches!(
            nested.validate(),
            Err(SessionError::InvalidInput { .. })
        ));
        config().validate().expect("clean config passes");
    }

    #[test]
    fn limits_must_carry_the_code_defined_step_limit() {
        let mut limits = limits();
        limits.max_model_steps = MAX_MODEL_STEPS + 1;
        assert!(limits.validate().is_err());
        limits.max_model_steps = MAX_MODEL_STEPS;
        limits.max_model_attempts = 0;
        assert!(limits.validate().is_err());
        limits.max_model_attempts = 1;
        limits.validate().expect("valid limits");
    }

    #[test]
    fn turn_started_facts_validate_user_message_shape() {
        let facts = |message: NativeMessage<UnboundToolCall>| TurnStartedFacts {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
            parent_conversation_tip: None,
            user_message: message,
            config: config(),
        };
        facts(user_message("hello")).validate().expect("valid");

        let assistant = NativeMessage::new(
            MessageRole::Assistant,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::Text {
                text: "hi".to_string(),
            })],
        );
        assert!(facts(assistant).validate().is_err());

        let with_call = NativeMessage::new(
            MessageRole::User,
            None,
            vec![MessagePart::semantic_only(MessageSemantic::ToolCall {
                call: UnboundToolCall {
                    provider_call_id: None,
                    name: "t".to_string(),
                    arguments: serde_json::json!({}),
                },
            })],
        );
        assert!(facts(with_call).validate().is_err());
    }

    #[test]
    fn turn_started_facts_reject_oversize_user_input() {
        let mut facts = TurnStartedFacts {
            turn_id: TurnId::parse("3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c").unwrap(),
            session_id: SessionId::parse("3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c").unwrap(),
            parent_conversation_tip: None,
            user_message: user_message(&"x".repeat(MAX_USER_INPUT_BYTES)),
            config: config(),
        };
        facts.validate().expect("exact boundary user input is legal");
        facts.user_message = user_message(&"x".repeat(MAX_USER_INPUT_BYTES + 1));
        assert!(facts.validate().is_err());
    }
}
