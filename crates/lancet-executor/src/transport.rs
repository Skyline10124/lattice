//! In-process Tokio channel transport for CEP JSON-RPC frames.

use crate::audit::{
    impact_kind_for_method, impact_kind_label, AuditOutcome, AuditStart, CallAuditMetadata,
    CepAuditContext, CepAuditStore,
};
use crate::envelope_store::ActiveEnvelopeStore;
use crate::policy::default::DefaultPolicyHook;
use crate::policy::Phase21TurnContext;
use crate::registry::{call_cancelled, Cancellation, EventSink, HandlerContext, MethodRegistry};
use lancet_protocol::policy::PolicyHook;
use lancet_protocol::{
    envelope::BaseExecutionEnvelope, error::to_jsonrpc_error, CatalogMethod, CepError, Id,
    ToolEvent, ToolResult,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch, RwLock};
use tokio::task::JoinSet;

type BoxCallFuture<T> = Pin<Box<dyn Future<Output = Result<T, CepError>> + Send>>;
type BoxValueCallFuture = BoxCallFuture<Value>;

/// Server-side audit and lifecycle configuration for a turn-scoped runtime.
#[derive(Clone)]
pub struct ServerRunConfig {
    pub audit: Arc<CepAuditStore>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    pub base_envelope: BaseExecutionEnvelope,
    pub phase21: Arc<Phase21TurnContext>,
    /// Turn-scoped active envelope store for Phase 22 envelope lifecycle.
    pub active_envelope: Arc<ActiveEnvelopeStore>,
    /// Per-call audit metadata (bridge vs native); defaults to Phase 20 bridge.
    pub call_audit_metadata: CallAuditMetadata,
    /// When true, resolve audit metadata per method (Phase 21 native vs Phase 22 bridge).
    pub production_audit_routing: bool,
    /// Pluggable policy hook for capability/command/network/budget decisions.
    pub policy_hook: Arc<dyn PolicyHook>,
    /// Executor-owned default hook for evidence capture when the default implementation is installed.
    pub default_policy_hook: Option<Arc<DefaultPolicyHook>>,
}

impl ServerRunConfig {
    /// In-memory audit store with stable test identifiers and safe Phase 21 policy/budget defaults.
    pub fn in_memory_for_tests() -> Self {
        let base_envelope = BaseExecutionEnvelope::for_workspace_root(".", "test turn observation");
        let phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root("."));
        let default_hook = Arc::new(DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from("."),
            ),
        ));
        Self {
            audit: Arc::new(CepAuditStore::in_memory().expect("in-memory audit store")),
            audit_context: Arc::new(RwLock::new(CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
            })),
            base_envelope,
            phase21,
            active_envelope: Arc::new(ActiveEnvelopeStore::new()),
            call_audit_metadata: CallAuditMetadata::bridge_defaults(),
            production_audit_routing: false,
            policy_hook: default_hook.clone(),
            default_policy_hook: Some(default_hook),
        }
    }
}

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
    pub async fn run(
        mut self,
        registry: MethodRegistry,
        config: ServerRunConfig,
    ) -> Result<(), CepError> {
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
                            let config = config.clone();
                            join_set.spawn(async move {
                                handle_request(registry, request, event_tx, config).await;
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

        config.audit.await_pending().await;

        Ok(())
    }
}

async fn handle_request(
    registry: MethodRegistry,
    request: ServerRequest,
    event_tx: mpsc::Sender<ToolEvent>,
    config: ServerRunConfig,
) {
    let ServerRunConfig {
        audit,
        audit_context,
        base_envelope,
        phase21,
        active_envelope,
        call_audit_metadata,
        production_audit_routing,
        policy_hook,
        default_policy_hook,
    } = config;

    let ServerRequest {
        id,
        method,
        params,
        response_tx,
        cancel_tx,
    } = request;

    let call_audit_metadata = if production_audit_routing {
        CallAuditMetadata::for_production_dispatch(&method)
    } else {
        call_audit_metadata
    };

    let ctx_snapshot = audit_context.read().await.clone();
    let audit_start = match audit.start_call(&ctx_snapshot, id, &method, &call_audit_metadata) {
        Ok(start) => start,
        Err(_) => {
            let _ = response_tx.send(Err(CepError::internal_error("audit start failed")));
            return;
        }
    };

    emit_cep_call_started(
        &event_tx,
        id,
        &method,
        audit_start.audit_id,
        &call_audit_metadata,
    )
    .await;

    let cancel_rx = cancel_tx.subscribe();
    let durable_metadata = crate::call_durable_metadata::CallDurableMetadata::default();
    let turn_id = ctx_snapshot.turn_id.clone();
    let ctx = HandlerContext {
        call_id: id,
        events: EventSink::new(event_tx.clone(), id),
        cancellation: Cancellation::new(cancel_rx),
        base_envelope,
        phase21: Arc::clone(&phase21),
        active_envelope: Arc::clone(&active_envelope),
        turn_id,
        durable_metadata: durable_metadata.clone(),
        policy_hook: Arc::clone(&policy_hook),
        default_policy_hook: default_policy_hook.clone(),
        audit_context: Arc::clone(&audit_context),
    };

    let dispatch_result = registry.dispatch(&method, params, ctx).await;
    let durable_metadata_json = durable_metadata.take_json();

    finalize_audit_for_dispatch(
        &audit,
        audit_start,
        &method,
        &dispatch_result,
        &call_audit_metadata,
        durable_metadata_json.as_deref(),
    );
    let ok = dispatch_result.is_ok();
    let error_code = dispatch_result
        .as_ref()
        .err()
        .map(|err| format!("{:?}", err.code));

    let response = match &dispatch_result {
        Ok(result) => ToolResult::Success {
            result: result.clone(),
            id: Id::Number(id as i64),
        },
        Err(err) => {
            let (code, message, data) = to_jsonrpc_error(err);
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

    emit_cep_call_finished(
        &event_tx,
        id,
        &method,
        audit_start.audit_id,
        ok,
        error_code.as_deref(),
        &call_audit_metadata,
    )
    .await;

    if response_tx.send(Ok(response)).is_err() {
        let _ = cancel_tx.send(true);
    }
}

fn finalize_audit_for_dispatch(
    audit: &CepAuditStore,
    audit_start: AuditStart,
    method: &str,
    dispatch_result: &Result<Value, CepError>,
    call_metadata: &CallAuditMetadata,
    durable_metadata_json: Option<&str>,
) {
    let semantic_status = if call_metadata.semantic_stage == crate::audit::SEMANTIC_STAGE_SCAFFOLD {
        "scaffold_stub"
    } else if call_metadata.policy_enforced {
        "native_handler"
    } else {
        "bridged_legacy_body"
    };
    match dispatch_result {
        Ok(value) => {
            let summary = compact_success_summary(method, value);
            let metadata = durable_metadata_json
                .map(str::to_string)
                .or_else(|| compact_success_metadata(method, value, call_metadata));
            let _ = audit.finalize_call(
                audit_start,
                AuditOutcome::Success { summary },
                Some(semantic_status),
                metadata.as_deref(),
            );
        }
        Err(err) if err.message == "call cancelled" => {
            let _ = audit.finalize_call(audit_start, AuditOutcome::Cancelled, None, None);
        }
        Err(err) => {
            let code = format!("{:?}", err.code);
            let summary = compact_error_summary(err);
            let _ = audit.finalize_call(
                audit_start,
                AuditOutcome::Error { code, summary },
                Some("native_handler_denied"),
                durable_metadata_json,
            );
        }
    }
}

fn compact_success_summary(method: &str, value: &Value) -> String {
    match method {
        "session.todo_write" => {
            let count = value
                .get("todos")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            format!("todo_count={count}")
        }
        "workspace.read_file" => "read_file".to_string(),
        _ => format!("method={method}"),
    }
}

fn compact_success_metadata(
    method: &str,
    value: &Value,
    call_metadata: &CallAuditMetadata,
) -> Option<String> {
    let backend_invoked = call_metadata
        .backend_invoked
        .unwrap_or(!call_metadata.policy_enforced);
    match method {
        "session.todo_write" => {
            let count = value
                .get("todos")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            Some(format!(
                r#"{{"outcome":"todo_write","op_count":{count},"backend_invoked":{backend_invoked}}}"#
            ))
        }
        "web.fetch" => {
            let response_size = value
                .get("body")
                .and_then(|v| v.as_str())
                .map(|s| s.len())
                .unwrap_or(0);
            let truncated = value
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(format!(
                r#"{{"outcome":"web_fetch","response_size":{response_size},"truncated":{truncated},"backend_invoked":{backend_invoked}}}"#
            ))
        }
        "workspace.inspect" | "workspace.navigate" => {
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let routed = value
                .get("routed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Some(format!(
                r#"{{"outcome":"scaffold","status":"{status}","routed":{routed},"observation_payload_bytes":0,"budget_charged":0,"backend_invoked":{backend_invoked}}}"#
            ))
        }
        _ => Some(format!(
            r#"{{"method":"{method}","backend_invoked":{backend_invoked}}}"#
        )),
    }
}

fn compact_error_summary(err: &CepError) -> String {
    if err.message.starts_with("Method not found:") {
        "unknown method".to_string()
    } else {
        err.message.chars().take(120).collect()
    }
}

async fn emit_cep_call_started(
    event_tx: &mpsc::Sender<ToolEvent>,
    call_id: u64,
    method: &str,
    audit_id: i64,
    metadata: &CallAuditMetadata,
) {
    let impact_kind = impact_kind_for_method(method)
        .map(impact_kind_label)
        .unwrap_or("unknown");
    let event = ToolEvent::new(
        "cep.call_started".to_string(),
        json!({
            "call_id": call_id,
            "method": method,
            "impact_kind": impact_kind,
            "audit_id": audit_id,
            "semantic_stage": metadata.semantic_stage,
            "guard_stage": metadata.guard_stage,
            "policy_enforced": metadata.policy_enforced,
        }),
    );
    let _ = event_tx.try_send(event);
}

async fn emit_cep_call_finished(
    event_tx: &mpsc::Sender<ToolEvent>,
    call_id: u64,
    method: &str,
    audit_id: i64,
    ok: bool,
    error_code: Option<&str>,
    metadata: &CallAuditMetadata,
) {
    let mut payload = json!({
        "call_id": call_id,
        "method": method,
        "audit_id": audit_id,
        "ok": ok,
        "semantic_stage": metadata.semantic_stage,
        "guard_stage": metadata.guard_stage,
        "policy_enforced": metadata.policy_enforced,
    });
    if let Some(code) = error_code {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("error_code".to_string(), Value::String(code.to_string()));
        }
    }
    let event = ToolEvent::new("cep.call_finished".to_string(), payload);
    let _ = event_tx.try_send(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::{GUARD_STAGE, SEMANTIC_STAGE};
    use crate::registry::wire_code_for_error;
    use crate::registry::MethodRegistry;
    use lancet_protocol::error::{jsonrpc, method_not_found};
    use lancet_protocol::methods::session::{
        TodoItem, TodoStatus, TodoWriteParams, TodoWriteResult,
    };
    use lancet_protocol::methods::TodoWrite;
    use lancet_protocol::CepMethod;
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

        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
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

    #[tokio::test]
    async fn base_envelope_visible_in_handler_context() {
        use lancet_protocol::envelope::{BaseExecutionEnvelope, Capability};
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        let envelope =
            BaseExecutionEnvelope::for_workspace_root(&root, "phase20 handler observation");
        let expected_root = envelope.inner.scope.workspace_root.clone();
        let registry = todo_registry(move |_params, ctx| {
            let expected_root = expected_root.clone();
            async move {
                assert_eq!(
                    ctx.base_envelope.inner.scope.workspace_root.as_deref(),
                    expected_root.as_deref()
                );
                assert!(ctx
                    .base_envelope
                    .inner
                    .capabilities
                    .contains(&Capability::Inspect));
                assert!(ctx
                    .base_envelope
                    .inner
                    .capabilities
                    .iter()
                    .any(|cap| matches!(cap, Capability::ReadPath { .. })));
                Ok(TodoWriteResult { todos: vec![] })
            }
        });

        let (client, server) = LancetTransport::pair(8);
        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope = envelope;
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        client
            .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
            .await
            .unwrap();

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn transport_structured_error_round_trip() {
        let (client, server) = LancetTransport::pair(8);
        let registry =
            todo_registry(|_params, _ctx| async move { Err(method_not_found("injected")) });

        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });

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
        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });

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

        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });

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
        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });

        let call = client.call::<TodoWrite>(TodoWriteParams { todos: vec![] });
        let mut collected = Vec::new();
        let collect = async {
            while collected.len() < 5 {
                if let Some(event) = events.lock().await.recv().await {
                    collected.push(event);
                }
            }
        };

        let (response, _) = tokio::join!(call, collect);
        response.unwrap();

        assert_eq!(collected.len(), 5);
        assert_eq!(collected[0].method, "cep.call_started");
        assert_eq!(collected[1].method, "started");
        assert_eq!(collected[2].method, "progress");
        assert_eq!(collected[3].method, "finished");
        assert_eq!(collected[4].method, "cep.call_finished");

        for event in &collected {
            assert_eq!(event.params["call_id"], 1);
        }

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

        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });

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

        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });
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

        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });

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
    /// Prevents parallel stress tests from sharing `ACTIVE_HANDLERS` (crate tests run concurrently).
    static ACTIVE_HANDLERS_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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
        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });

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
        let server_task = tokio::spawn(async move {
            server
                .run(registry, ServerRunConfig::in_memory_for_tests())
                .await
                .unwrap()
        });

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
        let _guard = ACTIVE_HANDLERS_TEST_LOCK.lock().await;
        ACTIVE_HANDLERS.store(0, Ordering::SeqCst);
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

            let server_task = tokio::spawn(async move {
                server
                    .run(registry, ServerRunConfig::in_memory_for_tests())
                    .await
                    .unwrap()
            });
            let _ = client
                .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
                .await;
            drop(client);
            server_task.abort();
        }

        assert_eq!(ACTIVE_HANDLERS.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cep_audit_transport_success_appends_one_row() {
        let config = ServerRunConfig::in_memory_for_tests();
        let audit = Arc::clone(&config.audit);
        let (client, server) = LancetTransport::pair(8);
        let registry = todo_registry(|params, _ctx| async move {
            Ok(TodoWriteResult {
                todos: params.todos,
            })
        });
        let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

        let _ = client
            .call::<TodoWrite>(TodoWriteParams { todos: vec![] })
            .await
            .unwrap();
        drop(client);
        server_task.await.unwrap();

        assert_eq!(audit.row_count().unwrap(), 1);
        let row = audit.row_by_audit_id(1).unwrap();
        assert_eq!(row.method, TodoWrite::NAME);
        assert!(row.ended_at.is_some());
        assert_eq!(row.semantic_stage, SEMANTIC_STAGE);
        assert_eq!(row.guard_stage, GUARD_STAGE);
        assert!(!row.policy_enforced);
    }

    #[tokio::test]
    async fn cep_audit_transport_native_policy_denial_persists_metadata() {
        use crate::handlers::observation::register_workspace_observation_methods;
        use lancet_protocol::methods::workspace::{ReadFile, ReadFileParams};
        use lancet_protocol::CepErrorCode;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let mut registry = MethodRegistry::new();
        register_workspace_observation_methods(&mut registry).unwrap();

        let mut config = ServerRunConfig::in_memory_for_tests();
        config.production_audit_routing = true;
        config.phase21 =
            Arc::new(crate::policy::Phase21TurnContext::defaults_for_project_root(root));
        config.base_envelope =
            BaseExecutionEnvelope::for_workspace_root(root, "native audit denial test");
        let audit = Arc::clone(&config.audit);

        let (client, server) = LancetTransport::pair(8);
        let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

        let err = client
            .call::<ReadFile>(ReadFileParams {
                path: "../outside".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
        drop(client);
        server_task.await.unwrap();

        let row = audit.row_by_audit_id(1).unwrap();
        let metadata = row.metadata_json.expect("policy denial metadata");
        assert!(metadata.contains("cep_policy"));
        assert!(metadata.contains("denial_source"));
        assert!(row.policy_enforced);
    }

    #[tokio::test]
    async fn cep_audit_transport_unknown_method_appends_row() {
        let config = ServerRunConfig::in_memory_for_tests();
        let audit = Arc::clone(&config.audit);
        let (client, server) = LancetTransport::pair(8);
        let registry = MethodRegistry::new();
        let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

        let err = client
            .call_raw("crafted.unknown", serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::METHOD_NOT_FOUND);
        drop(client);
        server_task.await.unwrap();

        assert_eq!(audit.row_count().unwrap(), 1);
        assert!(audit.row_by_audit_id(1).unwrap().ended_at.is_some());
    }

    #[tokio::test]
    async fn cep_audit_transport_cancellation_finalizes_row() {
        let config = ServerRunConfig::in_memory_for_tests();
        let audit = Arc::clone(&config.audit);
        let (client, server) = LancetTransport::pair(8);
        let registry = todo_registry(|_params, mut ctx| async move {
            ctx.cancellation.cancelled().await;
            Err(call_cancelled())
        });
        let server_task = tokio::spawn(async move { server.run(registry, config).await.unwrap() });

        let (call, control) = client.start_call::<TodoWrite>(TodoWriteParams { todos: vec![] });
        sleep(Duration::from_millis(20)).await;
        control.cancel();
        let _ = call.await;
        drop(client);
        server_task.await.unwrap();

        assert_eq!(audit.row_count().unwrap(), 1);
        let row = audit.row_by_audit_id(1).unwrap();
        assert_eq!(row.result_or_error.as_deref(), Some("cancelled"));
        assert!(row.ended_at.is_some());
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
            let _guard = ACTIVE_HANDLERS_TEST_LOCK.lock().await;
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

                let server_task = tokio::spawn(async move {
                    server
                        .run(registry, ServerRunConfig::in_memory_for_tests())
                        .await
                        .unwrap()
                });
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
