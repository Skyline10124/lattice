// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Provider-neutral token usage contract (ADR 0005 §10).
//!
//! Usage is authoritative per attempt: each attempt stores the
//! provider-reported `Option<TokenUsage>`. Missing usage is **unknown**, never
//! zero. Retry attempts are recorded separately. A Turn-level total is a
//! derived `Complete | Partial` aggregate, never an independent authority.

use serde::{Deserialize, Serialize};

/// Token usage reported by one provider attempt.
///
/// Previously defined in `lattice-session` as `TurnUsage`; the shape is
/// unchanged and `lattice-session` re-exports it under that name. The
/// provider-neutral contract uses `u64` counters; the `lattice-llm` wire type
/// (`u32`) is widened checked at the Agent boundary until #139-B unifies them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Saturating component-wise addition for derived aggregates. Individual
    /// attempt records stay untouched; this only builds Turn-level sums.
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            prompt_tokens: self.prompt_tokens.saturating_add(other.prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_add(other.completion_tokens),
            total_tokens: self.total_tokens.saturating_add(other.total_tokens),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_usage_is_unknown_not_zero() {
        let unknown: Option<TokenUsage> = None;
        // The contract: a missing report must stay distinguishable from a
        // real all-zero report.
        let zero = Some(TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        });
        assert_ne!(unknown, zero);
    }

    #[test]
    fn usage_serde_roundtrip() {
        let usage = TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 4,
            total_tokens: 14,
        };
        let json = serde_json::to_string(&usage).expect("serialize");
        assert_eq!(
            json,
            r#"{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}"#
        );
        let back: TokenUsage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(usage, back);
    }
}
