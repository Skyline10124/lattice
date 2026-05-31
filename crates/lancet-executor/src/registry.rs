//! Typed method registry for CEP catalog dispatch.

use crate::audit::{CepAuditContext, CepAuditStore};
use crate::call_durable_metadata::CallDurableMetadata;
use crate::envelope_store::ActiveEnvelopeStore;
use crate::invariants::InvariantCardIndex;
use crate::policy::default::DefaultPolicyHook;
use crate::policy::Phase21TurnContext;
use lancet_protocol::envelope::{BaseExecutionEnvelope, CepMode};
use lancet_protocol::error::{invalid_params, method_not_found, to_jsonrpc_error, StateErrorData};
use lancet_protocol::policy::PolicyHook;
use lancet_protocol::{
    CatalogMethod, CepError, CepErrorCode, CepErrorData, Retriability, ToolEvent,
};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, RwLock};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type ErasedHandler =
    Arc<dyn Fn(Value, HandlerContext) -> BoxFuture<'static, Result<Value, CepError>> + Send + Sync>;

/// Cancellation signal delivered to in-flight handlers.
#[derive(Clone, Debug)]
pub struct Cancellation {
    rx: watch::Receiver<bool>,
}

impl Cancellation {
    pub(crate) fn new(rx: watch::Receiver<bool>) -> Self {
        Self { rx }
    }

    /// Returns whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.rx.borrow()
    }

    /// Waits until cancellation is requested.
    pub async fn cancelled(&mut self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            if self.rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

/// Sink for streaming [`ToolEvent`] notifications during a call.
#[derive(Clone, Debug)]
pub struct EventSink {
    tx: mpsc::Sender<ToolEvent>,
    call_id: u64,
}

impl EventSink {
    pub(crate) fn new(tx: mpsc::Sender<ToolEvent>, call_id: u64) -> Self {
        Self { tx, call_id }
    }

    /// Emit a CEP notification tagged with this call's id.
    ///
    /// Progress events are best-effort: a closed or full client channel does not fail the handler.
    pub async fn emit(&self, method: impl Into<String>, mut params: Value) {
        if let Some(obj) = params.as_object_mut() {
            obj.entry("call_id".to_string())
                .or_insert_with(|| Value::from(self.call_id));
        } else {
            params = serde_json::json!({
                "call_id": self.call_id,
                "payload": params,
            });
        }

        let event = ToolEvent::new(method.into(), params);
        let _ = self.tx.try_send(event);
    }
}

/// Per-call context passed to registered handlers.
#[derive(Clone)]
pub struct HandlerContext {
    pub call_id: u64,
    pub events: EventSink,
    pub cancellation: Cancellation,
    pub base_envelope: BaseExecutionEnvelope,
    pub phase21: Arc<Phase21TurnContext>,
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
            .field("phase21", &self.phase21)
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

/// Typed registry of catalog method handlers.
#[derive(Clone, Default)]
pub struct MethodRegistry {
    handlers: Arc<HashMap<String, ErasedHandler>>,
}

impl MethodRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed handler for catalog method `M`.
    ///
    /// # Errors
    ///
    /// Returns [`CepErrorCode::DuplicateRegistration`] if `M::NAME` is already registered.
    ///
    /// Non-catalog types cannot be registered:
    ///
    /// ```compile_fail
    /// use lancet_executor::{HandlerContext, MethodRegistry};
    /// use lancet_protocol::{CepMethod, RequiredCapability};
    /// use lancet_protocol::envelope::ImpactKind;
    /// use schemars::JsonSchema;
    /// use serde::{Deserialize, Serialize};
    ///
    /// struct LocalOnly;
    ///
    /// #[derive(Serialize, Deserialize, JsonSchema)]
    /// struct P {}
    /// #[derive(Serialize, Deserialize, JsonSchema)]
    /// struct R {}
    ///
    /// impl CepMethod for LocalOnly {
    ///     const NAME: &'static str = "local.only";
    ///     const DESCRIPTION: &'static str = "not in catalog";
    ///     type Params = P;
    ///     type Result = R;
    ///     const IMPACT: ImpactKind = ImpactKind::WorkspaceObservation;
    ///     const CAPABILITY: RequiredCapability = RequiredCapability::Inspect;
    /// }
    ///
    /// let mut registry = MethodRegistry::new();
    /// registry.register_method::<LocalOnly, _, _>(|_params, _ctx| async {
    ///     Ok(R {})
    /// }).unwrap();
    /// ```
    pub fn register_method<M, F, Fut>(&mut self, handler: F) -> Result<(), CepError>
    where
        M: CatalogMethod,
        F: Fn(M::Params, HandlerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<M::Result, CepError>> + Send + 'static,
    {
        let name = M::NAME;
        if self.handlers.contains_key(name) {
            return Err(duplicate_registration(name));
        }

        let handler = Arc::new(handler);
        let erased: ErasedHandler = Arc::new(move |params, ctx| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let typed: M::Params = serde_json::from_value(params).map_err(|err| {
                    invalid_params(format!("invalid params for {}: {err}", M::NAME))
                })?;
                let result = handler(typed, ctx).await?;
                serde_json::to_value(result).map_err(|err| {
                    CepError::internal_error(format!("failed to serialize result: {err}"))
                })
            })
        });

        Arc::make_mut(&mut self.handlers).insert(name.to_string(), erased);
        Ok(())
    }

    /// Dispatch a method by name with raw JSON params.
    pub async fn dispatch(
        &self,
        method: &str,
        params: Value,
        ctx: HandlerContext,
    ) -> Result<Value, CepError> {
        let handler = self
            .handlers
            .get(method)
            .ok_or_else(|| method_not_found(method))?;
        handler(params, ctx).await
    }

    /// Returns true when `method` is registered.
    pub fn contains(&self, method: &str) -> bool {
        self.handlers.contains_key(method)
    }
}

/// Build a duplicate-registration state error.
pub fn duplicate_registration(method: impl Into<String>) -> CepError {
    let method = method.into();
    CepError::new(
        CepErrorCode::DuplicateRegistration,
        format!("method already registered: {method}"),
        CepErrorData::State(StateErrorData {
            resource: method,
            reason: "duplicate registration".into(),
        }),
        Retriability::Permanent,
    )
}

/// Build a client-visible cancellation error.
pub fn call_cancelled() -> CepError {
    CepError::new(
        CepErrorCode::ExecutionFailed,
        "call cancelled",
        CepErrorData::State(StateErrorData {
            resource: "call".into(),
            reason: "cancelled".into(),
        }),
        Retriability::Permanent,
    )
}

/// Map a registry error to its JSON-RPC wire code.
pub fn wire_code_for_error(err: &CepError) -> i32 {
    to_jsonrpc_error(err).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::envelope::BaseExecutionEnvelope;
    use lancet_protocol::error::jsonrpc;
    use lancet_protocol::methods::session::{
        TodoItem, TodoStatus, TodoWriteParams, TodoWriteResult,
    };
    use lancet_protocol::methods::TodoWrite;
    use lancet_protocol::CepMethod;

    fn test_handler_context(call_id: u64, event_tx: mpsc::Sender<ToolEvent>) -> HandlerContext {
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from("."),
            ),
        ));
        HandlerContext {
            call_id,
            events: EventSink::new(event_tx, call_id),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(".", "registry test"),
            phase21: Arc::new(Phase21TurnContext::defaults_for_project_root(".")),
            active_envelope: Arc::new(ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: CallDurableMetadata::default(),
            effect_ledger: crate::effect::ledger::EffectLedger::new(),
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: default_hook.clone() as Arc<dyn PolicyHook>,
            default_policy_hook: Some(default_hook),
            audit_context: Arc::new(RwLock::new(CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
                ..Default::default()
            })),
            audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
            invariant_index: Arc::new(crate::invariants::InvariantCardIndex::new(vec![])),
            cep_mode: CepMode::Off,
        }
    }

    fn sample_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        registry
            .register_method::<TodoWrite, _, _>(|params, _ctx| async move {
                Ok(TodoWriteResult {
                    todos: params.todos,
                })
            })
            .unwrap();
        registry
    }

    #[tokio::test]
    async fn registry_typed_handler_dispatch_returns_result() {
        let registry = sample_registry();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ctx = test_handler_context(1, event_tx);

        let params = serde_json::to_value(TodoWriteParams {
            todos: vec![TodoItem {
                content: "test".into(),
                status: TodoStatus::Pending,
            }],
        })
        .unwrap();

        let result = registry
            .dispatch(TodoWrite::NAME, params, ctx)
            .await
            .unwrap();
        let parsed: TodoWriteResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.todos.len(), 1);
    }

    #[tokio::test]
    async fn registry_invalid_params_returns_32602() {
        let registry = sample_registry();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ctx = test_handler_context(2, event_tx);

        let err = registry
            .dispatch(TodoWrite::NAME, serde_json::json!({"bad": true}), ctx)
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn registry_unknown_method_returns_32601() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = MethodRegistry::new();
        crate::register_phase20_legacy_backend(
            &mut registry,
            dir.path().to_str().unwrap(),
            reqwest::Client::new(),
        )
        .unwrap();

        let (event_tx, _event_rx) = mpsc::channel(8);
        let ctx = test_handler_context(3, event_tx);

        let err = registry
            .dispatch("not.in.catalog", serde_json::json!({}), ctx)
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::METHOD_NOT_FOUND);
    }

    #[test]
    fn registry_full_catalog_registered_via_public_helper() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = MethodRegistry::new();
        crate::register_phase20_legacy_backend(
            &mut registry,
            dir.path().to_str().unwrap(),
            reqwest::Client::new(),
        )
        .unwrap();

        for descriptor in lancet_protocol::catalog() {
            // Native handlers — not in Phase 20 legacy backend
            if descriptor.name.starts_with("audit.") || descriptor.name == "envelope.submit_intent"
            {
                continue;
            }
            assert!(registry.contains(descriptor.name));
        }
    }

    #[test]
    fn registry_duplicate_registration_returns_error() {
        let mut registry = MethodRegistry::new();
        registry
            .register_method::<TodoWrite, _, _>(|params, _ctx| async move {
                Ok(TodoWriteResult {
                    todos: params.todos,
                })
            })
            .unwrap();
        let err = registry
            .register_method::<TodoWrite, _, _>(|params, _ctx| async move {
                Ok(TodoWriteResult {
                    todos: params.todos,
                })
            })
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::DuplicateRegistration);
    }
}
