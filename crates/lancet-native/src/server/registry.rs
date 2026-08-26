// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native handler context and registry type binding.

use lancet_authority::policy::CepTurnContext;
use lancet_authority::policy::default::DefaultPolicyHook;
pub use lancet_dispatch::{Cancellation, EventSink};
use lancet_protocol::policy::CepMode;
use lancet_protocol::policy::PolicyHook;
use lancet_protocol::{InvocationId, SessionId, TurnId};
use std::sync::Arc;

/// Method registry specialized to native CEP handler context.
pub type MethodRegistry = lancet_dispatch::MethodRegistry<HandlerContext>;

/// Per-call context passed to native registered handlers.
///
/// There is no backend identity, Upper identity, or replaceable workspace
/// implementation on the native call context. Component calls receive their
/// concrete Local Workspace authority through Tool Host imports instead.
#[derive(Clone)]
pub struct HandlerContext {
    pub invocation_id: InvocationId,
    pub events: EventSink,
    pub cancellation: Cancellation,
    pub cep_context: Arc<CepTurnContext>,
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub effect_ledger: lancet_authority::effect::ledger::EffectLedger,
    pub authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot,
    pub policy_hook: Arc<dyn PolicyHook>,
    pub default_policy_hook: Option<Arc<DefaultPolicyHook>>,
    pub cep_mode: CepMode,
}

impl std::fmt::Debug for HandlerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandlerContext")
            .field("invocation_id", &self.invocation_id)
            .field("events", &self.events)
            .field("cancellation", &self.cancellation)
            .field("cep_context", &self.cep_context)
            .field("session_id", &self.session_id)
            .field("turn_id", &self.turn_id)
            .field("effect_ledger", &self.effect_ledger)
            .field("authorized_region", &self.authorized_region)
            .field("policy_hook", &"<dyn PolicyHook>")
            .field("default_policy_hook", &self.default_policy_hook)
            .field("cep_mode", &self.cep_mode)
            .finish()
    }
}

impl HandlerContext {
    /// Record an observed effect into the per-call effect ledger.
    pub fn record_effect(&self, effect: lancet_authority::effect::ledger::ObservedEffect) {
        self.effect_ledger.record(effect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::native_registry::register_production_method_registry;
    use lancet_catalog::catalog;

    #[test]
    fn registry_full_catalog_registered_via_production_helper() {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();

        for descriptor in catalog() {
            assert!(registry.contains(descriptor.name));
        }
    }
}
