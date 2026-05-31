//! Effect-region capture and decision layer for the CEP executor.
//!
//! - [`EffectLedger`] — Arc-backed handle for recording observed effects during dispatch.
//! - [`EffectiveEffect`] / [`AuthorizedEffectRegion`] — per-invocation decision types (D-03).
//!
//! # Enforcement Model (EFF-05)
//!
//! The effect-region contract is runtime-enforced and test-time-enforced only —
//! there is no static type-and-effect proof system.
//!
//! - **Runtime (EFF-02):** [`checker::check_effects`] compares `observed ⊆ authorized`
//!   after every dispatch; `EffectViolation` on overreach.
//! - **Test-time (EFF-04):** [`crate::testing::effect_harness::effect_contract_harness`]
//!   provides property-based fuzz tests for all catalog methods.
//!
//! Static type-and-effect proving is a v1.3-beta candidate.

pub mod checker;
pub mod ledger;
pub mod region;

pub use ledger::{AuthorizedRegionSlot, EffectLedger, EffectSource, ObservedEffect, ObservedScope};
pub use region::{AuthorizedEffectRegion, EffectiveEffect};

#[cfg(test)]
pub(crate) fn shared_test_policy() -> crate::policy::CepPolicyConfig {
    use crate::policy::command::CommandPolicy;
    use crate::policy::network::NetworkPolicyConfig;
    use crate::policy::observation::ObservationPolicyConfig;
    use crate::policy::BudgetLimits;
    use std::path::PathBuf;

    crate::policy::CepPolicyConfig {
        project_root: PathBuf::from("/project"),
        observation: ObservationPolicyConfig {
            allowed_roots: vec![PathBuf::from("/project")],
            max_single_read_bytes: 10_000_000,
            max_turn_observation_bytes: 10_000_000,
            max_grep_matches: 1000,
            max_list_entries: 1000,
            max_find_paths: 1000,
            max_todo_ops: 100,
        },
        network: NetworkPolicyConfig::safe_defaults(),
        budget: BudgetLimits {
            max_single_read_bytes: 10_000_000,
            max_turn_observation_bytes: 10_000_000,
            max_grep_matches: 1000,
            max_list_entries: 1000,
            max_find_paths: 1000,
            max_todo_ops: 100,
        },
        command: CommandPolicy::defaults(),
    }
}
