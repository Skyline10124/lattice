//! Compatibility re-exports for CEP authority policy.

pub mod budget {
    pub use lancet_authority::policy::budget::{
        BudgetExhausted, BudgetLimits, ObservationBudgetSnapshot, ObservationBudgetTracker,
    };
}
pub mod capability {
    pub use lancet_authority::policy::capability::{
        match_capability, CapabilityActionRequired, CapabilityFailureReason, CapabilityMatch,
        CapabilityMatchVerdict, InvocationFacts, SessionStateOperation,
    };
}
pub mod command {
    pub use lancet_authority::policy::command::{
        build_effective_env, classify_command, validate_timeout, CommandPolicy, EffectiveImpact,
    };
}
pub mod default {
    pub use lancet_authority::policy::default::{
        CapabilitySourceClass, CapabilitySourcePolicy, DefaultPolicyHook, ExternalBudgetPolicy,
        ExternalCapabilitySourcePolicy, ExternalCommandPolicy, ExternalMethodPolicy,
        ExternalNetworkPolicy, ExternalObservationPolicy, ExternalPolicyFile, MethodPolicy,
        PolicyConfig, PolicyEvidence, RichPolicyOutcome,
    };
}
pub mod network {
    pub use lancet_authority::policy::network::{
        validate_fetch_url, NetworkPolicyConfig, ValidatedFetchUrl,
    };
}
pub mod observation {
    pub use lancet_authority::policy::observation::{
        backend_safety_net_denial, reject_parent_traversal, resolve_observation_target,
        ObservationPolicyConfig, ResolvedObservationTarget,
    };

    #[cfg(test)]
    pub(crate) const BACKEND_SAFETY_NET_TEST_PATH: &str = "__cep_backend_safety_net__";

    /// Test-only hook simulating a backend-time denial after CEP policy allowed the path.
    #[cfg(test)]
    pub(crate) fn check_backend_safety_net(path: &str) -> Option<lancet_protocol::CepError> {
        if path == BACKEND_SAFETY_NET_TEST_PATH {
            Some(backend_safety_net_denial(path))
        } else {
            None
        }
    }
}
pub mod resolved {
    pub use lancet_authority::policy::resolved::{
        PolicyResolver, ResolvedCommand, ResolvedNetwork, ResolvedPath, ResolvedPolicyInput,
        ResolvedSessionState,
    };
}

pub use lancet_authority::policy::{
    build_effective_env, classify_command, validate_timeout, BudgetExhausted, BudgetLimits,
    CepPolicyConfig, CepTurnContext, CommandPolicy, EffectiveImpact, NetworkPolicyConfig,
    ObservationBudgetSnapshot, ObservationBudgetTracker, ObservationPolicyConfig,
};
