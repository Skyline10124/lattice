//! In-process Tokio channel transport for CEP JSON-RPC frames.

use crate::registry::{call_cancelled, Cancellation, EventSink, HandlerContext, MethodRegistry};
use lancet_protocol::{
    error::to_jsonrpc_error, CatalogMethod, CepError, Id, ToolEvent, ToolResult,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinSet;

type BoxCallFuture<T> = Pin<Box<dyn Future<Output = Result<T, CepError>> + Send>>;
type BoxValueCallFuture = BoxCallFuture<Value>;

/// Factory for in-process client/server transport pairs.
pub struct LancetTransport;

impl LancetTransport {
    /// Create a connected client/server pair backed by Tokio channels.
    pub fn pair(buffer: usize) -> (LancetClient, LancetServer) {
        let (request_tx, request_rx) = mpsc::channel(buffer);
        let (event_tx, event_rx) = mpsc::channel(buffer);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let client = LancetClient {
            request_tx,
            event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
            next_id: Arc::new(AtomicU64::new(1)),
        };

        let server = LancetServer {
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
pub struct LancetClient {
    request_tx: mpsc::Sender<ServerRequest>,
    event_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<ToolEvent>>>,
    next_id: Arc<AtomicU64>,
}

impl LancetClient {
    /// Issue a typed catalog method call.
    pub async fn call<M>(&self, params: M::Params) -> Result<M::Result, CepError>
    where
        M: CatalogMethod,
        M::Params: Serialize,
        M::Result: DeserializeOwned,
    {
        let (call, _control) = self.start_call::<M>(params);
        match call.await {
            Ok(result) => {
                _control.disarm();
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

/// Server-side transport handle.
pub struct LancetServer {
    request_rx: mpsc::Receiver<ServerRequest>,
    event_tx: mpsc::Sender<ToolEvent>,
    shutdown_rx: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
}

impl LancetServer {
    /// Clone the shutdown signal sender for out-of-band shutdown requests.
    pub fn shutdown_handle(&self) -> watch::Sender<bool> {
        self.shutdown_tx.clone()
    }

    /// Signal shutdown; in-flight handlers are cancelled and joined before `run` returns.
    pub fn request_shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    /// Run the server loop until the request channel closes or shutdown is signaled.
    ///
    /// On shutdown, all in-flight handlers receive cancellation and are joined before return.
    pub async fn run(mut self, registry: MethodRegistry) -> Result<(), CepError> {
        let mut join_set: JoinSet<u64> = JoinSet::new();
        let mut cancel_senders: HashMap<u64, watch::Sender<bool>> = HashMap::new();
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
                            let event_tx = self.event_tx.clone();
                            let registry = registry.clone();
                            join_set.spawn(async move {
                                handle_request(registry, request, event_tx).await;
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

async fn handle_request(
    registry: MethodRegistry,
    request: ServerRequest,
    event_tx: mpsc::Sender<ToolEvent>,
) {
    let ServerRequest {
        id,
        method,
        params,
        response_tx,
        cancel_tx,
    } = request;

    let cancel_rx = cancel_tx.subscribe();
    let ctx = HandlerContext {
        call_id: id,
        events: EventSink::new(event_tx, id),
        cancellation: Cancellation::new(cancel_rx),
    };

    let result = registry.dispatch(&method, params, ctx).await;

    let response = match result {
        Ok(result) => ToolResult::Success {
            result,
            id: Id::Number(id as i64),
        },
        Err(err) => {
            let (code, message, data) = to_jsonrpc_error(&err);
            ToolResult::Error {
                error: lancet_protocol::ErrorObject {
                    code,
                    message,
                    data: Some(data),
                },
                id: Id::Number(id as i64),
            }
        }
    };

    if response_tx.send(Ok(response)).is_err() {
        let _ = cancel_tx.send(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::wire_code_for_error;
    use crate::registry::MethodRegistry;
    use lancet_protocol::error::{jsonrpc, method_not_found};
    use lancet_protocol::methods::session::{
        TodoItem, TodoStatus, TodoWriteParams, TodoWriteResult,
    };
    use lancet_protocol::methods::TodoWrite;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::time::sleep;

    fn todo_registry<F, Fut>(handler: F) -> MethodRegistry
    where
        F: Fn(TodoWriteParams, HandlerContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<TodoWriteResult, CepError>> + Send + 'static,
    {
        let mut registry = MethodRegistry::new();
        registry
            .register_method::<TodoWrite, _, _>(handler)
            .unwrap();
        registry
    }

    #[tokio::test]
    async fn transport_round_trip_returns_typed_result() {
        let (client, server) = LancetTransport::pair(8);
        let registry = todo_registry(|params, _ctx| async move {
            Ok(TodoWriteResult {
                todos: params.todos,
            })
        });

        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

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

    #[tokio::test]
    async fn transport_structured_error_round_trip() {
        let (client, server) = LancetTransport::pair(8);
        let registry =
            todo_registry(|_params, _ctx| async move { Err(method_not_found("injected")) });

        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

        let err = client
            .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::METHOD_NOT_FOUND);

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn transport_unknown_raw_method_returns_32601() {
        let (client, server) = LancetTransport::pair(8);
        let registry = MethodRegistry::new();
        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

        let err = client
            .call_raw("crafted.unknown", serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::METHOD_NOT_FOUND);

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn transport_long_running_handler_has_no_transport_timeout() {
        let (client, server) = LancetTransport::pair(8);
        let registry = todo_registry(|params, _ctx| async move {
            sleep(Duration::from_millis(150)).await;
            Ok(TodoWriteResult {
                todos: params.todos,
            })
        });

        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

        let result = client
            .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
            .await
            .unwrap();
        assert!(result.todos.is_empty());

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn notification_events_stream_in_order_with_matching_call_id() {
        let (client, server) = LancetTransport::pair(16);
        let registry = todo_registry(|params, ctx| async move {
            ctx.events
                .emit("started", serde_json::json!({"phase": "begin"}))
                .await;
            ctx.events
                .emit("progress", serde_json::json!({"step": 1}))
                .await;
            ctx.events
                .emit("finished", serde_json::json!({"phase": "end"}))
                .await;
            Ok(TodoWriteResult {
                todos: params.todos,
            })
        });

        let events = client.events();
        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

        let call = client.call::<TodoWrite>(TodoWriteParams { todos: vec![] });
        let mut collected = Vec::new();
        let collect = async {
            while collected.len() < 3 {
                if let Some(event) = events.lock().await.recv().await {
                    collected.push(event);
                }
            }
        };

        let (response, _) = tokio::join!(call, collect);
        response.unwrap();

        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].method, "started");
        assert_eq!(collected[1].method, "progress");
        assert_eq!(collected[2].method, "finished");

        for event in &collected {
            assert_eq!(event.params["call_id"], 1);
        }

        assert_eq!(collected.last().unwrap().method, "finished");

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn call_completes_when_progress_events_are_not_drained() {
        let (client, server) = LancetTransport::pair(1);
        let registry = todo_registry(|params, ctx| async move {
            for step in 0..8 {
                ctx.events
                    .emit("progress", serde_json::json!({ "step": step }))
                    .await;
            }
            Ok(TodoWriteResult {
                todos: params.todos,
            })
        });

        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            client.call::<TodoWrite>(TodoWriteParams { todos: vec![] }),
        )
        .await
        .expect("call should not block when events are not drained")
        .unwrap();
        assert!(result.todos.is_empty());

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn cancellation_dropping_pending_call_signals_handler() {
        let observed = Arc::new(tokio::sync::Notify::new());
        let observed_clone = Arc::clone(&observed);
        let ready = Arc::new(tokio::sync::Notify::new());
        let ready_clone = Arc::clone(&ready);

        let (client, server) = LancetTransport::pair(8);
        let registry = todo_registry(move |_params, mut ctx| {
            let observed = Arc::clone(&observed_clone);
            let ready = Arc::clone(&ready_clone);
            async move {
                ready.notify_one();
                ctx.cancellation.cancelled().await;
                observed.notify_one();
                Err(call_cancelled())
            }
        });

        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });
        let notified = observed.notified();
        let ready_wait = ready.notified();

        let (call, control) = client.start_call::<TodoWrite>(TodoWriteParams { todos: vec![] });
        let call_task = tokio::spawn(call);

        tokio::time::timeout(Duration::from_secs(1), ready_wait)
            .await
            .expect("handler should reach cancellation wait point");
        drop(control);
        call_task.abort();
        let _ = call_task.await;

        tokio::time::timeout(Duration::from_secs(1), notified)
            .await
            .expect("handler should observe cancellation");

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn cancellation_explicit_returns_typed_error() {
        let (client, server) = LancetTransport::pair(8);
        let registry = todo_registry(|_params, mut ctx| async move {
            ctx.cancellation.cancelled().await;
            Err(call_cancelled())
        });

        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

        let (call, control) = client.start_call::<TodoWrite>(TodoWriteParams { todos: vec![] });
        tokio::pin!(call);
        sleep(Duration::from_millis(20)).await;
        control.cancel();
        let err = call.await.unwrap_err();
        assert_eq!(err.message, "call cancelled");

        drop(client);
        server_task.abort();
    }

    static ACTIVE_HANDLERS: AtomicUsize = AtomicUsize::new(0);

    #[tokio::test]
    async fn shutdown_cancels_in_flight_handler_and_joins() {
        let cleaned = Arc::new(tokio::sync::Notify::new());
        let cleaned_clone = Arc::clone(&cleaned);

        let (client, server) = LancetTransport::pair(8);
        let registry = todo_registry(move |_params, mut ctx| {
            let cleaned = Arc::clone(&cleaned_clone);
            async move {
                ctx.cancellation.cancelled().await;
                cleaned.notify_one();
                Err(call_cancelled())
            }
        });

        let notified = cleaned.notified();
        let shutdown = server.shutdown_handle();
        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

        let (call, _control) = client.start_call::<TodoWrite>(TodoWriteParams { todos: vec![] });
        tokio::pin!(call);
        tokio::select! {
            _ = &mut call => panic!("unexpected call completion"),
            _ = sleep(Duration::from_millis(50)) => {}
        }
        shutdown.send(true).ok();
        drop(client);

        tokio::time::timeout(Duration::from_secs(1), notified)
            .await
            .expect("handler should be cancelled on shutdown");

        let _ = server_task.await;
    }

    #[tokio::test]
    async fn shutdown_cancels_remaining_handler_after_earlier_call_completes() {
        let hanging_ready = Arc::new(tokio::sync::Notify::new());
        let hanging_ready_clone = Arc::clone(&hanging_ready);
        let cancelled = Arc::new(tokio::sync::Notify::new());
        let cancelled_clone = Arc::clone(&cancelled);

        let (client, server) = LancetTransport::pair(8);
        let registry = todo_registry(move |params, mut ctx| {
            let hanging_ready = Arc::clone(&hanging_ready_clone);
            let cancelled = Arc::clone(&cancelled_clone);
            async move {
                let marker = params
                    .todos
                    .first()
                    .map(|todo| todo.content.as_str())
                    .unwrap_or_default();
                if marker == "fast" {
                    return Ok(TodoWriteResult {
                        todos: params.todos,
                    });
                }

                hanging_ready.notify_one();
                ctx.cancellation.cancelled().await;
                cancelled.notify_one();
                Err(call_cancelled())
            }
        });

        let shutdown = server.shutdown_handle();
        let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });

        let (hanging_call, _hanging_control) = client.start_call::<TodoWrite>(TodoWriteParams {
            todos: vec![TodoItem {
                content: "hang".into(),
                status: TodoStatus::InProgress,
            }],
        });
        let hanging_task = tokio::spawn(hanging_call);

        tokio::time::timeout(Duration::from_secs(1), hanging_ready.notified())
            .await
            .expect("hanging handler should start");

        let fast = client
            .call::<TodoWrite>(TodoWriteParams {
                todos: vec![TodoItem {
                    content: "fast".into(),
                    status: TodoStatus::Pending,
                }],
            })
            .await
            .unwrap();
        assert_eq!(fast.todos[0].content, "fast");

        sleep(Duration::from_millis(20)).await;
        shutdown.send(true).ok();
        drop(client);

        tokio::time::timeout(Duration::from_secs(1), cancelled.notified())
            .await
            .expect("remaining handler should be cancelled on shutdown");

        let err = hanging_task.await.unwrap().unwrap_err();
        assert_eq!(err.message, "call cancelled");
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("server should join all handlers")
            .unwrap();
    }

    #[tokio::test]
    async fn shutdown_stress_leaves_no_tracked_handler_tasks() {
        for _ in 0..50 {
            let (client, server) = LancetTransport::pair(8);
            let registry = todo_registry(|params, _ctx| async move {
                ACTIVE_HANDLERS.fetch_add(1, Ordering::SeqCst);
                sleep(Duration::from_millis(5)).await;
                ACTIVE_HANDLERS.fetch_sub(1, Ordering::SeqCst);
                Ok(TodoWriteResult {
                    todos: params.todos,
                })
            });

            let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });
            let _ = client
                .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
                .await;
            drop(client);
            server_task.abort();
        }

        assert_eq!(ACTIVE_HANDLERS.load(Ordering::SeqCst), 0);
    }

    #[cfg(unix)]
    mod unix_stress {
        use super::*;
        use std::fs;
        use std::process::Stdio;
        use tokio::process::Command;

        fn open_fd_count() -> usize {
            fs::read_dir("/proc/self/fd")
                .map(|entries| entries.count())
                .unwrap_or(0)
        }

        #[tokio::test]
        async fn cancellation_stress_kills_child_and_reclaims_resources() {
            let initial_fds = open_fd_count();

            for _ in 0..20 {
                let (client, server) = LancetTransport::pair(8);
                let child_done = Arc::new(tokio::sync::Notify::new());
                let child_done_clone = Arc::clone(&child_done);

                let mut registry = MethodRegistry::new();
                registry
                    .register_method::<TodoWrite, _, _>(move |_params, mut ctx| {
                        let child_done = Arc::clone(&child_done_clone);
                        async move {
                            ACTIVE_HANDLERS.fetch_add(1, Ordering::SeqCst);

                            let mut child = Command::new("sleep")
                                .arg("600")
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .spawn()
                                .expect("spawn sleep");

                            let pid = child.id().expect("child pid");

                            ctx.cancellation.cancelled().await;
                            let _ = child.kill().await;
                            let _ = child.wait().await;

                            assert!(
                                !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                                "child process {pid} still alive after kill"
                            );

                            ACTIVE_HANDLERS.fetch_sub(1, Ordering::SeqCst);
                            child_done.notify_one();
                            Err(call_cancelled())
                        }
                    })
                    .unwrap();

                let server_task = tokio::spawn(async move { server.run(registry).await.unwrap() });
                let child_done_wait = child_done.notified();

                let (call, control) =
                    client.start_call::<TodoWrite>(TodoWriteParams { todos: vec![] });
                tokio::pin!(call);
                sleep(Duration::from_millis(30)).await;
                control.cancel();
                let _ = call.await;

                tokio::time::timeout(Duration::from_secs(2), child_done_wait)
                    .await
                    .expect("handler should clean up child on cancel");

                drop(client);
                server_task.abort();
            }

            let final_fds = open_fd_count();
            assert!(
                final_fds <= initial_fds + 2,
                "fd leak: initial={initial_fds} final={final_fds}"
            );
            assert_eq!(ACTIVE_HANDLERS.load(Ordering::SeqCst), 0);
        }
    }
}
