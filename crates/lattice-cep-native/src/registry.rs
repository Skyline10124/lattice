//! Native handler context and registry type binding.

use crate::call_durable_metadata::CallDurableMetadata;
use crate::envelope_store::ActiveEnvelopeStore;
use crate::invariants::InvariantCardIndex;
use crate::policy::default::DefaultPolicyHook;
use crate::policy::CepTurnContext;
use lattice_cep_audit::{CepAuditContext, CepAuditStore};
pub use lattice_cep_dispatch::{Cancellation, EventSink};
use lattice_cep_protocol::envelope::{BaseExecutionEnvelope, CepMode};
use lattice_cep_protocol::policy::PolicyHook;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Method registry specialized to native CEP handler context.
pub type MethodRegistry = lattice_cep_dispatch::MethodRegistry<HandlerContext>;

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
    pub effect_ledger: crate::effect::ledger::EffectLedger,
    pub authorized_region: crate::effect::ledger::AuthorizedRegionSlot,
    pub policy_hook: Arc<dyn PolicyHook>,
    pub default_policy_hook: Option<Arc<DefaultPolicyHook>>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    pub audit: Arc<CepAuditStore>,
    pub invariant_index: Arc<InvariantCardIndex>,
    pub cep_mode: CepMode,
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
            .finish()
    }
}

impl HandlerContext {
    /// Add compact metadata that will be persisted to the call's durable audit row.
    pub fn record_durable_metadata(&self, metadata: Value) {
        self.durable_metadata.record(metadata);
    }

    /// Record an observed effect into the per-call effect ledger.
    pub fn record_effect(&self, effect: crate::effect::ledger::ObservedEffect) {
        self.effect_ledger.record(effect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_registry::register_production_method_registry;
    use lattice_cep_catalog::catalog;

    #[test]
    fn registry_full_catalog_registered_via_production_helper() {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();

        for descriptor in catalog() {
            assert!(registry.contains(descriptor.name));
        }
    }
}
