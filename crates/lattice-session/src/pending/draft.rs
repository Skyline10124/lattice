// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Cross-store `ResultDraft` material (ADR 0005 §12, GitHub #144).
//!
//! A draft is material, never outcome authority: it binds an invocation's
//! canonical ToolContent by digest and byte length and contains no outcome.
//! It cannot upgrade itself into success or failure after a crash. Canonical
//! ToolContent is limited to [`MAX_TOOL_CONTENT_BYTES`]; a plus-one result is
//! never truncated (an arbitrary prefix may be invalid JSON, invalid UTF-8,
//! or a semantically false result) — oversize content writes no draft and the
//! terminal keeps the real execution outcome plus
//! `Unavailable(OutputTooLarge { .. })`.

use lancet_protocol::{InvocationId, ModelMessageAddress, TurnId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Result;

/// Canonical ToolContent byte limit (64 KiB, ADR 0005 §12). Exact boundary
/// legal; plus one rejected without truncation.
pub const MAX_TOOL_CONTENT_BYTES: usize = 65_536;

const CONTENT_DOMAIN: &[u8] = b"lattice-tool-content-digest-v1\0";

/// Construction/validation error for a draft's canonical content.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolContentError {
    /// The canonical content exceeds the byte limit. Carries the exact
    /// limit, the actual length, and the digest of the full content so the
    /// terminal's `Unavailable(OutputTooLarge)` is honest without storing
    /// the bytes.
    #[error("tool content too large: limit {limit} bytes, got {actual}")]
    TooLarge {
        /// Protocol limit in bytes.
        limit: usize,
        /// Actual canonical byte length.
        actual: usize,
        /// Digest of the full (unstored) content.
        full_digest: [u8; 32],
    },
}
impl ToolContentError {
    /// The digest of the full oversize content, hex-encoded.
    pub fn full_digest_hex(&self) -> String {
        match self {
            Self::TooLarge { full_digest, .. } => {
                full_digest.iter().map(|b| format!("{b:02x}")).collect()
            }
        }
    }
}

/// SHA-256 over the domain separator, explicit byte length, and the
/// canonical ToolContent bytes (same construction as the conversation
/// digests, ADR 0005 §7/§12).
pub fn tool_content_digest(canonical_content: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(CONTENT_DOMAIN);
    hasher.update((canonical_content.len() as u64).to_be_bytes());
    hasher.update(canonical_content.as_bytes());
    hasher.finalize().into()
}

/// The exact cross-store ResultDraft record (ADR 0005 §12). Carries no
/// outcome: only identity, binding, address, canonical content, digest, and
/// byte length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultDraft {
    /// The invocation this draft belongs to.
    pub invocation_id: InvocationId,
    /// Digest of the exact binding tuple this draft closes.
    #[serde(with = "super::hex_digest")]
    pub binding_digest: [u8; 32],
    /// The owning Turn.
    pub turn_id: TurnId,
    /// Structural address of the containing model message.
    pub message: ModelMessageAddress,
    /// Part ordinal of the ToolCall inside that message.
    pub part_ordinal: u32,
    /// Canonical JSON text of the provider-neutral ToolContent.
    pub canonical_tool_content: String,
    /// Digest of the canonical content.
    #[serde(with = "super::hex_digest")]
    pub content_digest: [u8; 32],
    /// Canonical content byte length.
    pub byte_length: u64,
}

impl ResultDraft {
    /// Build a draft from canonical ToolContent, enforcing the byte limit
    /// with no truncation. `canonical_tool_content` must already be the
    /// canonical JSON spelling of the content; its length is the byte
    /// length.
    pub fn new(
        invocation_id: InvocationId,
        binding_digest: [u8; 32],
        turn_id: TurnId,
        message: ModelMessageAddress,
        part_ordinal: u32,
        canonical_tool_content: String,
    ) -> std::result::Result<Self, ToolContentError> {
        let actual = canonical_tool_content.len();
        if actual > MAX_TOOL_CONTENT_BYTES {
            return Err(ToolContentError::TooLarge {
                limit: MAX_TOOL_CONTENT_BYTES,
                actual,
                full_digest: tool_content_digest(&canonical_tool_content),
            });
        }
        Ok(Self {
            invocation_id,
            binding_digest,
            turn_id,
            message,
            part_ordinal,
            content_digest: tool_content_digest(&canonical_tool_content),
            byte_length: actual as u64,
            canonical_tool_content,
        })
    }

    /// Validate a deserialized draft: the recorded digest and byte length
    /// must describe the recorded canonical content, and the content must
    /// fit the limit.
    pub fn validate(&self) -> Result<()> {
        if self.canonical_tool_content.len() > MAX_TOOL_CONTENT_BYTES {
            return Err(crate::SessionError::JournalCorruption {
                detail: format!(
                    "result draft content exceeds {} bytes",
                    MAX_TOOL_CONTENT_BYTES
                ),
            });
        }
        if tool_content_digest(&self.canonical_tool_content) != self.content_digest {
            return Err(crate::SessionError::JournalCorruption {
                detail: format!(
                    "result draft digest does not describe its content for invocation {}",
                    self.invocation_id
                ),
            });
        }
        if self.byte_length != self.canonical_tool_content.len() as u64 {
            return Err(crate::SessionError::JournalCorruption {
                detail: format!(
                    "result draft byte length {} does not match content length {}",
                    self.byte_length,
                    self.canonical_tool_content.len()
                ),
            });
        }
        Ok(())
    }
}

/// Classification of one draft publication attempt (ADR 0005 §12).
///
/// - `Durable`: the draft object is durably published at `draft_oid`.
/// - `CleanAbsent`: provably no draft was published (reopen finds the
///   content-addressed object absent); permits a terminal with the true
///   execution outcome and `Unavailable(DraftPublicationFailed)`.
/// - `Uncertain`: publication failed after the write was attempted (a
///   durability barrier error); must be revalidated against the exact
///   candidate during the live finite FinalizeBudget, and a mismatch is
///   corruption. Reopen classification never returns this variant: the
///   content-addressed object is either present (`Durable`) or absent
///   (`CleanAbsent`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftPublication {
    /// The draft is durably published.
    Durable {
        /// Object id of the published draft.
        draft_oid: String,
    },
    /// No draft was published.
    CleanAbsent,
    /// Publication was attempted; durability is unproven. The exact
    /// candidate (identity and digests) is carried for revalidation.
    Uncertain {
        /// The exact candidate that was being published.
        candidate: ResultDraft,
    },
}

/// Reopen-side classification of a pinned draft candidate: the
/// content-addressed object either survived (`Durable`) or was never durably
/// written (`CleanAbsent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DraftClassification {
    /// The draft object exists and matches the candidate.
    Durable,
    /// The draft object is absent.
    CleanAbsent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::{SessionId, TurnId};

    const TURN: &str = "3f8b2c1a-7d4e-4f5a-8b9c-0d1e2f3a4b5c";
    const SESSION: &str = "3f8b2c1a7d4e4f5a8b9c0d1e2f3a4b5c";

    fn draft_for(content: String) -> std::result::Result<ResultDraft, ToolContentError> {
        ResultDraft::new(
            InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
            [2u8; 32],
            TurnId::parse(TURN).unwrap(),
            ModelMessageAddress {
                session_id: SessionId::parse(SESSION).unwrap(),
                turn_id: TurnId::parse(TURN).unwrap(),
                model_step_ordinal: 0,
            },
            0,
            content,
        )
    }

    #[test]
    fn exact_boundary_content_is_legal_plus_one_is_rejected_untruncated() {
        let exact = "{\"v\":\"".to_string() + &"x".repeat(MAX_TOOL_CONTENT_BYTES - 8) + "\"}";
        assert_eq!(exact.len(), MAX_TOOL_CONTENT_BYTES);
        let draft = draft_for(exact).expect("exact boundary");
        draft.validate().expect("valid");
        assert_eq!(draft.byte_length, MAX_TOOL_CONTENT_BYTES as u64);

        let oversize = "{\"v\":\"".to_string() + &"x".repeat(MAX_TOOL_CONTENT_BYTES - 7) + "\"}";
        assert_eq!(oversize.len(), MAX_TOOL_CONTENT_BYTES + 1);
        match draft_for(oversize) {
            Err(ToolContentError::TooLarge {
                limit,
                actual,
                full_digest,
            }) => {
                assert_eq!(limit, MAX_TOOL_CONTENT_BYTES);
                assert_eq!(actual, MAX_TOOL_CONTENT_BYTES + 1);
                // The full digest stays honest for the oversize terminal.
                assert_eq!(full_digest.len(), 32);
            }
            Ok(draft) => panic!("plus-one content must not produce a draft: {draft:?}"),
        }
    }

    #[test]
    fn draft_carries_no_outcome_and_validates_digest() {
        let draft = draft_for("{\"rows\":[]}".to_string()).expect("draft");
        // Serialized shape: no outcome field exists on the contract.
        let value = serde_json::to_value(&draft).unwrap();
        assert!(value.get("outcome").is_none());
        draft.validate().expect("valid");

        let mut tampered = draft.clone();
        tampered.canonical_tool_content = "{\"rows\":[1]}".to_string();
        assert!(tampered.validate().is_err());

        let mut lying_length = draft;
        lying_length.byte_length += 1;
        assert!(lying_length.validate().is_err());
    }

    #[test]
    fn publication_classification_variants_serialize() {
        let durable = DraftPublication::Durable {
            draft_oid: "0123".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&durable).unwrap(),
            "{\"durable\":{\"draft_oid\":\"0123\"}}"
        );
        assert_eq!(
            serde_json::to_string(&DraftPublication::CleanAbsent).unwrap(),
            "\"clean_absent\""
        );
        let uncertain = DraftPublication::Uncertain {
            candidate: draft_for("{}".to_string()).unwrap(),
        };
        let back: DraftPublication =
            serde_json::from_value(serde_json::to_value(&uncertain).unwrap()).unwrap();
        assert_eq!(back, uncertain);
    }
}
