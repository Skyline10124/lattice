// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP CEP policy and budget configuration (immutable per turn).

pub mod budget;
pub mod capability;
pub mod default;
pub mod network;
pub mod observation;
pub mod resolved;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

pub use budget::{
    BudgetExhausted, BudgetLimits, ObservationBudgetSnapshot, ObservationBudgetTracker,
};
use lancet_protocol::effect_types::ImpactKind;
pub use network::NetworkPolicyConfig;
pub use observation::ObservationPolicyConfig;

/// Immutable CEP policy limits and roots assembled once per turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CepPolicyConfig {
    pub project_root: PathBuf,
    pub observation: ObservationPolicyConfig,
    pub network: NetworkPolicyConfig,
    pub budget: BudgetLimits,
    pub denied_effect_kinds: BTreeSet<ImpactKind>,
}

impl CepPolicyConfig {
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
            denied_effect_kinds: BTreeSet::new(),
        }
    }

    pub fn new_budget_tracker(&self) -> Arc<ObservationBudgetTracker> {
        ObservationBudgetTracker::new(self.budget.clone())
    }
}

/// Turn-scoped policy and mutable budget state shared by handlers.
#[derive(Clone, Debug)]
pub struct CepTurnContext {
    pub policy: CepPolicyConfig,
    pub budget: Arc<ObservationBudgetTracker>,
}

impl CepTurnContext {
    pub fn from_policy(policy: CepPolicyConfig) -> Self {
        let budget = policy.new_budget_tracker();
        Self { policy, budget }
    }

    pub fn defaults_for_project_root(project_root: impl Into<PathBuf>) -> Self {
        Self::from_policy(CepPolicyConfig::defaults_for_project_root(project_root))
    }
}
