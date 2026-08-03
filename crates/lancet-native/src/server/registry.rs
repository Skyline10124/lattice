// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native handler context and registry type binding.

use crate::server::call_durable_metadata::CallDurableMetadata;
use lancet_audit::{CepAuditContext, CepAuditStore, RejectionRecord};
use lancet_authority::policy::CepTurnContext;
use lancet_authority::policy::default::DefaultPolicyHook;
pub use lancet_dispatch::{Cancellation, EventSink};
use lancet_protocol::policy::CepMode;
use lancet_protocol::policy::PolicyHook;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Method registry specialized to native CEP handler context.
pub type MethodRegistry = lancet_dispatch::MethodRegistry<HandlerContext>;

/// Per-call context passed to native registered handlers.
///
/// There is no backend identity, Upper identity, or replaceable workspace
/// implementation on the native call context. Component calls receive their
/// concrete Local Workspace authority through Tool Host imports instead.
#[derive(Clone)]
pub struct HandlerContext {
    pub call_id: u64,
    pub events: EventSink,
    pub cancellation: Cancellation,
    pub cep_context: Arc<CepTurnContext>,
    pub turn_id: String,
    pub(crate) durable_metadata: CallDurableMetadata,
    pub effect_ledger: lancet_authority::effect::ledger::EffectLedger,
    pub authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot,
    pub policy_hook: Arc<dyn PolicyHook>,
    pub default_policy_hook: Option<Arc<DefaultPolicyHook>>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    pub audit: Arc<CepAuditStore>,
    pub cep_mode: CepMode,
}

impl std::fmt::Debug for HandlerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandlerContext")
            .field("call_id", &self.call_id)
            .field("events", &self.events)
            .field("cancellation", &self.cancellation)
            .field("cep_context", &self.cep_context)
            .field("turn_id", &self.turn_id)
            .field("effect_ledger", &self.effect_ledger)
            .field("authorized_region", &self.authorized_region)
            .field("policy_hook", &"<dyn PolicyHook>")
            .field("default_policy_hook", &self.default_policy_hook)
            .field("audit_context", &self.audit_context)
            .field("cep_mode", &self.cep_mode)
            .finish()
    }
}

impl HandlerContext {
    /// Add compact metadata that will be persisted to the call's durable audit row.
    pub fn record_durable_metadata(&self, metadata: Value) {
        self.durable_metadata.record(metadata);
    }

    /// Merge typed rejection evidence; coarse `native_handler_denied` is set at finalize time.
    pub fn record_rejection(&self, record: RejectionRecord) {
        self.record_durable_metadata(json!({
            "rejection_stage": record.rejection_stage.as_str(),
            "denial_source": record.denial_source,
        }));
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

    mod durable_metadata {
        use super::*;
        use lancet_audit::{RejectionRecord, RejectionStage};
        use lancet_authority::policy::default::PolicyConfig;
        use serde_json::Value;

        fn test_ctx(root: &std::path::Path) -> HandlerContext {
            let default_hook = Arc::new(DefaultPolicyHook::new(
                PolicyConfig::defaults_for_project_root(root.to_path_buf()),
            ));
            HandlerContext {
                call_id: 1,
                events: EventSink::new(tokio::sync::mpsc::channel(1).0, 1),
                cancellation: Cancellation::new(tokio::sync::watch::channel(false).1),
                cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
                turn_id: "turn".into(),
                durable_metadata: CallDurableMetadata::default(),
                effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
                authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone(),
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(RwLock::new(CepAuditContext::default())),
                audit: Arc::new(CepAuditStore::in_memory().expect("audit")),
                cep_mode: CepMode::Off,
            }
        }

        fn metadata(ctx: &HandlerContext) -> Value {
            serde_json::from_str(&ctx.durable_metadata.take_json().expect("json")).unwrap()
        }

        #[test]
        fn policy_denial_records_policy_gate_stage() {
            let dir = tempfile::TempDir::new().unwrap();
            let ctx = test_ctx(dir.path());
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::PolicyGate,
                denial_source: "cep_policy".into(),
            });
            let meta = metadata(&ctx);
            assert_eq!(meta["rejection_stage"], "policy_gate");
            assert_eq!(meta["denial_source"], "cep_policy");
        }

        #[test]
        fn path_guard_denial_records_path_guard_stage() {
            let dir = tempfile::TempDir::new().unwrap();
            let ctx = test_ctx(dir.path());
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::PathGuard,
                denial_source: "guard_l1".into(),
            });
            let meta = metadata(&ctx);
            assert_eq!(meta["rejection_stage"], "path_guard");
            assert_eq!(meta["denial_source"], "guard_l1");
        }

        #[test]
        fn local_io_failure_records_local_io_stage() {
            let dir = tempfile::TempDir::new().unwrap();
            let ctx = test_ctx(dir.path());
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::LocalIo,
                denial_source: "local_workspace".into(),
            });
            let meta = metadata(&ctx);
            assert_eq!(meta["rejection_stage"], "local_io");
            assert_eq!(meta["denial_source"], "local_workspace");
        }

        #[test]
        fn success_has_no_rejection_fields() {
            let dir = tempfile::TempDir::new().unwrap();
            let ctx = test_ctx(dir.path());
            ctx.record_durable_metadata(json!({"policy_enforced": true}));
            let meta = metadata(&ctx);
            assert!(meta.get("rejection_stage").is_none());
            assert!(meta.get("denial_source").is_none());
        }
    }
}
