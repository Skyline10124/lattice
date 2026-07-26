// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! In-process Tokio channel transport for CEP JSON-RPC frames.

use crate::registry::{Cancellation, EventSink, MethodRegistry};
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
    ///
    /// Delegates to [`CepClient::call_named`]: serializes `M::Params`, sends
    /// a raw JSON-RPC call for `M::NAME`, and deserializes the result into
    /// `M::Result`. The helper carries no JSON Schema — Component
    /// params/result validation is enforced by generated descriptor
    /// validators, not by this transport layer.
    pub async fn call<M>(&self, params: M::Params) -> Result<M::Result, CepError>
    where
        M: CatalogMethod,
        M::Params: Serialize,
        M::Result: DeserializeOwned,
    {
        self.call_named(M::NAME, params).await
    }

    /// Name-dynamic, type-static transport helper.
    ///
    /// Serializes `params` to a JSON value, sends a raw JSON-RPC call for
    /// `method`, and deserializes the result into `R`. Unlike [`call`], the
    /// method name is a runtime string — this is the path Component Tools
    /// use (their method names come from generated descriptors, not from
    /// `CatalogMethod::NAME`). The helper carries no JSON Schema; Component
    /// params/result validation is done by generated descriptor validators
    /// on the server side.
    pub async fn call_named<P, R>(&self, method: &str, params: P) -> Result<R, CepError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let raw_params = serde_json::to_value(params).map_err(|e| {
            CepError::invalid_params(format!("failed to serialize {method} params: {e}"))
        })?;
        let raw = self.call_raw(method.to_owned(), raw_params).await?;
        serde_json::from_value(raw).map_err(|e| {
            CepError::internal_error(format!("failed to deserialize {method} result: {e}"))
        })
    }

    /// Begin a typed call. Dropping the future or calling [`CallControl::cancel`]
    /// signals cancellation; a pending call then resolves with the server-mapped
    /// structured outcome (the server observes the same cancellation watch), not
    /// a client-synthesized error (#48).
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

    /// Begin a raw JSON call, returning the pending future and its cancellation
    /// control. After cancellation the future resolves with the server-mapped
    /// structured outcome (#48).
    #[doc(hidden)]
    pub fn start_call_raw(
        &self,
        method: String,
        params: Value,
    ) -> (BoxValueCallFuture, CallControl) {
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

            let mut response_rx = response_rx;
            let response = tokio::select! {
                res = &mut response_rx => {
                    guard.complete();
                    res.map_err(|_| CepError::internal_error("response channel closed"))??
                }
                _ = wait_cancelled(&mut cancel_rx) => {
                    // Cancellation requested: the server observes the same
                    // watch and maps the call outcome authoritatively (#48).
                    // Await its structured response instead of synthesizing
                    // a client-side error.
                    response_rx
                        .await
                        .map_err(|_| CepError::internal_error("response channel closed"))??
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
    use lancet_catalog::CepMethod;
    use lancet_catalog::methods::TodoWrite;
    use lancet_catalog::methods::session::{
        TodoItem, TodoStatus, TodoWriteParams, TodoWriteResult,
    };
    use lancet_protocol::error::jsonrpc;

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

    // `call_named` is the name-dynamic, type-static transport helper that
    // `call<M>` delegates to. It serializes params, calls `call_raw`, and
    // deserializes the result — without carrying a JSON Schema (Component
    // params/result validation is done by generated descriptor validators).

    fn echo_registry() -> MethodRegistry<(EventSink, Cancellation)> {
        let mut registry = MethodRegistry::<(EventSink, Cancellation)>::new();
        registry
            .register_method::<TodoWrite, _, _>(|params, _ctx| async move {
                Ok(TodoWriteResult {
                    todos: params.todos,
                })
            })
            .unwrap();
        registry
    }

    fn spawn_echo_server(
        registry: MethodRegistry<(EventSink, Cancellation)>,
    ) -> (CepClient, tokio::task::JoinHandle<()>) {
        let (client, server) = CepTransport::pair(8);
        let task = tokio::spawn(async move {
            let _ = server
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
                .await;
        });
        (client, task)
    }

    #[tokio::test]
    async fn call_named_round_trips_typed_result() {
        let (client, task) = spawn_echo_server(echo_registry());

        let result: TodoWriteResult = client
            .call_named(
                TodoWrite::NAME,
                TodoWriteParams {
                    todos: vec![TodoItem {
                        content: "named".into(),
                        status: TodoStatus::Pending,
                    }],
                },
            )
            .await
            .expect("call_named succeeds");

        assert_eq!(result.todos[0].content, "named");
        drop(client);
        task.abort();
    }

    #[tokio::test]
    async fn call_named_returns_invalid_params_when_params_fail_to_serialize() {
        // A type that cannot be serialized as JSON must produce a
        // structured invalid_params error, not a panic.
        let (client, task) = spawn_echo_server(echo_registry());

        // Serialize a map with a non-string key — serde_json rejects it.
        #[derive(Debug)]
        struct Unserializable;
        impl Serialize for Unserializable {
            fn serialize<S: serde::Serializer>(&self, _s: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("synthetic serialize failure"))
            }
        }

        let err = client
            .call_named::<Unserializable, TodoWriteResult>(TodoWrite::NAME, Unserializable)
            .await
            .expect_err("serialize failure must error");
        assert_eq!(
            lancet_protocol::error::to_jsonrpc_error(&err).0,
            jsonrpc::INVALID_PARAMS
        );
        drop(client);
        task.abort();
    }

    #[tokio::test]
    async fn call_named_returns_internal_error_when_result_fails_to_deserialize() {
        // The server returns a valid TodoWriteResult, but we ask for a
        // different type — the deserialize step must fail with an
        // ExecutionFailed (internal_error) CepError.
        let (client, task) = spawn_echo_server(echo_registry());

        let err = client
            .call_named::<TodoWriteParams, String>(
                TodoWrite::NAME,
                TodoWriteParams { todos: vec![] },
            )
            .await
            .expect_err("deserialize failure must error");
        assert_eq!(err.code, lancet_protocol::CepErrorCode::ExecutionFailed);
        assert!(err.message.contains("failed to deserialize"));
        drop(client);
        task.abort();
    }

    /// #48: after cancellation the pending call resolves with the
    /// server-mapped structured error, not a client-synthesized one. The
    /// server observes the same cancellation watch and is the authority
    /// for the call outcome.
    #[tokio::test]
    async fn cancel_resolves_with_server_structured_error() {
        let mut registry = MethodRegistry::<(EventSink, Cancellation)>::new();
        registry
            .register_method::<TodoWrite, _, _>(|_params, mut ctx| async move {
                // Simulate the Component path: wait until cancellation is
                // observed, then return the host-mapped `CallCancelled`.
                ctx.1.cancelled().await;
                Err(lancet_protocol::CepError::tool_execution(
                    lancet_protocol::CepErrorCode::CallCancelled,
                    "session.todo_write",
                    "cancelled",
                    None,
                    lancet_protocol::Retriability::Permanent,
                ))
            })
            .unwrap();
        let (client, task) = spawn_echo_server(registry);

        let (call, control) = client.start_call::<TodoWrite>(TodoWriteParams {
            todos: vec![TodoItem {
                content: "cancel-me".into(),
                status: TodoStatus::Pending,
            }],
        });
        control.cancel();

        let err = call.await.expect_err("cancelled call must fail");
        assert_eq!(
            err.code,
            lancet_protocol::CepErrorCode::CallCancelled,
            "client must see the server-mapped error code"
        );
        assert_eq!(err.message, "tool session.todo_write failed: cancelled");
        drop(client);
        task.abort();
    }

    /// #48: dropping the call future still signals cancellation to the
    /// server-side handler through `CancelGuard` (unchanged semantics).
    #[tokio::test]
    async fn drop_future_signals_cancellation_to_server() {
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel::<()>(1);
        let (observed_tx, mut observed_rx) = tokio::sync::mpsc::channel::<bool>(1);
        let mut registry = MethodRegistry::<(EventSink, Cancellation)>::new();
        registry
            .register_method::<TodoWrite, _, _>(move |params, mut ctx| {
                let started_tx = started_tx.clone();
                let observed_tx = observed_tx.clone();
                async move {
                    let _ = started_tx.send(()).await;
                    ctx.1.cancelled().await;
                    let _ = observed_tx.send(true).await;
                    Ok(TodoWriteResult {
                        todos: params.todos,
                    })
                }
            })
            .unwrap();
        let (client, task) = spawn_echo_server(registry);

        let (mut call, control) = client.start_call::<TodoWrite>(TodoWriteParams {
            todos: vec![TodoItem {
                content: "drop-me".into(),
                status: TodoStatus::Pending,
            }],
        });
        // Poll the call until the request reaches the server handler.
        tokio::select! {
            _ = &mut call => panic!("call cannot complete before the handler runs"),
            _ = started_rx.recv() => {}
        }
        drop(call);

        let observed = tokio::time::timeout(std::time::Duration::from_secs(5), observed_rx.recv())
            .await
            .expect("server observes cancellation")
            .expect("observation channel open");
        assert!(observed);
        drop(control);
        drop(client);
        task.abort();
    }
}
