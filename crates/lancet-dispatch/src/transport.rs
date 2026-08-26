// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! In-process Tokio channel transport for CEP JSON-RPC frames.

use crate::receipt::{ExecutionReceipt, InternalCallResponse};
use crate::registry::{Cancellation, EventSink, MethodRegistry};
use lancet_catalog::CatalogMethod;
use lancet_protocol::error::{ExecutionErrorData, to_jsonrpc_error};
use lancet_protocol::{
    CellEvidenceTriple, CepError, CepErrorCode, CepErrorData, ErrorObject, InvocationBinding,
    InvocationId, Retriability, ToolEvent, ToolResult,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

struct TransportRequestMarker {
    _private: bool,
}

/// Private per-request transport identity. Pointer identity keeps correlation
/// and cancellation separate from the durable domain InvocationId.
#[derive(Clone)]
struct TransportRequestKey(Arc<TransportRequestMarker>);

impl TransportRequestKey {
    fn new() -> Self {
        Self(Arc::new(TransportRequestMarker { _private: true }))
    }
}

impl PartialEq for TransportRequestKey {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for TransportRequestKey {}

impl Hash for TransportRequestKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

struct ServerRequest {
    request_key: TransportRequestKey,
    dispatch_started: Arc<AtomicBool>,
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
        let result = call.await;
        control.disarm();
        result
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
        let armed = Arc::new(AtomicBool::new(true));
        let control = CallControl {
            cancel_tx: cancel_tx.clone(),
            armed: Arc::clone(&armed),
        };
        let request_key = TransportRequestKey::new();
        let dispatch_started = Arc::new(AtomicBool::new(false));
        let response_state = Arc::clone(&dispatch_started);
        let request = ServerRequest {
            request_key,
            dispatch_started,
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
                armed,
            };
            if request_tx.send(request).await.is_err() {
                guard.complete();
                return Err(transport_request_unavailable());
            }
            let mut response_rx = response_rx;
            let response = tokio::select! {
                res = &mut response_rx => {
                    guard.complete();
                    res.map_err(|_| transport_response_unavailable(&response_state))??
                }
                _ = wait_cancelled(&mut cancel_rx) => {
                    let response = response_rx.await;
                    guard.complete();
                    response.map_err(|_| transport_response_unavailable(&response_state))??
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
    armed: Arc<AtomicBool>,
}

impl CallControl {
    /// Signal cancellation to the server-side handler.
    pub fn cancel(&self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            let _ = self.cancel_tx.send(true);
        }
    }

    fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }
}

impl Drop for CallControl {
    fn drop(&mut self) {
        if self.armed.swap(false, Ordering::AcqRel) {
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
    armed: Arc<AtomicBool>,
}

impl CancelGuard {
    fn complete(&mut self) {
        self.armed.store(false, Ordering::Release);
        self.cancel_tx = None;
    }
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        if let Some(tx) = self.cancel_tx.take()
            && self.armed.swap(false, Ordering::AcqRel)
        {
            let _ = tx.send(true);
        }
    }
}

fn transport_request_unavailable() -> CepError {
    CepError::internal_error("transport request unavailable before dispatch")
}

fn transport_response_unavailable(dispatch_started: &AtomicBool) -> CepError {
    if !dispatch_started.load(Ordering::Acquire) {
        return CepError::internal_error("transport response unavailable before dispatch");
    }
    CepError::new(
        CepErrorCode::OutcomeIndeterminate,
        "transport response unavailable after dispatch started",
        CepErrorData::Execution(ExecutionErrorData {
            command: String::new(),
            exit_code: None,
            stderr: None,
        }),
        Retriability::Permanent,
    )
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

    /// Record that authoritative admission durably crossed DispatchStarted.
    /// Response loss after this point is classified as outcome-indeterminate.
    /// Call only after the invocation store confirms the durable transition.
    pub fn mark_dispatch_started(&self) {
        self.request.dispatch_started.store(true, Ordering::Release);
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
        let _ = self.request.response_tx.send(Ok(response));
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
        let mut join_set: JoinSet<TransportRequestKey> = JoinSet::new();
        let mut cancel_senders: std::collections::HashMap<
            TransportRequestKey,
            watch::Sender<bool>,
        > = std::collections::HashMap::new();
        let mut task_requests: std::collections::HashMap<tokio::task::Id, TransportRequestKey> =
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
                            let request_key = request.request_key.clone();
                            cancel_senders.insert(request_key.clone(), request.cancel_tx.clone());
                            let call = ServerCall {
                                request,
                                event_tx: self.event_tx.clone(),
                            };
                            let handler = handler.clone();
                            let completed_key = request_key.clone();
                            let task = join_set.spawn(async move {
                                handler(call).await;
                                completed_key
                            });
                            task_requests.insert(task.id(), request_key);
                        }
                        None => {
                            shutdown = true;
                            for tx in cancel_senders.values() {
                                let _ = tx.send(true);
                            }
                        }
                    }
                }
                Some(res) = join_set.join_next_with_id(), if !join_set.is_empty() => {
                    match res {
                        Ok((task_id, request_key)) => {
                            task_requests.remove(&task_id);
                            cancel_senders.remove(&request_key);
                        }
                        Err(join_error) => {
                            if let Some(request_key) = task_requests.remove(&join_error.id()) {
                                cancel_senders.remove(&request_key);
                            }
                        }
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

        debug_assert!(cancel_senders.is_empty());
        debug_assert!(task_requests.is_empty());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_protocol::{BindingIdentity, SessionId, TurnId, arguments_digest};
    use std::time::Duration;

    fn binding(ordinal: u64, method: &str, params: &Value) -> InvocationBinding {
        InvocationBinding {
            invocation_id: InvocationId::parse(&format!("00000000-0000-4000-8000-{ordinal:012x}"))
                .expect("invocation id"),
            identity: BindingIdentity {
                session_id: SessionId::parse("00000000000000000000000000000001")
                    .expect("session id"),
                turn_id: TurnId::parse("00000000-0000-4000-8000-000000000001").expect("turn id"),
                model_step_ordinal: ordinal as u32,
                part_ordinal: 0,
                method: method.to_string(),
                descriptor_identity: "test-descriptor".into(),
                arguments_digest: arguments_digest(params),
            },
        }
    }

    fn cancelled(method: &str) -> CepError {
        CepError::tool_execution(
            CepErrorCode::CallCancelled,
            method,
            "cancelled",
            None,
            Retriability::Permanent,
        )
    }

    #[tokio::test]
    async fn caller_drop_and_shutdown_cancel_only_exact_transport_requests() {
        let (client, server) = CepTransport::pair(8);
        let shutdown = server.shutdown_handle();
        let (arrived_tx, mut arrived_rx) = mpsc::channel::<String>(4);
        let (cancelled_tx, mut cancelled_rx) = mpsc::channel::<String>(4);
        let server_task = tokio::spawn(async move {
            server
                .run_with_handler(move |call| {
                    let arrived_tx = arrived_tx.clone();
                    let cancelled_tx = cancelled_tx.clone();
                    async move {
                        let label = call.params()["label"]
                            .as_str()
                            .expect("test label")
                            .to_string();
                        let mut cancellation = call.cancellation();
                        arrived_tx.send(label.clone()).await.expect("arrival");
                        cancellation.cancelled().await;
                        cancelled_tx
                            .send(label)
                            .await
                            .expect("cancellation observation");
                        let method = call.method().to_string();
                        call.respond(Err(cancelled(&method)));
                    }
                })
                .await
                .expect("server run")
        });

        let shared = binding(1, "test.wait", &serde_json::json!({}));
        let (first, _first_control) = client.start_call_raw(
            shared.clone(),
            "test.wait".into(),
            serde_json::json!({"label": "first"}),
        );
        let (second, _second_control) = client.start_call_raw(
            shared,
            "test.wait".into(),
            serde_json::json!({"label": "second"}),
        );
        let first_task = tokio::spawn(first);
        let second_task = tokio::spawn(second);

        let mut arrivals = [
            arrived_rx.recv().await.expect("first arrival"),
            arrived_rx.recv().await.expect("second arrival"),
        ];
        arrivals.sort();
        assert_eq!(arrivals, ["first", "second"]);

        first_task.abort();
        assert!(first_task.await.expect_err("caller dropped").is_cancelled());
        assert_eq!(cancelled_rx.recv().await.as_deref(), Some("first"));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), cancelled_rx.recv())
                .await
                .is_err(),
            "dropping the first caller must not cancel the same-InvocationId peer"
        );

        shutdown.send(true).expect("shutdown");
        let second_err = tokio::time::timeout(Duration::from_secs(1), second_task)
            .await
            .expect("second request cancelled on shutdown")
            .expect("second caller task")
            .expect_err("shutdown is authoritative cancellation");
        assert_eq!(second_err.code, CepErrorCode::CallCancelled);
        assert_eq!(cancelled_rx.recv().await.as_deref(), Some("second"));
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("server drains all private request keys")
            .expect("server task");
    }

    #[tokio::test]
    async fn response_loss_is_typed_by_dispatch_boundary_and_join_errors_are_redacted() {
        let (client, server) = CepTransport::pair(8);
        let shutdown = server.shutdown_handle();
        let server_task = tokio::spawn(async move {
            server
                .run_with_handler(|call| async move {
                    match call.params()["phase"].as_str().expect("phase") {
                        "before" => panic!("pre-admission /private/repository secret"),
                        "after" => {
                            call.mark_dispatch_started();
                            panic!("post-admission /private/repository secret");
                        }
                        "ok" => call.respond(Ok(serde_json::json!({"ok": true}))),
                        other => panic!("unexpected phase: {other}"),
                    }
                })
                .await
                .expect("server run")
        });

        let before_params = serde_json::json!({"phase": "before"});
        let before = client
            .call_raw(
                binding(2, "test.response", &before_params),
                "test.response",
                before_params,
            )
            .await
            .expect_err("sender loss before admission is deterministic");
        assert_eq!(before.code, CepErrorCode::ExecutionFailed);
        assert_eq!(
            before.message,
            "transport response unavailable before dispatch"
        );
        let public_before = serde_json::to_string(&before).expect("serialize public error");
        assert!(!public_before.contains("private"));
        assert!(!public_before.contains("secret"));

        let ok_params = serde_json::json!({"phase": "ok"});
        assert_eq!(
            client
                .call_raw(
                    binding(3, "test.response", &ok_params),
                    "test.response",
                    ok_params,
                )
                .await
                .expect("server survives pre-admission worker panic"),
            serde_json::json!({"ok": true})
        );

        let after_params = serde_json::json!({"phase": "after"});
        let after = client
            .call_raw(
                binding(4, "test.response", &after_params),
                "test.response",
                after_params,
            )
            .await
            .expect_err("sender loss after DispatchStarted is indeterminate");
        assert_eq!(after.code, CepErrorCode::OutcomeIndeterminate);
        assert_eq!(
            after.message,
            "transport response unavailable after dispatch started"
        );
        assert_eq!(after.retriability, Retriability::Permanent);
        let public_after = serde_json::to_string(&after).expect("serialize public error");
        assert!(!public_after.contains("private"));
        assert!(!public_after.contains("secret"));

        let final_params = serde_json::json!({"phase": "ok"});
        client
            .call_raw(
                binding(5, "test.response", &final_params),
                "test.response",
                final_params,
            )
            .await
            .expect("server survives post-admission worker panic");

        shutdown.send(true).expect("shutdown");
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("server shutdown")
            .expect("server task");
    }
}
