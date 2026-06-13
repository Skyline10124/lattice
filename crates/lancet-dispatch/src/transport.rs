// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! In-process Tokio channel transport for CEP JSON-RPC frames.

use crate::registry::{Cancellation, EventSink, MethodRegistry, call_cancelled};
use lancet_catalog::CatalogMethod;
use lancet_protocol::{CepError, ErrorObject, Id, ToolEvent, ToolResult, error::to_jsonrpc_error};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

type BoxCallFuture<T> = Pin<Box<dyn Future<Output = Result<T, CepError>> + Send>>;
type BoxValueCallFuture = BoxCallFuture<Value>;
type BoxServerFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Runtime-specific server lifecycle used by [`CepServer::run`].
///
/// Implementations own policy, audit, effect checks, native context construction,
/// and any other production-specific behavior. The dispatch crate only provides
/// typed dispatch and transport plumbing.
pub trait ServerRuntime<C>: Clone + Send + Sync + 'static
where
    C: Send + 'static,
{
    fn handle_call(&self, registry: MethodRegistry<C>, call: ServerCall) -> BoxServerFuture;

    fn after_shutdown(&self) -> BoxServerFuture {
        Box::pin(async {})
    }
}

/// Factory for in-process client/server transport pairs.
pub struct CepTransport;

impl CepTransport {
    /// Create a connected client/server pair backed by Tokio channels.
    pub fn pair(buffer: usize) -> (CepClient, CepServer) {
        let (request_tx, request_rx) = mpsc::channel(buffer);
        let (event_tx, event_rx) = mpsc::channel(buffer);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let client = CepClient {
            request_tx,
            event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
            next_id: Arc::new(AtomicU64::new(1)),
        };

        let server = CepServer {
            request_rx,
            event_tx,
            shutdown_rx,
            shutdown_tx,
        };

        (client, server)
    }
}

struct ServerRequest {
    id: u64,
    method: String,
    params: Value,
    response_tx: oneshot::Sender<Result<ToolResult, CepError>>,
    cancel_tx: watch::Sender<bool>,
}

/// Client-side transport handle.
#[derive(Clone)]
pub struct CepClient {
    request_tx: mpsc::Sender<ServerRequest>,
    event_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<ToolEvent>>>,
    next_id: Arc<AtomicU64>,
}

impl CepClient {
    /// Issue a typed catalog method call.
    pub async fn call<M>(&self, params: M::Params) -> Result<M::Result, CepError>
    where
        M: CatalogMethod,
        M::Params: Serialize,
        M::Result: DeserializeOwned,
    {
        let (call, control) = self.start_call::<M>(params);
        match call.await {
            Ok(result) => {
                control.disarm();
                Ok(result)
            }
            Err(err) => Err(err),
        }
    }

    /// Begin a typed call; dropping the future or calling [`CallControl::cancel`] signals cancellation.
    #[doc(hidden)]
    pub fn start_call<M>(&self, params: M::Params) -> (BoxCallFuture<M::Result>, CallControl)
    where
        M: CatalogMethod,
        M::Params: Serialize,
        M::Result: DeserializeOwned,
    {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        let (raw, control) = self.start_call_raw(M::NAME.to_string(), params);
        let typed = async move {
            let value = raw.await?;
            serde_json::from_value(value).map_err(|err| {
                CepError::internal_error(format!("failed to deserialize result: {err}"))
            })
        };
        (Box::pin(typed), control)
    }

    /// Raw JSON-RPC call for test/support scenarios (unknown method probing).
    #[doc(hidden)]
    pub async fn call_raw(
        &self,
        method: impl Into<String>,
        params: Value,
    ) -> Result<Value, CepError> {
        let (call, control) = self.start_call_raw(method.into(), params);
        match call.await {
            Ok(value) => {
                control.disarm();
                Ok(value)
            }
            Err(err) => Err(err),
        }
    }

    fn start_call_raw(&self, method: String, params: Value) -> (BoxValueCallFuture, CallControl) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (response_tx, response_rx) = oneshot::channel();
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let control = CallControl {
            cancel_tx: cancel_tx.clone(),
            armed: Arc::clone(&armed),
        };

        let request = ServerRequest {
            id,
            method,
            params,
            response_tx,
            cancel_tx: cancel_tx.clone(),
        };

        let request_tx = self.request_tx.clone();
        let guard_cancel = cancel_tx;

        let fut = async move {
            let mut guard = CancelGuard {
                cancel_tx: Some(guard_cancel),
            };

            request_tx
                .send(request)
                .await
                .map_err(|_| CepError::internal_error("transport request channel closed"))?;

            let response = tokio::select! {
                res = response_rx => {
                    guard.complete();
                    res.map_err(|_| CepError::internal_error("response channel closed"))??
                }
                _ = wait_cancelled(&mut cancel_rx) => {
                    return Err(call_cancelled());
                }
            };

            match response {
                ToolResult::Success { result, .. } => Ok(result),
                ToolResult::Error { error, .. } => {
                    let cep = error.data.unwrap_or_else(|| {
                        CepError::internal_error(format!("{} ({})", error.message, error.code))
                    });
                    Err(cep)
                }
            }
        };

        (Box::pin(fut), control)
    }

    /// Receive the next streamed [`ToolEvent`], if any.
    pub async fn next_event(&self) -> Option<ToolEvent> {
        self.event_rx.lock().await.recv().await
    }

    /// Clone the event receiver handle for concurrent polling in tests.
    pub fn events(&self) -> Arc<tokio::sync::Mutex<mpsc::Receiver<ToolEvent>>> {
        Arc::clone(&self.event_rx)
    }
}

/// Explicit cancellation control for an in-flight call.
#[derive(Clone, Debug)]
pub struct CallControl {
    cancel_tx: watch::Sender<bool>,
    armed: Arc<std::sync::atomic::AtomicBool>,
}

impl CallControl {
    /// Signal cancellation to the server-side handler.
    pub fn cancel(&self) {
        let _ = self.cancel_tx.send(true);
    }

    fn disarm(&self) {
        self.armed
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for CallControl {
    fn drop(&mut self) {
        if self.armed.load(std::sync::atomic::Ordering::Acquire) {
            let _ = self.cancel_tx.send(true);
        }
    }
}

async fn wait_cancelled(rx: &mut watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            break;
        }
    }
}

/// Sends cancellation when dropped unless explicitly completed.
struct CancelGuard {
    cancel_tx: Option<watch::Sender<bool>>,
}

impl CancelGuard {
    fn complete(&mut self) {
        self.cancel_tx = None;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.cancel_tx.take() {
            let _ = tx.send(true);
        }
    }
}

/// A server-side call delivered by [`CepServer::run_with_handler`].
pub struct ServerCall {
    request: ServerRequest,
    event_tx: mpsc::Sender<ToolEvent>,
}

impl ServerCall {
    pub fn id(&self) -> u64 {
        self.request.id
    }

    pub fn method(&self) -> &str {
        &self.request.method
    }

    pub fn params(&self) -> &Value {
        &self.request.params
    }

    pub fn event_sink(&self) -> EventSink {
        EventSink::new(self.event_tx.clone(), self.request.id)
    }

    pub fn cancellation(&self) -> Cancellation {
        Cancellation::new(self.request.cancel_tx.subscribe())
    }

    /// Send a raw JSON dispatch result back to the client.
    pub fn respond(self, result: Result<Value, CepError>) {
        let response = match result {
            Ok(result) => ToolResult::Success {
                result,
                id: Id::Number(self.request.id as i64),
            },
            Err(err) => {
                let (code, message, data) = to_jsonrpc_error(&err);
                ToolResult::Error {
                    error: ErrorObject {
                        code,
                        message,
                        data: Some(data),
                    },
                    id: Id::Number(self.request.id as i64),
                }
            }
        };

        if self.request.response_tx.send(Ok(response)).is_err() {
            let _ = self.request.cancel_tx.send(true);
        }
    }
}

/// Server-side transport handle.
pub struct CepServer {
    request_rx: mpsc::Receiver<ServerRequest>,
    event_tx: mpsc::Sender<ToolEvent>,
    shutdown_rx: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
}

impl CepServer {
    /// Clone the shutdown signal sender for out-of-band shutdown requests.
    pub fn shutdown_handle(&self) -> watch::Sender<bool> {
        self.shutdown_tx.clone()
    }

    /// Signal shutdown; in-flight handlers are cancelled and joined before `run` returns.
    pub fn request_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Run the server loop until the request channel closes or shutdown is signaled.
    pub async fn run<C, R>(self, registry: MethodRegistry<C>, runtime: R) -> Result<(), CepError>
    where
        C: Send + 'static,
        R: ServerRuntime<C>,
    {
        let after = runtime.clone();
        self.run_with_handler(move |call| {
            let registry = registry.clone();
            runtime.clone().handle_call(registry, call)
        })
        .await?;
        after.after_shutdown().await;
        Ok(())
    }

    /// Run the server loop until the request channel closes or shutdown is signaled.
    ///
    /// The provided handler owns policy, audit, effect, and any other runtime-specific
    /// lifecycle. Dispatch does not depend on those native implementations.
    pub async fn run_with_handler<H, Fut>(mut self, handler: H) -> Result<(), CepError>
    where
        H: Fn(ServerCall) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut join_set: JoinSet<u64> = JoinSet::new();
        let mut cancel_senders: std::collections::HashMap<u64, watch::Sender<bool>> =
            std::collections::HashMap::new();
        let mut shutdown = false;

        loop {
            if shutdown && join_set.is_empty() {
                break;
            }

            tokio::select! {
                changed = self.shutdown_rx.changed(), if !shutdown => {
                    changed.ok();
                    shutdown = true;
                    self.request_rx.close();
                    for tx in cancel_senders.values() {
                        let _ = tx.send(true);
                    }
                }
                msg = self.request_rx.recv(), if !shutdown => {
                    match msg {
                        Some(request) => {
                            let request_id = request.id;
                            cancel_senders.insert(request_id, request.cancel_tx.clone());
                            let call = ServerCall {
                                request,
                                event_tx: self.event_tx.clone(),
                            };
                            let handler = handler.clone();
                            join_set.spawn(async move {
                                handler(call).await;
                                request_id
                            });
                        }
                        None => {
                            shutdown = true;
                            for tx in cancel_senders.values() {
                                let _ = tx.send(true);
                            }
                        }
                    }
                }
                Some(res) = join_set.join_next(), if !join_set.is_empty() => {
                    if let Ok(request_id) = res {
                        cancel_senders.remove(&request_id);
                    }
                }
                else => {
                    if shutdown && join_set.is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
        }

        while let Some(res) = join_set.join_next().await {
            let _ = res;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::MethodRegistry;
    use lancet_catalog::methods::TodoWrite;
    use lancet_catalog::methods::session::{
        TodoItem, TodoStatus, TodoWriteParams, TodoWriteResult,
    };

    #[tokio::test]
    async fn transport_round_trip_returns_typed_result() {
        let (client, server) = CepTransport::pair(8);
        let mut registry = MethodRegistry::<(EventSink, Cancellation)>::new();
        registry
            .register_method::<TodoWrite, _, _>(|params, _ctx| async move {
                Ok(TodoWriteResult {
                    todos: params.todos,
                })
            })
            .unwrap();

        let server_task = tokio::spawn(async move {
            server
                .run_with_handler(move |call| {
                    let registry = registry.clone();
                    async move {
                        let ctx = (call.event_sink(), call.cancellation());
                        let result = registry
                            .dispatch(call.method(), call.params().clone(), ctx)
                            .await;
                        call.respond(result);
                    }
                })
                .await
                .unwrap()
        });

        let result = client
            .call::<TodoWrite>(TodoWriteParams {
                todos: vec![TodoItem {
                    content: "ship".into(),
                    status: TodoStatus::InProgress,
                }],
            })
            .await
            .unwrap();

        assert_eq!(result.todos[0].content, "ship");
        drop(client);
        server_task.abort();
    }
}
