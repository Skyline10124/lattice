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
/// `lattice-session` re-exports this type as `TurnUsage`, and provider wire
/// projectors parse directly into the same `u64` counters. Missing usage remains
/// `None`; malformed or overflowing provider counters fail at the projection seam.
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
