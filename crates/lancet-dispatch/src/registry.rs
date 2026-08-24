// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed method registry for CEP catalog dispatch.

use lancet_catalog::CatalogMethod;
use lancet_protocol::error::{StateErrorData, invalid_params, method_not_found, to_jsonrpc_error};
use lancet_protocol::{
    CepError, CepErrorCode, CepErrorData, InvocationId, Retriability, ToolEvent,
};
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

type ErasedHandler<C> =
    Arc<dyn Fn(Value, C) -> BoxFuture<'static, Result<Value, CepError>> + Send + Sync>;

/// Cancellation signal delivered to in-flight handlers.
#[derive(Clone, Debug)]
pub struct Cancellation {
    rx: watch::Receiver<bool>,
}

impl Cancellation {
    /// Create a cancellation handle from a watch receiver.
    pub fn new(rx: watch::Receiver<bool>) -> Self {
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
    invocation_id: InvocationId,
}

impl EventSink {
    /// Create an event sink for one invocation.
    pub fn new(tx: mpsc::Sender<ToolEvent>, invocation_id: InvocationId) -> Self {
        Self { tx, invocation_id }
    }

    /// Emit a CEP notification tagged with this invocation's id.
    ///
    /// Progress events are best-effort: a closed or full client channel does not fail the handler.
    pub async fn emit(&self, method: impl Into<String>, mut params: Value) {
        if let Some(obj) = params.as_object_mut() {
            obj.entry("invocation_id".to_string())
                .or_insert_with(|| Value::from(self.invocation_id.to_string()));
        } else {
            params = serde_json::json!({
                "invocation_id": self.invocation_id,
                "payload": params,
            });
        }

        let event = ToolEvent::new(method.into(), params);
        let _ = self.tx.try_send(event);
    }

    /// Emit a CEP notification synchronously, without the `&self` async
    /// context required by `emit`. Used by Component Tool Host imports
    /// (#27 (Codeberg #5) slice C), which run inside a synchronous wasmtime
    /// call.
    pub fn emit_sync(&self, method: impl Into<String>, mut params: Value) {
        if let Some(obj) = params.as_object_mut() {
            obj.entry("invocation_id".to_string())
                .or_insert_with(|| Value::from(self.invocation_id.to_string()));
        } else {
            params = serde_json::json!({
                "invocation_id": self.invocation_id,
                "payload": params,
            });
        }
        let event = ToolEvent::new(method.into(), params);
        let _ = self.tx.try_send(event);
    }
}

/// Typed registry of catalog method handlers.
pub struct MethodRegistry<C> {
    handlers: Arc<HashMap<String, ErasedHandler<C>>>,
}

impl<C> Clone for MethodRegistry<C> {
    fn clone(&self) -> Self {
        Self {
            handlers: Arc::clone(&self.handlers),
        }
    }
}

impl<C> Default for MethodRegistry<C> {
    fn default() -> Self {
        Self {
            handlers: Arc::new(HashMap::new()),
        }
    }
}

impl<C> MethodRegistry<C>
where
    C: Send + 'static,
{
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a typed handler for catalog method `M`.
    ///
    /// # Errors
    ///
    /// Returns [`CepErrorCode::DuplicateRegistration`] if `M::NAME` is already registered.
    pub fn register_method<M, F, Fut>(&mut self, handler: F) -> Result<(), CepError>
    where
        M: CatalogMethod,
        F: Fn(M::Params, C) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<M::Result, CepError>> + Send + 'static,
    {
        let name = M::NAME;
        if self.handlers.contains_key(name) {
            return Err(duplicate_registration(name));
        }
        M::EFFECT
            .validate_complete(name)
            .map_err(|reason| incomplete_effect_contract(name, reason))?;

        let handler = Arc::new(handler);
        let erased: ErasedHandler<C> = Arc::new(move |params, ctx| {
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
    pub async fn dispatch(&self, method: &str, params: Value, ctx: C) -> Result<Value, CepError> {
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

    /// Every registered method name, sorted.
    ///
    /// Effect-kind denial (both the pre-dispatch declared check and the
    /// post-dispatch observed check) keys on `lancet_catalog::catalog()`, so a
    /// method registered without a catalog descriptor is exempt from
    /// `[lancet.effects] deny` (GitHub #71). Callers use this to assert
    /// registry ⊆ catalog instead of maintaining a hand-written name list.
    pub fn registered_methods(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.handlers.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }
}

fn incomplete_effect_contract(method: &str, reason: String) -> CepError {
    CepError::new(
        CepErrorCode::SchemaMismatch,
        format!("method {method} has an incomplete effect contract: {reason}"),
        CepErrorData::State(StateErrorData {
            resource: method.to_string(),
            reason,
        }),
        Retriability::Permanent,
    )
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

/// Map a registry error to its JSON-RPC wire code.
pub fn wire_code_for_error(err: &CepError) -> i32 {
    to_jsonrpc_error(err).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_catalog::CepMethod;
    use lancet_catalog::test_method::{
        TestEcho, TestEchoItem, TestEchoParams, TestEchoResult, TestEchoStatus,
    };
    use lancet_protocol::error::jsonrpc;

    #[derive(Clone)]
    struct TestContext {
        events: EventSink,
    }

    fn invocation_id(ordinal: u64) -> InvocationId {
        InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
            .expect("test invocation id")
    }

    fn sample_registry() -> MethodRegistry<TestContext> {
        let mut registry = MethodRegistry::new();
        registry
            .register_method::<TestEcho, _, _>(|params, ctx: TestContext| async move {
                ctx.events
                    .emit("test.todo", serde_json::json!({"seen": true}))
                    .await;
                Ok(TestEchoResult {
                    items: params.items,
                })
            })
            .unwrap();
        registry
    }

    #[tokio::test]
    async fn registry_typed_handler_dispatch_returns_result() {
        let registry = sample_registry();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ctx = TestContext {
            events: EventSink::new(event_tx, invocation_id(1)),
        };

        let params = serde_json::to_value(TestEchoParams {
            items: vec![TestEchoItem {
                content: "test".into(),
                status: TestEchoStatus::Pending,
            }],
        })
        .unwrap();

        let result = registry
            .dispatch(TestEcho::NAME, params, ctx)
            .await
            .unwrap();
        let parsed: TestEchoResult = serde_json::from_value(result).unwrap();
        assert_eq!(parsed.items.len(), 1);
    }

    #[tokio::test]
    async fn registry_invalid_params_returns_32602() {
        let registry = sample_registry();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ctx = TestContext {
            events: EventSink::new(event_tx, invocation_id(2)),
        };

        let err = registry
            .dispatch(TestEcho::NAME, serde_json::json!({"bad": true}), ctx)
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn registry_unknown_method_returns_32601() {
        let registry = sample_registry();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ctx = TestContext {
            events: EventSink::new(event_tx, invocation_id(3)),
        };

        let err = registry
            .dispatch("not.in.catalog", serde_json::json!({}), ctx)
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::METHOD_NOT_FOUND);
    }

    #[test]
    fn registry_duplicate_registration_returns_error() {
        let mut registry = MethodRegistry::<TestContext>::new();
        registry
            .register_method::<TestEcho, _, _>(|params, _ctx| async move {
                Ok(TestEchoResult {
                    items: params.items,
                })
            })
            .unwrap();
        let err = registry
            .register_method::<TestEcho, _, _>(|params, _ctx| async move {
                Ok(TestEchoResult {
                    items: params.items,
                })
            })
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::DuplicateRegistration);
    }
}
