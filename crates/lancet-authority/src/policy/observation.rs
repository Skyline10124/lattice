// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP workspace observation path policy configuration and resolution.

/// Immutable observation limits derived from profile `[lancet.observation]`.
///
/// Limits only — observation scope is the canonical project root plus the
/// `[lancet.workspace]` read-visibility rules, enforced by secure path
/// resolution, Guard L1, and the `VisibilityEngine`. An `allowed_roots` field
/// here was configurable but reached no read decision, so it silently promised
/// a narrower surface than the Agent actually had (#121).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationPolicyConfig {
    pub max_single_read_bytes: usize,
    pub max_turn_observation_bytes: usize,
    pub max_grep_matches: usize,
    pub max_list_entries: usize,
    pub max_find_paths: usize,
}

impl ObservationPolicyConfig {
    /// Compiled limit defaults.
    pub fn defaults() -> Self {
        Self {
            max_single_read_bytes: 10 * 1024 * 1024,
            max_turn_observation_bytes: 10 * 1024 * 1024,
            max_grep_matches: 1000,
            max_list_entries: 1000,
            max_find_paths: 1000,
        }
    }

    /// Project the turn-scoped budget out of these limits.
    ///
    /// This is the only place [`super::BudgetLimits`] is derived, so profile
    /// resolution and the turn-scoped tracker cannot drift.
    pub fn budget_limits(&self) -> super::BudgetLimits {
        super::BudgetLimits {
            max_turn_observation_bytes: self.max_turn_observation_bytes,
        }
    }
}
