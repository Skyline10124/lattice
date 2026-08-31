// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP CEP policy and budget configuration (immutable per turn).

pub mod budget;
pub mod default;
pub mod network;
pub mod observation;
pub mod visibility;

use std::path::PathBuf;
use std::sync::Arc;

pub use budget::{
    BudgetExhausted, BudgetLimits, ObservationBudgetSnapshot, ObservationBudgetTracker,
};
use lancet_protocol::evidence::CapabilityAdmissionPolicy;
pub use network::{NetworkPolicyConfig, host_allowed_by_list, normalize_host_pattern};
pub use observation::ObservationPolicyConfig;

/// Immutable CEP policy limits and roots assembled once per turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CepPolicyConfig {
    pub project_root: PathBuf,
    /// The one observation limit set. Per-op caps are read from here by the
    /// handlers; the turn-scoped budget is derived through
    /// [`ObservationPolicyConfig::budget_limits`] (#121) rather than stored as a
    /// second copy that has to be kept in sync.
    pub observation: ObservationPolicyConfig,
    pub network: NetworkPolicyConfig,
    /// Generated Component capability admission, always enforced.
    pub capability_admission: CapabilityAdmissionPolicy,
}

impl CepPolicyConfig {
    pub fn defaults_for_project_root(project_root: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        let project_root = std::fs::canonicalize(&project_root).unwrap_or(project_root);
        Self {
            observation: ObservationPolicyConfig::defaults(),
            project_root,
            network: NetworkPolicyConfig::safe_defaults(),
            // Fail closed: no tool is enabled until an operator policy says so.
            capability_admission: CapabilityAdmissionPolicy::default(),
        }
    }

    /// The turn-scoped budget derived from the observation limits (#121).
    pub fn budget(&self) -> BudgetLimits {
        self.observation.budget_limits()
    }

    pub fn new_budget_tracker(&self) -> Arc<ObservationBudgetTracker> {
        ObservationBudgetTracker::new(self.budget())
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
