//! Native CEP server lifecycle layered on dispatch transport.

use crate::call_durable_metadata::CallDurableMetadata;
use crate::envelope_store::ActiveEnvelopeStore;
use crate::invariants::InvariantCardIndex;
use crate::policy::default::{DefaultPolicyHook, PolicyConfig};
use crate::policy::CepTurnContext;
use crate::registry::{HandlerContext, MethodRegistry};
use lattice_cep_audit::{
    impact_kind_for_method, impact_kind_label, merge_finalize_metadata, AuditOutcome, AuditStart,
    CallAuditMetadata, CepAuditContext, CepAuditStore, SEMANTIC_STAGE_SCAFFOLD,
    SEMANTIC_STAGE_UNROUTED,
};
use lattice_cep_dispatch::{CepServer, ServerCall, ServerRuntime};
use lattice_cep_protocol::policy::PolicyHook;
use lattice_cep_protocol::{
    envelope::{BaseExecutionEnvelope, CepMode},
    CepError, CepErrorCode, CepErrorData, Retriability,
};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Server-side audit and lifecycle configuration for a turn-scoped native runtime.
#[derive(Clone)]
pub struct ServerRunConfig {
    pub audit: Arc<CepAuditStore>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    pub base_envelope: BaseExecutionEnvelope,
    pub cep_context: Arc<CepTurnContext>,
    /// Turn-scoped active envelope store for CEP envelope lifecycle.
    pub active_envelope: Arc<ActiveEnvelopeStore>,
    /// Pluggable policy hook for capability/command/network/budget decisions.
    pub policy_hook: Arc<dyn PolicyHook>,
    /// Executor-owned default hook for evidence capture when the default implementation is installed.
    pub default_policy_hook: Option<Arc<DefaultPolicyHook>>,
    /// CEP enforcement mode.
    pub cep_mode: CepMode,
    /// Invariant cards indexed for this turn/session.
    pub invariant_index: Arc<InvariantCardIndex>,
}

impl ServerRunConfig {
    /// In-memory audit store with stable test identifiers and safe CEP policy/budget defaults.
    pub fn in_memory_for_tests() -> Self {
        let base_envelope = BaseExecutionEnvelope::for_workspace_root(".", "test turn observation");
        let cep_context = Arc::new(CepTurnContext::defaults_for_project_root("."));
        let default_hook = Arc::new(DefaultPolicyHook::new(
            PolicyConfig::defaults_for_project_root(std::path::PathBuf::from(".")),
        ));
        Self {
            audit: Arc::new(CepAuditStore::in_memory().expect("in-memory audit store")),
            audit_context: Arc::new(RwLock::new(CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
                ..Default::default()
            })),
            base_envelope,
            cep_context,
            active_envelope: Arc::new(ActiveEnvelopeStore::new()),
            policy_hook: default_hook.clone(),
            default_policy_hook: Some(default_hook),
            cep_mode: CepMode::Off,
            invariant_index: Arc::new(InvariantCardIndex::new(vec![])),
        }
    }
}

impl ServerRuntime<HandlerContext> for ServerRunConfig {
    fn handle_call(
        &self,
        registry: MethodRegistry,
        call: ServerCall,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let config = self.clone();
        Box::pin(async move {
            handle_request(registry, call, config).await;
        })
    }

    fn after_shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let audit = Arc::clone(&self.audit);
        Box::pin(async move {
            audit.await_pending().await;
        })
    }
}

/// Convenience extension for running a dispatch server with the native CEP lifecycle.
pub trait CepServerNativeExt {
    fn run_native(
        self,
        registry: MethodRegistry,
        config: ServerRunConfig,
    ) -> Pin<Box<dyn Future<Output = Result<(), CepError>> + Send>>;
}

impl CepServerNativeExt for CepServer {
    fn run_native(
        self,
        registry: MethodRegistry,
        config: ServerRunConfig,
    ) -> Pin<Box<dyn Future<Output = Result<(), CepError>> + Send>> {
        Box::pin(async move { self.run(registry, config).await })
    }
}

async fn handle_request(registry: MethodRegistry, call: ServerCall, config: ServerRunConfig) {
    let ServerRunConfig {
        audit,
        audit_context,
        base_envelope,
        cep_context,
        active_envelope,
        policy_hook,
        default_policy_hook,
        cep_mode,
        invariant_index,
    } = config;

    let id = call.id();
    let method = call.method().to_string();
    let params = call.params().clone();
    let events = call.event_sink();
    let cancellation = call.cancellation();

    let call_audit_metadata = CallAuditMetadata::for_production_dispatch(&method);

    let ctx_snapshot = audit_context.read().await.clone();
    let audit_start = match audit.start_call(&ctx_snapshot, id, &method, &call_audit_metadata) {
        Ok(start) => start,
        Err(_) => {
            call.respond(Err(CepError::internal_error("audit start failed")));
            return;
        }
    };

    emit_cep_call_started(&events, &method, audit_start.audit_id, &call_audit_metadata).await;

    let durable_metadata = CallDurableMetadata::default();
    let effect_ledger = crate::effect::ledger::EffectLedger::new();
    let authorized_region_slot = crate::effect::ledger::AuthorizedRegionSlot::new();
    let turn_id = ctx_snapshot.turn_id.clone();
    let ctx = HandlerContext {
        call_id: id,
        events: events.clone(),
        cancellation,
        base_envelope,
        cep_context: Arc::clone(&cep_context),
        active_envelope: Arc::clone(&active_envelope),
        turn_id,
        durable_metadata: durable_metadata.clone(),
        effect_ledger: effect_ledger.clone(),
        authorized_region: authorized_region_slot.clone(),
        policy_hook: Arc::clone(&policy_hook),
        default_policy_hook: default_policy_hook.clone(),
        audit_context: Arc::clone(&audit_context),
        audit: Arc::clone(&audit),
        invariant_index: Arc::clone(&invariant_index),
        cep_mode: cep_mode.clone(),
    };

    let dispatch_result = registry.dispatch(&method, params, ctx).await;
    let observed_effects = effect_ledger.snapshot();

    let final_result = apply_effect_check(
        &method,
        dispatch_result,
        &observed_effects,
        &authorized_region_slot,
        cep_mode,
        &durable_metadata,
    );

    let durable_metadata_json = durable_metadata.take_json();
    let active_envelope_id = active_envelope.active_envelope().await.map(|(id, _)| id);
    let observed_capabilities: Vec<_> = observed_effects
        .iter()
        .map(|effect| effect.required_capability)
        .collect();
    let merged_metadata = merge_finalize_metadata(
        durable_metadata_json.as_deref(),
        &method,
        &observed_capabilities,
        active_envelope_id.as_deref(),
    );

    finalize_audit_for_dispatch(
        &audit,
        audit_start,
        &method,
        &final_result,
        &call_audit_metadata,
        merged_metadata.as_deref(),
    );
    let ok = final_result.is_ok();
    let error_code = final_result
        .as_ref()
        .err()
        .map(|err| format!("{:?}", err.code));

    emit_cep_call_finished(
        &events,
        &method,
        audit_start.audit_id,
        ok,
        error_code.as_deref(),
        &call_audit_metadata,
    )
    .await;

    call.respond(final_result);
}

fn is_incomplete_dispatch(result: &Result<Value, CepError>) -> bool {
    matches!(
        result,
        Err(err)
            if err.code == CepErrorCode::ExecutionFailed
                && err.message.contains("cancelled")
    )
}

fn apply_effect_check(
    method: &str,
    dispatch_result: Result<Value, CepError>,
    observed_effects: &[crate::effect::ledger::ObservedEffect],
    authorized_region_slot: &crate::effect::ledger::AuthorizedRegionSlot,
    cep_mode: CepMode,
    durable_metadata: &CallDurableMetadata,
) -> Result<Value, CepError> {
    if is_incomplete_dispatch(&dispatch_result) {
        if !observed_effects.is_empty() {
            durable_metadata.record(json!({
                "partial_effects": observed_effects.iter().map(|e| json!({
                    "kind": format!("{:?}", e.kind),
                    "scope": format!("{:?}", e.scope),
                })).collect::<Vec<_>>(),
                "incomplete_dispatch": true,
            }));
        }
        return dispatch_result;
    }

    let Ok(dispatch_value) = dispatch_result else {
        return dispatch_result;
    };

    if cep_mode == CepMode::Off {
        return Ok(dispatch_value);
    }

    let effect_decl = lattice_cep_catalog::catalog()
        .iter()
        .find(|d| d.name == method)
        .map(|d| &d.effect);

    let Some(effect_decl) = effect_decl else {
        return Ok(dispatch_value);
    };

    let region = authorized_region_slot.get();

    match region {
        Some(ref authorized) => match crate::effect::checker::check_effects_against_region(
            method,
            observed_effects,
            authorized,
            effect_decl,
        ) {
            Ok(()) => Ok(dispatch_value),
            Err(violation_err) => match cep_mode {
                CepMode::Enforce => Err(violation_err),
                CepMode::Warn => {
                    durable_metadata.record(json!({
                        "effect_warning": true,
                        "violation": violation_err.message,
                    }));
                    Ok(dispatch_value)
                }
                CepMode::Off => unreachable!("Off early-returned above"),
            },
        },
        None => match cep_mode {
            CepMode::Enforce => Err(CepError::new(
                CepErrorCode::EffectViolation,
                format!("authorized region missing after successful dispatch for {method}"),
                CepErrorData::Effect(Box::new(lattice_cep_protocol::error::EffectErrorData {
                    method: method.to_string(),
                    declared_effect: effect_decl.kinds.0.to_vec(),
                    authorized_effect: vec![],
                    observed_effects: observed_effects.iter().map(|e| e.kind.clone()).collect(),
                    violations: vec![
                        "handler completed without setting authorized region".to_string()
                    ],
                })),
                Retriability::Permanent,
            )),
            CepMode::Warn => {
                tracing::warn!(
                    method = %method,
                    "authorized region missing after successful dispatch; warn mode"
                );
                durable_metadata.record(json!({
                    "effect_warning": true,
                    "violation": "authorized region missing after successful dispatch",
                }));
                Ok(dispatch_value)
            }
            CepMode::Off => unreachable!("Off early-returned above"),
        },
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
    let semantic_status = if call_metadata.semantic_stage == SEMANTIC_STAGE_SCAFFOLD {
        "scaffold_stub"
    } else if call_metadata.policy_enforced {
        "native_handler"
    } else {
        "unrouted"
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
            let semantic_status = if call_metadata.semantic_stage == SEMANTIC_STAGE_UNROUTED {
                "method_not_found"
            } else {
                "native_handler_denied"
            };
            let _ = audit.finalize_call(
                audit_start,
                AuditOutcome::Error { code, summary },
                Some(semantic_status),
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
    events: &lattice_cep_dispatch::EventSink,
    method: &str,
    audit_id: i64,
    metadata: &CallAuditMetadata,
) {
    let impact_kind = impact_kind_for_method(method)
        .as_ref()
        .map(impact_kind_label)
        .unwrap_or("unknown");
    events
        .emit(
            "cep.call_started",
            json!({
                "method": method,
                "impact_kind": impact_kind,
                "audit_id": audit_id,
                "semantic_stage": metadata.semantic_stage,
                "guard_stage": metadata.guard_stage,
                "policy_enforced": metadata.policy_enforced,
            }),
        )
        .await;
}

async fn emit_cep_call_finished(
    events: &lattice_cep_dispatch::EventSink,
    method: &str,
    audit_id: i64,
    ok: bool,
    error_code: Option<&str>,
    metadata: &CallAuditMetadata,
) {
    let mut payload = json!({
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
    events.emit("cep.call_finished", payload).await;
}
