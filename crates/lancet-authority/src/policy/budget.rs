// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Turn-scoped observation budget tracking.

use std::sync::{Arc, Mutex};

/// Immutable per-turn budget limits, derived from the observation policy.
///
/// This is a projection, not a configuration surface: build it with
/// [`super::ObservationPolicyConfig::budget_limits`] rather than by hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetLimits {
    pub max_turn_observation_bytes: usize,
}

impl BudgetLimits {
    /// Defaults matching [`super::ObservationPolicyConfig::defaults`].
    pub fn defaults() -> Self {
        Self {
            max_turn_observation_bytes: 10 * 1024 * 1024,
        }
    }
}

/// Compact budget snapshot for audit metadata (not a decision source).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationBudgetSnapshot {
    pub turn_observation_bytes_used: usize,
    pub max_turn_observation_bytes: usize,
}

/// Error when a budget reservation would exceed a turn limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetExhausted {
    pub limit: &'static str,
    pub requested: usize,
    pub remaining: usize,
}

/// Mutable per-turn observation counter.
#[derive(Debug)]
pub struct ObservationBudgetTracker {
    limits: BudgetLimits,
    turn_observation_bytes: Mutex<usize>,
}

impl ObservationBudgetTracker {
    pub fn new(limits: BudgetLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            turn_observation_bytes: Mutex::new(0),
        })
    }

    /// Reserve returned LLM-visible observation payload bytes for this turn.
    pub fn try_charge_observation(&self, bytes: usize) -> Result<(), BudgetExhausted> {
        let mut used = self
            .turn_observation_bytes
            .lock()
            .expect("budget mutex poisoned");
        let next = used.saturating_add(bytes);
        if next > self.limits.max_turn_observation_bytes {
            return Err(BudgetExhausted {
                limit: "observation.max_turn_observation_bytes",
                requested: bytes,
                remaining: self.limits.max_turn_observation_bytes.saturating_sub(*used),
            });
        }
        *used = next;
        Ok(())
    }

    pub fn limits(&self) -> &BudgetLimits {
        &self.limits
    }

    pub fn snapshot(&self) -> ObservationBudgetSnapshot {
        let turn_observation_bytes_used = *self
            .turn_observation_bytes
            .lock()
            .expect("budget mutex poisoned");
        ObservationBudgetSnapshot {
            turn_observation_bytes_used,
            max_turn_observation_bytes: self.limits.max_turn_observation_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cep_budget_rejects_aggregate_observation_exhaustion() {
        let limits = BudgetLimits {
            max_turn_observation_bytes: 100,
        };
        let tracker = ObservationBudgetTracker::new(limits);
        tracker.try_charge_observation(60).unwrap();
        let err = tracker.try_charge_observation(50).unwrap_err();
        assert_eq!(err.limit, "observation.max_turn_observation_bytes");
        assert_eq!(err.remaining, 40);
    }
}
