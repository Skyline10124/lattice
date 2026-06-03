//! Native handler context and registry type binding.

use crate::enforcement::isolation::CommandIsolationRuntimeConfig;
use crate::server::call_durable_metadata::CallDurableMetadata;
use lancet_audit::{CepAuditContext, CepAuditStore};
use lancet_authority::envelope_store::ActiveEnvelopeStore;
use lancet_authority::policy::default::DefaultPolicyHook;
use lancet_authority::policy::CepTurnContext;
pub use lancet_dispatch::{Cancellation, EventSink};
use lancet_invariants::InvariantCardIndex;
use lancet_protocol::envelope::{BaseExecutionEnvelope, CepMode};
use lancet_protocol::policy::PolicyHook;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Method registry specialized to native CEP handler context.
pub type MethodRegistry = lancet_dispatch::MethodRegistry<HandlerContext>;

/// Per-call context passed to native registered handlers.
#[derive(Clone)]
pub struct HandlerContext {
    pub call_id: u64,
    pub events: EventSink,
    pub cancellation: Cancellation,
    pub base_envelope: BaseExecutionEnvelope,
    pub cep_context: Arc<CepTurnContext>,
    pub active_envelope: Arc<ActiveEnvelopeStore>,
    pub turn_id: String,
    pub(crate) durable_metadata: CallDurableMetadata,
    pub effect_ledger: lancet_authority::effect::ledger::EffectLedger,
    pub authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot,
    pub policy_hook: Arc<dyn PolicyHook>,
    pub default_policy_hook: Option<Arc<DefaultPolicyHook>>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    pub audit: Arc<CepAuditStore>,
    pub invariant_index: Arc<InvariantCardIndex>,
    pub cep_mode: CepMode,
    pub command_isolation: CommandIsolationRuntimeConfig,
}

impl std::fmt::Debug for HandlerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandlerContext")
            .field("call_id", &self.call_id)
            .field("events", &self.events)
            .field("cancellation", &self.cancellation)
            .field("base_envelope", &self.base_envelope)
            .field("cep_context", &self.cep_context)
            .field("active_envelope", &self.active_envelope)
            .field("turn_id", &self.turn_id)
            .field("effect_ledger", &self.effect_ledger)
            .field("authorized_region", &self.authorized_region)
            .field("policy_hook", &"<dyn PolicyHook>")
            .field("default_policy_hook", &self.default_policy_hook)
            .field("audit_context", &self.audit_context)
            .field("invariant_index", &self.invariant_index)
            .field("cep_mode", &self.cep_mode)
            .field("command_isolation", &self.command_isolation)
            .finish()
    }
}

impl HandlerContext {
    /// Add compact metadata that will be persisted to the call's durable audit row.
    pub fn record_durable_metadata(&self, metadata: Value) {
        self.durable_metadata.record(metadata);
    }

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
