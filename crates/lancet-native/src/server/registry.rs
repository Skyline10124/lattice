// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native handler context and registry type binding.

use crate::execution_backend::{ExecutionRouting, RoutingInvocationState};
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
    /// Upper (MSB) execution routing — always present since MSB is the sole
    /// execution path.
    pub execution: ExecutionRouting,
    /// Monotonic per-call adapter/backend invocation flags.
    pub routing_invocation: RoutingInvocationState,
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
            .field("execution", &"<ExecutionRouting>")
            .field("routing_invocation", &self.routing_invocation)
            .finish()
    }
}

pub(crate) fn build_routing_metadata(
    execution: Option<&ExecutionRouting>,
    routing: &RoutingInvocationState,
) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "adapter_preflight_invoked".into(),
        routing.adapter_preflight_invoked().into(),
    );
    obj.insert("backend_invoked".into(), routing.backend_invoked().into());
    if let Some(exec) = execution {
        obj.insert(
            "execution_backend".into(),
            exec.upper.execution_backend.clone().into(),
        );
        obj.insert(
            "upper_workspace_id".into(),
            exec.upper.upper_workspace_id.clone().into(),
        );
    }
    Value::Object(obj)
}

impl HandlerContext {
    /// Add compact metadata that will be persisted to the call's durable audit row.
    pub fn record_durable_metadata(&self, metadata: Value) {
        self.durable_metadata.record(metadata);
    }

    /// Record routing identity and accurate invocation flags for durable audit metadata.
    pub fn record_routing_metadata(&self) {
        self.record_durable_metadata(build_routing_metadata(
            Some(&self.execution),
            &self.routing_invocation,
        ));
    }

    /// Merge typed rejection evidence; coarse `native_handler_denied` is set at finalize time.
    pub fn record_rejection(&self, record: RejectionRecord) {
        self.record_durable_metadata(json!({
            "rejection_stage": record.rejection_stage.as_str(),
            "denial_source": record.denial_source,
        }));
    }

    /// Mark adapter preflight as invoked (monotonic).
    pub fn mark_adapter_preflight_invoked(&self) {
        self.routing_invocation.mark_adapter_preflight_invoked();
    }

    /// Mark Upper backend execution as invoked (monotonic).
    pub fn mark_backend_invoked(&self) {
        self.routing_invocation.mark_backend_invoked();
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

    mod routing_metadata {
        use super::*;
        use crate::execution_backend::{
            DirectoryEntry, ExecutionBackend, ExecutionBackendError, ExecutionRouting, FindRequest,
            GrepMatch, GrepRequest, PathFacts, PathIntent, UpperContext, WorkspacePath,
        };
        use lancet_audit::{RejectionRecord, RejectionStage};
        use serde_json::Value;
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::Arc;

        struct SpyBackend;

        impl ExecutionBackend for SpyBackend {
            fn backend_name(&self) -> &str {
                "spy"
            }

            fn resolve_path(
                &self,
                _path: &WorkspacePath,
                _intent: PathIntent,
            ) -> Pin<Box<dyn Future<Output = Result<PathFacts, ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "spy backend".into(),
                    })
                })
            }

            fn read_file(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "spy backend".into(),
                    })
                })
            }

            fn write_file(
                &self,
                _path: &WorkspacePath,
                _content: &[u8],
            ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "spy backend".into(),
                    })
                })
            }

            fn remove_file(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<Box<dyn Future<Output = Result<(), ExecutionBackendError>> + Send + '_>>
            {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "spy backend".into(),
                    })
                })
            }

            fn list_dir(
                &self,
                _path: &WorkspacePath,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<DirectoryEntry>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "spy backend".into(),
                    })
                })
            }

            fn grep(
                &self,
                _request: &GrepRequest,
            ) -> Pin<
                Box<dyn Future<Output = Result<Vec<GrepMatch>, ExecutionBackendError>> + Send + '_>,
            > {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "spy backend".into(),
                    })
                })
            }

            fn find_files(
                &self,
                _request: &FindRequest,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async {
                    Err(ExecutionBackendError::Unavailable {
                        message: "spy backend".into(),
                    })
                })
            }

            fn changed_paths(
                &self,
            ) -> Pin<
                Box<
                    dyn Future<Output = Result<Vec<WorkspacePath>, ExecutionBackendError>>
                        + Send
                        + '_,
                >,
            > {
                Box::pin(async { Ok(Vec::new()) })
            }
        }

        fn test_ctx(execution: ExecutionRouting) -> HandlerContext {
            let default_hook = Arc::new(DefaultPolicyHook::new(
                lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                    std::path::PathBuf::from("."),
                ),
            ));
            HandlerContext {
                call_id: 1,
                events: EventSink::new(tokio::sync::mpsc::channel(1).0, 1),
                cancellation: Cancellation::new(tokio::sync::watch::channel(false).1),
                cep_context: Arc::new(CepTurnContext::defaults_for_project_root(".")),
                turn_id: "turn".into(),
                durable_metadata: CallDurableMetadata::default(),
                effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
                authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
                policy_hook: default_hook.clone(),
                default_policy_hook: Some(default_hook),
                audit_context: Arc::new(RwLock::new(CepAuditContext::default())),
                audit: Arc::new(CepAuditStore::in_memory().expect("audit")),
                cep_mode: CepMode::Off,
                execution,
                routing_invocation: RoutingInvocationState::default(),
            }
        }

        fn metadata(ctx: &HandlerContext) -> Value {
            ctx.record_routing_metadata();
            serde_json::from_str(&ctx.durable_metadata.take_json().expect("json")).unwrap()
        }

        fn execution_routing() -> ExecutionRouting {
            ExecutionRouting {
                backend: Arc::new(SpyBackend),
                upper: UpperContext {
                    execution_backend: "spy".into(),
                    upper_workspace_id: "upper-test".into(),
                },
            }
        }

        #[test]
        fn policy_denial_precedes_adapter() {
            let ctx = test_ctx(execution_routing());
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::PolicyGate,
                denial_source: "cep_policy".into(),
            });
            let meta = metadata(&ctx);
            assert_eq!(meta["adapter_preflight_invoked"], false);
            assert_eq!(meta["backend_invoked"], false);
            assert_eq!(meta["rejection_stage"], "policy_gate");
            assert_eq!(meta["denial_source"], "cep_policy");
            assert_eq!(meta["execution_backend"], "spy");
        }

        #[test]
        fn path_denial_records_preflight_only() {
            let ctx = test_ctx(execution_routing());
            ctx.mark_adapter_preflight_invoked();
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::AdapterPreflight,
                denial_source: "guard_l1".into(),
            });
            let meta = metadata(&ctx);
            assert_eq!(meta["adapter_preflight_invoked"], true);
            assert_eq!(meta["backend_invoked"], false);
            assert_eq!(meta["rejection_stage"], "adapter_preflight");
        }

        #[test]
        fn backend_failure_records_both_invocations() {
            let ctx = test_ctx(execution_routing());
            ctx.mark_adapter_preflight_invoked();
            ctx.mark_backend_invoked();
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::BackendExecution,
                denial_source: "spy".into(),
            });
            let meta = metadata(&ctx);
            assert_eq!(meta["adapter_preflight_invoked"], true);
            assert_eq!(meta["backend_invoked"], true);
            assert_eq!(meta["rejection_stage"], "backend_execution");
        }

        #[test]
        fn verification_failure_after_backend() {
            let ctx = test_ctx(execution_routing());
            ctx.mark_adapter_preflight_invoked();
            ctx.mark_backend_invoked();
            ctx.record_rejection(RejectionRecord {
                rejection_stage: RejectionStage::VerificationFailure,
                denial_source: "post_backend_check".into(),
            });
            let meta = metadata(&ctx);
            assert_eq!(meta["adapter_preflight_invoked"], true);
            assert_eq!(meta["backend_invoked"], true);
            assert_eq!(meta["rejection_stage"], "verification_failure");
        }

        #[test]
        fn success_has_no_rejection_fields() {
            let ctx = test_ctx(execution_routing());
            ctx.mark_adapter_preflight_invoked();
            ctx.mark_backend_invoked();
            let meta = metadata(&ctx);
            assert!(meta.get("rejection_stage").is_none());
            assert!(meta.get("denial_source").is_none());
        }

        #[test]
        fn null_backend_records_backend_identity() {
            use crate::execution_backend::{ExecutionRouting, NullExecutionBackend, UpperContext};
            let routing = ExecutionRouting {
                backend: Arc::new(NullExecutionBackend),
                upper: UpperContext {
                    execution_backend: "null".into(),
                    upper_workspace_id: "null-upper".into(),
                },
            };
            let ctx = test_ctx(routing);
            let meta = metadata(&ctx);
            assert_eq!(meta["adapter_preflight_invoked"], false);
            assert_eq!(meta["backend_invoked"], false);
            assert_eq!(meta["execution_backend"], "null");
            assert_eq!(meta["upper_workspace_id"], "null-upper");
        }

        #[test]
        fn invocation_flags_are_monotonic() {
            let ctx = test_ctx(execution_routing());
            ctx.mark_adapter_preflight_invoked();
            ctx.mark_backend_invoked();
            ctx.mark_adapter_preflight_invoked();
            ctx.mark_backend_invoked();
            let meta = metadata(&ctx);
            assert_eq!(meta["adapter_preflight_invoked"], true);
            assert_eq!(meta["backend_invoked"], true);
        }
    }
}
