//! Turn-scoped observation budget tracking for Phase 21 CEP handlers.

use lancet_protocol::error::ReservedErrorData;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability};
use std::sync::{Arc, Mutex};

/// Immutable per-turn budget limits (from profile defaults or explicit values).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetLimits {
    pub max_single_read_bytes: usize,
    pub max_turn_observation_bytes: usize,
    pub max_grep_matches: usize,
    pub max_list_entries: usize,
    pub max_find_paths: usize,
    pub max_todo_ops: usize,
}

impl BudgetLimits {
    /// Defaults aligned with [`lattice_tooling::ToolLimits`] and legacy search caps.
    pub fn defaults() -> Self {
        Self {
            max_single_read_bytes: 10 * 1024 * 1024,
            max_turn_observation_bytes: 10 * 1024 * 1024,
            max_grep_matches: 1000,
            max_list_entries: 1000,
            max_find_paths: 1000,
            max_todo_ops: 100,
        }
    }
}

/// Compact budget snapshot for audit metadata (not a decision source).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationBudgetSnapshot {
    pub turn_observation_bytes_used: usize,
    pub max_turn_observation_bytes: usize,
    pub todo_ops_used: usize,
    pub max_todo_ops: usize,
}

/// Error when a budget reservation would exceed a turn limit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetExhausted {
    pub limit: &'static str,
    pub requested: usize,
    pub remaining: usize,
}

impl BudgetExhausted {
    /// Map budget exhaustion to a typed CEP error for handler responses.
    pub fn to_cep_error(&self) -> CepError {
        CepError::new(
            CepErrorCode::Reserved,
            format!("observation budget exhausted: {}", self.limit),
            CepErrorData::Reserved(ReservedErrorData {
                code: self.limit.to_string(),
                detail: format!("requested={} remaining={}", self.requested, self.remaining),
            }),
            Retriability::RequiresReplan,
        )
    }
}

/// Mutable per-turn observation and todo-op counters.
#[derive(Debug)]
pub struct ObservationBudgetTracker {
    limits: BudgetLimits,
    turn_observation_bytes: Mutex<usize>,
    todo_ops: Mutex<usize>,
}

impl ObservationBudgetTracker {
    pub fn new(limits: BudgetLimits) -> Arc<Self> {
        Arc::new(Self {
            limits,
            turn_observation_bytes: Mutex::new(0),
            todo_ops: Mutex::new(0),
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

    /// Reserve `count` todo items for a single `session.todo_write` call this turn.
    pub fn try_charge_todo_ops(&self, count: usize) -> Result<(), BudgetExhausted> {
        if count == 0 {
            return Ok(());
        }
        let mut ops = self.todo_ops.lock().expect("budget mutex poisoned");
        let next = *ops + count;
        if next > self.limits.max_todo_ops {
            return Err(BudgetExhausted {
                limit: "observation.max_todo_ops",
                requested: count,
                remaining: self.limits.max_todo_ops.saturating_sub(*ops),
            });
        }
        *ops = next;
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
        let todo_ops_used = *self.todo_ops.lock().expect("budget mutex poisoned");
        ObservationBudgetSnapshot {
            turn_observation_bytes_used,
            max_turn_observation_bytes: self.limits.max_turn_observation_bytes,
            todo_ops_used,
            max_todo_ops: self.limits.max_todo_ops,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase21_budget_rejects_aggregate_observation_exhaustion() {
        let limits = BudgetLimits {
            max_turn_observation_bytes: 100,
            ..BudgetLimits::defaults()
        };
        let tracker = ObservationBudgetTracker::new(limits);
        tracker.try_charge_observation(60).unwrap();
        let err = tracker.try_charge_observation(50).unwrap_err();
        assert_eq!(err.limit, "observation.max_turn_observation_bytes");
        assert_eq!(err.remaining, 40);
    }

    #[test]
    fn phase21_budget_snapshot_without_audit_store() {
        let tracker = ObservationBudgetTracker::new(BudgetLimits::defaults());
        tracker.try_charge_observation(128).unwrap();
        tracker.try_charge_todo_ops(1).unwrap();
        let snap = tracker.snapshot();
        assert_eq!(snap.turn_observation_bytes_used, 128);
        assert_eq!(snap.todo_ops_used, 1);
        assert_eq!(
            snap.max_turn_observation_bytes,
            BudgetLimits::defaults().max_turn_observation_bytes
        );
    }

    #[test]
    fn phase21_budget_todo_op_cap() {
        let limits = BudgetLimits {
            max_todo_ops: 2,
            ..BudgetLimits::defaults()
        };
        let tracker = ObservationBudgetTracker::new(limits);
        tracker.try_charge_todo_ops(1).unwrap();
        tracker.try_charge_todo_ops(1).unwrap();
        let err = tracker.try_charge_todo_ops(1).unwrap_err();
        assert_eq!(err.limit, "observation.max_todo_ops");
        assert_eq!(err.remaining, 0);
    }

    #[test]
    fn phase21_budget_todo_ops_charge_by_item_count() {
        let limits = BudgetLimits {
            max_todo_ops: 1,
            ..BudgetLimits::defaults()
        };
        let tracker = ObservationBudgetTracker::new(limits);
        let err = tracker.try_charge_todo_ops(2).unwrap_err();
        assert_eq!(err.requested, 2);
        assert_eq!(err.remaining, 1);
    }
}
