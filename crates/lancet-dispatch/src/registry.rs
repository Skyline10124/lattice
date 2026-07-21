// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Typed method registry for CEP catalog dispatch.

use lancet_catalog::CatalogMethod;
use lancet_protocol::error::{StateErrorData, invalid_params, method_not_found, to_jsonrpc_error};
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability, ToolEvent};
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
    call_id: u64,
}

impl EventSink {
    /// Create an event sink for one call.
    pub fn new(tx: mpsc::Sender<ToolEvent>, call_id: u64) -> Self {
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

    /// Emit a CEP notification synchronously, without the `&self` async
    /// context required by `emit`. Used by Component Tool Host imports
    /// (#5 slice C), which run inside a synchronous wasmtime call.
    pub fn emit_sync(&self, method: impl Into<String>, mut params: Value) {
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
    use lancet_catalog::CepMethod;
    use lancet_catalog::methods::TodoWrite;
    use lancet_catalog::methods::session::{
        TodoItem, TodoStatus, TodoWriteParams, TodoWriteResult,
    };
    use lancet_protocol::error::jsonrpc;

    #[derive(Clone)]
    struct TestContext {
        events: EventSink,
    }

    fn sample_registry() -> MethodRegistry<TestContext> {
        let mut registry = MethodRegistry::new();
        registry
            .register_method::<TodoWrite, _, _>(|params, ctx: TestContext| async move {
                ctx.events
                    .emit("test.todo", serde_json::json!({"seen": true}))
                    .await;
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
        let ctx = TestContext {
            events: EventSink::new(event_tx, 1),
        };

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
        let ctx = TestContext {
            events: EventSink::new(event_tx, 2),
        };

        let err = registry
            .dispatch(TodoWrite::NAME, serde_json::json!({"bad": true}), ctx)
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn registry_unknown_method_returns_32601() {
        let registry = sample_registry();
        let (event_tx, _event_rx) = mpsc::channel(8);
        let ctx = TestContext {
            events: EventSink::new(event_tx, 3),
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
