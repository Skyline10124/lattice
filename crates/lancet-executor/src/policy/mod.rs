//! Phase 21 CEP policy and budget configuration (immutable per turn).

pub mod budget;
pub mod command;
pub mod network;
pub mod observation;

use std::path::PathBuf;
use std::sync::Arc;

pub use budget::{
    BudgetExhausted, BudgetLimits, ObservationBudgetSnapshot, ObservationBudgetTracker,
};
pub use command::{classify_command, build_effective_env, validate_timeout, CommandPolicy, EffectiveImpact};
pub use network::NetworkPolicyConfig;
pub use observation::ObservationPolicyConfig;

/// Immutable Phase 21 policy limits and roots assembled once per turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Phase21PolicyConfig {
    pub project_root: PathBuf,
    pub observation: ObservationPolicyConfig,
    pub network: NetworkPolicyConfig,
    pub budget: BudgetLimits,
    pub command: CommandPolicy,
}

impl Phase21PolicyConfig {
    pub fn defaults_for_project_root(project_root: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        let project_root = std::fs::canonicalize(&project_root).unwrap_or(project_root);
        let observation = ObservationPolicyConfig::defaults_for_root(&project_root);
        let budget = BudgetLimits {
            max_single_read_bytes: observation.max_single_read_bytes,
            max_turn_observation_bytes: observation.max_turn_observation_bytes,
            max_grep_matches: observation.max_grep_matches,
            max_list_entries: observation.max_list_entries,
            max_find_paths: observation.max_find_paths,
            max_todo_ops: observation.max_todo_ops,
        };
        Self {
            project_root,
            observation,
            network: NetworkPolicyConfig::safe_defaults(),
            budget,
            command: CommandPolicy::defaults(),
        }
    }

    pub fn new_budget_tracker(&self) -> Arc<ObservationBudgetTracker> {
        ObservationBudgetTracker::new(self.budget.clone())
    }
}

/// Turn-scoped policy and mutable budget state shared by handlers.
#[derive(Clone, Debug)]
pub struct Phase21TurnContext {
    pub policy: Phase21PolicyConfig,
    pub budget: Arc<ObservationBudgetTracker>,
}

impl Phase21TurnContext {
    pub fn from_policy(policy: Phase21PolicyConfig) -> Self {
        let budget = policy.new_budget_tracker();
        Self { policy, budget }
    }

    pub fn defaults_for_project_root(project_root: impl Into<PathBuf>) -> Self {
        Self::from_policy(Phase21PolicyConfig::defaults_for_project_root(project_root))
    }
}
