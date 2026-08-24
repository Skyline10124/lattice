// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! In-process Tokio channel transport for CEP JSON-RPC frames.

use crate::receipt::{ExecutionReceipt, InternalCallResponse};
use crate::registry::{Cancellation, EventSink, MethodRegistry};
use lancet_catalog::CatalogMethod;
use lancet_protocol::{
    CellEvidenceTriple, CepError, ErrorObject, InvocationBinding, InvocationId, ToolEvent,
    ToolResult, error::to_jsonrpc_error,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

type BoxCallFuture<T> = Pin<Box<dyn Future<Output = Result<T, CepError>> + Send>>;
type BoxValueCallFuture = BoxCallFuture<Value>;
type BoxReceiptCallFuture = BoxCallFuture<ExecutionReceipt>;
type BoxServerFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Runtime-specific server lifecycle used by [`CepServer::run`].
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
    binding: InvocationBinding,
    method: String,
    params: Value,
    response_tx: oneshot::Sender<Result<InternalCallResponse, CepError>>,
    cancel_tx: watch::Sender<bool>,
}

/// Client-side transport handle.
#[derive(Clone)]
pub struct CepClient {
    request_tx: mpsc::Sender<ServerRequest>,
    event_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<ToolEvent>>>,
}

impl CepClient {
    /// Issue a typed catalog method call.
    pub async fn call<M>(
        &self,
        binding: InvocationBinding,
        params: M::Params,
    ) -> Result<M::Result, CepError>
    where
        M: CatalogMethod,
        M::Params: Serialize,
        M::Result: DeserializeOwned,
    {
        self.call_named(binding, M::NAME, params).await
    }

    /// Name-dynamic, type-static transport helper.
    pub async fn call_named<P, R>(
        &self,
        binding: InvocationBinding,
        method: &str,
        params: P,
    ) -> Result<R, CepError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let raw_params = serde_json::to_value(params).map_err(|e| {
            CepError::invalid_params(format!("failed to serialize {method} params: {e}"))
        })?;
        let raw = self
            .call_raw(binding, method.to_owned(), raw_params)
            .await?;
        serde_json::from_value(raw).map_err(|e| {
            CepError::internal_error(format!("failed to deserialize {method} result: {e}"))
        })
    }

    /// Begin a typed call.
    #[doc(hidden)]
    pub fn start_call<M>(
        &self,
        binding: InvocationBinding,
        params: M::Params,
    ) -> (BoxCallFuture<M::Result>, CallControl)
    where
        M: CatalogMethod,
        M::Params: Serialize,
        M::Result: DeserializeOwned,
    {
        let params = serde_json::to_value(params).unwrap_or(Value::Null);
        let (raw, control) = self.start_call_raw(binding, M::NAME.to_string(), params);
        let typed = async move {
            let value = raw.await?;
            serde_json::from_value(value).map_err(|err| {
                CepError::internal_error(format!("failed to deserialize result: {err}"))
            })
        };
        (Box::pin(typed), control)
    }

    /// Raw JSON-RPC call for ordinary callers (result only).
    #[doc(hidden)]
    pub async fn call_raw(
        &self,
        binding: InvocationBinding,
        method: impl Into<String>,
        params: Value,
    ) -> Result<Value, CepError> {
        let receipt = self
            .call_raw_with_receipt(binding, method.into(), params)
            .await?;
        receipt.result
    }

    /// Coordinator-facing call: result plus content-free Cell evidence.
    #[doc(hidden)]
    pub async fn call_raw_with_receipt(
        &self,
        binding: InvocationBinding,
        method: impl Into<String>,
        params: Value,
    ) -> Result<ExecutionReceipt, CepError> {
        let (call, control) = self.start_call_raw_with_receipt(binding, method.into(), params);
        match call.await {
            Ok(receipt) => {
                control.disarm();
                Ok(receipt)
            }
            Err(err) => Err(err),
        }
    }

    /// Begin a raw JSON call, returning the pending future and its cancellation control.
    #[doc(hidden)]
    pub fn start_call_raw(
        &self,
        binding: InvocationBinding,
        method: String,
        params: Value,
    ) -> (BoxValueCallFuture, CallControl) {
        let (receipt_fut, control) = self.start_call_raw_with_receipt(binding, method, params);
        let fut = async move { receipt_fut.await?.result };
        (Box::pin(fut), control)
    }

    fn start_call_raw_with_receipt(
        &self,
        binding: InvocationBinding,
        method: String,
        params: Value,
    ) -> (BoxReceiptCallFuture, CallControl) {
        let (response_tx, response_rx) = oneshot::channel();
        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let armed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let control = CallControl {
            cancel_tx: cancel_tx.clone(),
            armed: Arc::clone(&armed),
        };
        let request = ServerRequest {
            binding,
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
                    response_rx
                        .await
                        .map_err(|_| CepError::internal_error("response channel closed"))??
                }
            };
            Ok(response.into_receipt())
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
    pub fn id(&self) -> InvocationId {
        self.request.binding.invocation_id
    }

    pub fn binding(&self) -> &InvocationBinding {
        &self.request.binding
    }

    pub fn method(&self) -> &str {
        &self.request.method
    }

    pub fn params(&self) -> &Value {
        &self.request.params
    }

    pub fn event_sink(&self) -> EventSink {
        EventSink::new(self.event_tx.clone(), self.id())
    }

    pub fn cancellation(&self) -> Cancellation {
        Cancellation::new(self.request.cancel_tx.subscribe())
    }

    /// Send a raw JSON dispatch result back to the client.
    pub fn respond(self, result: Result<Value, CepError>) {
        let method = self.request.method.clone();
        self.respond_with_evidence(result, CellEvidenceTriple::native_method(method));
    }

    /// Send a JSON-RPC result plus content-free Cell evidence.
    pub fn respond_with_evidence(
        self,
        result: Result<Value, CepError>,
        evidence: CellEvidenceTriple,
    ) {
        let id = self.request.binding.invocation_id;
        let tool_result = match result {
            Ok(result) => ToolResult::Success { result, id },
            Err(err) => {
                let (code, message, data) = to_jsonrpc_error(&err);
                ToolResult::Error {
                    error: ErrorObject {
                        code,
                        message,
                        data: Some(data),
                    },
                    id,
                }
            }
        };
        let response = InternalCallResponse {
            tool_result,
            evidence,
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
    pub async fn run_with_handler<H, Fut>(mut self, handler: H) -> Result<(), CepError>
    where
        H: Fn(ServerCall) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut join_set: JoinSet<InvocationId> = JoinSet::new();
        let mut cancel_senders: std::collections::HashMap<InvocationId, watch::Sender<bool>> =
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
                            let request_id = request.binding.invocation_id;
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
