// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Native CEP server lifecycle layered on dispatch transport.

use crate::server::call_durable_metadata::CallDurableMetadata;
use crate::server::registry::{HandlerContext, MethodRegistry};
use crate::workspace::LocalWorkspace;
use lancet_audit::{
    AuditOutcome, AuditStart, CallAuditMetadata, CepAuditContext, CepAuditStore,
    SEMANTIC_STAGE_SCAFFOLD, SEMANTIC_STAGE_UNROUTED, impact_kind_for_method, impact_kind_label,
    merge_finalize_metadata,
};
use lancet_authority::policy::CepTurnContext;
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_dispatch::{CepServer, ServerCall, ServerRuntime};
use lancet_protocol::policy::PolicyHook;
use lancet_protocol::{CepError, CepErrorCode, CepErrorData, Retriability, policy::CepMode};
use lancet_runtime::{CellEvidenceLedger, ComponentCallContext, ComponentRuntime};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Server-side audit and lifecycle configuration for a turn-scoped native runtime.
///
/// The Local Workspace is not configuration: the server constructs the one
/// concrete [`LocalWorkspace`] from `cep_context.policy.project_root` when the
/// run starts. Callers supply only the canonical project root (via policy)
/// and resolved policy.
///
/// `components` carries the closed Component runtime (ADR 0004). When a
/// method name is in the Component catalog, `handle_request` dispatches to
/// it instead of the native typed registry. Production builds compose
/// `ComponentRuntime::from_embedded()` (empty until #29–#32, Codeberg
/// #7–#10); tests may inject a no-op fixture via `with_components`.
#[derive(Clone)]
pub struct ServerRunConfig {
    pub audit: Arc<CepAuditStore>,
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    pub cep_context: Arc<CepTurnContext>,
    /// Pluggable policy hook for capability/command/network/budget decisions.
    pub policy_hook: Arc<dyn PolicyHook>,
    /// Executor-owned default hook for evidence capture when the default implementation is installed.
    pub default_policy_hook: Option<Arc<DefaultPolicyHook>>,
    /// CEP enforcement mode.
    pub cep_mode: CepMode,
    /// Closed Component runtime for ADR 0004 Tool dispatch. `None` in
    /// legacy callers; the native server treats `None` as an empty
    /// Component catalog (all methods go to the native typed registry).
    pub components: Option<Arc<ComponentRuntime>>,
}

impl ServerRunConfig {
    /// In-memory audit store with stable test identifiers and safe CEP policy/budget defaults.
    pub fn in_memory_for_tests() -> Self {
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
            cep_context,
            policy_hook: default_hook.clone(),
            default_policy_hook: Some(default_hook),
            cep_mode: CepMode::Off,
            components: None,
        }
    }
}

/// Per-run native server runtime: the run configuration plus the one
/// concrete [`LocalWorkspace`] constructed once from the resolved policy
/// project root when the server starts.
#[derive(Clone)]
struct NativeServerRuntime {
    config: ServerRunConfig,
    workspace: Arc<LocalWorkspace>,
}

impl ServerRuntime<HandlerContext> for NativeServerRuntime {
    fn handle_call(
        &self,
        registry: MethodRegistry,
        call: ServerCall,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let config = self.config.clone();
        let workspace = Arc::clone(&self.workspace);
        Box::pin(async move {
            handle_request(registry, call, config, workspace).await;
        })
    }

    fn after_shutdown(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let audit = Arc::clone(&self.config.audit);
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
        Box::pin(async move {
            // ADR 0003: the one concrete Local Workspace is constructed once
            // per server run from the resolved policy's canonical project
            // root. No caller-supplied workspace implementation is accepted.
            let workspace = Arc::new(LocalWorkspace::new(&config.cep_context.policy.project_root));
            self.run(registry, NativeServerRuntime { config, workspace })
                .await
        })
    }
}

async fn handle_request(
    registry: MethodRegistry,
    call: ServerCall,
    config: ServerRunConfig,
    workspace: Arc<LocalWorkspace>,
) {
    let ServerRunConfig {
        audit,
        audit_context,
        cep_context,
        policy_hook,
        default_policy_hook,
        cep_mode,
        components,
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
    let effect_ledger = lancet_authority::effect::ledger::EffectLedger::new();
    let authorized_region_slot = lancet_authority::effect::ledger::AuthorizedRegionSlot::new();
    let turn_id = ctx_snapshot.turn_id.clone();

    // D.3: Component/native dispatch split. If the method name is in the
    // closed Component catalog, dispatch to the Component runtime; otherwise
    // fall through to the existing 5-method typed registry. The split happens
    // after audit start and before finalize, so Component and native calls
    // share one durable audit path. Component calls produce Cell evidence
    // (no domain operation in #27); native calls produce effect-ledger
    // evidence.
    let component_evidence = CellEvidenceLedger::new();

    // Pre-dispatch effect-kind deny (issue #24 slice 3): compare the method's
    // static declared effect kinds with `denied_effect_kinds` before invoking
    // any handler/backend. Post-dispatch observed-effect checking remains
    // defense in depth below.
    if let Some(descriptor) = lancet_catalog::catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        && let Err(denial) = lancet_authority::effect::checker::check_declared_effect_kinds_denied(
            &method,
            &descriptor.effect,
            &cep_context.policy.denied_effect_kinds,
        )
    {
        let final_result: Result<Value, CepError> = Err(denial);
        finalize_audit_for_dispatch(
            &audit,
            audit_start,
            &method,
            &final_result,
            &call_audit_metadata,
            None,
        );
        let error_code = final_result
            .as_ref()
            .err()
            .map(|err| format!("{:?}", err.code));
        emit_cep_call_finished(
            &events,
            &method,
            audit_start.audit_id,
            false,
            error_code.as_deref(),
            &call_audit_metadata,
        )
        .await;
        call.respond(final_result);
        return;
    }

    let dispatch_result = if let Some(ref rt) = components
        && rt.contains(&method)
    {
        let comp_ctx = ComponentCallContext {
            call_id: id,
            turn_id: turn_id.clone(),
            events: events.clone(),
            cancellation: cancellation.clone(),
            evidence: component_evidence.clone(),
        };
        rt.call(&method, params, comp_ctx).await
    } else {
        let ctx = HandlerContext {
            call_id: id,
            events: events.clone(),
            cancellation,
            cep_context: Arc::clone(&cep_context),
            turn_id,
            durable_metadata: durable_metadata.clone(),
            effect_ledger: effect_ledger.clone(),
            authorized_region: authorized_region_slot.clone(),
            policy_hook: Arc::clone(&policy_hook),
            default_policy_hook: default_policy_hook.clone(),
            audit_context: Arc::clone(&audit_context),
            audit: Arc::clone(&audit),
            cep_mode: cep_mode.clone(),
            workspace,
        };
        registry.dispatch(&method, params, ctx).await
    };

    let observed_effects = effect_ledger.snapshot();

    let final_result = apply_effect_check(
        &method,
        dispatch_result,
        &observed_effects,
        &authorized_region_slot,
        cep_mode,
        &durable_metadata,
        &cep_context.policy.denied_effect_kinds,
    );

    let durable_metadata_json = durable_metadata.take_json();
    let observed_capabilities: Vec<_> = observed_effects
        .iter()
        .map(|effect| effect.required_capability)
        .collect();
    let component_cells = component_evidence.snapshot_keys();
    let merged_metadata = merge_finalize_metadata(
        durable_metadata_json.as_deref(),
        &method,
        &observed_capabilities,
        &component_cells,
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

/// #70 (Codeberg #48): a dispatch that never completed because the call was
/// cancelled.
/// Cancellation state is the classification authority end to end, so every
/// call-scoped cancellation surfaces as the structured `CallCancelled` code
/// — never matched on a guest/client-controlled error string.
fn is_incomplete_dispatch(result: &Result<Value, CepError>) -> bool {
    matches!(result, Err(err) if err.code == CepErrorCode::CallCancelled)
}

fn apply_effect_check(
    method: &str,
    dispatch_result: Result<Value, CepError>,
    observed_effects: &[lancet_authority::effect::ledger::ObservedEffect],
    authorized_region_slot: &lancet_authority::effect::ledger::AuthorizedRegionSlot,
    cep_mode: CepMode,
    durable_metadata: &CallDurableMetadata,
    denied_effect_kinds: &std::collections::BTreeSet<lancet_protocol::effect_types::ImpactKind>,
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

    let effect_decl = lancet_catalog::catalog()
        .iter()
        .find(|descriptor| descriptor.name == method)
        .map(|descriptor| &descriptor.effect);

    let Some(effect_decl) = effect_decl else {
        return Ok(dispatch_value);
    };

    lancet_authority::effect::checker::check_denied_effect_kinds(
        method,
        observed_effects,
        effect_decl,
        denied_effect_kinds,
    )?;

    if cep_mode == CepMode::Off {
        return Ok(dispatch_value);
    }

    let region = authorized_region_slot.get();

    match region {
        Some(ref authorized) => {
            match lancet_authority::effect::checker::check_effects_against_region(
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
            }
        }
        None => match cep_mode {
            CepMode::Enforce => Err(CepError::new(
                CepErrorCode::EffectViolation,
                format!("authorized region missing after successful dispatch for {method}"),
                CepErrorData::Effect(Box::new(lancet_protocol::error::EffectErrorData {
                    method: method.to_string(),
                    declared_effect: effect_decl.kinds.0.to_vec(),
                    authorized_effect: vec![],
                    observed_effects: observed_effects.iter().map(|e| e.kind.clone()).collect(),
                    violations: vec![
                        "handler completed without setting authorized region".to_string(),
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
            let metadata = if call_metadata.semantic_stage == SEMANTIC_STAGE_SCAFFOLD {
                merge_scaffold_success_metadata(
                    durable_metadata_json,
                    compact_success_metadata(method, value).as_deref(),
                )
            } else {
                durable_metadata_json
                    .map(str::to_string)
                    .or_else(|| compact_success_metadata(method, value))
            };
            let _ = audit.finalize_call(
                audit_start,
                AuditOutcome::Success { summary },
                Some(semantic_status),
                metadata.as_deref(),
            );
        }
        Err(err) if err.code == CepErrorCode::CallCancelled => {
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

fn merge_scaffold_success_metadata(
    durable_metadata_json: Option<&str>,
    compact_metadata_json: Option<&str>,
) -> Option<String> {
    let mut merged = compact_metadata_json
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| json!({}));

    let Some(merged_obj) = merged.as_object_mut() else {
        return durable_metadata_json
            .or(compact_metadata_json)
            .map(str::to_string);
    };

    if let Some(durable) = durable_metadata_json
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| v.as_object().cloned())
    {
        for (key, value) in durable {
            merged_obj.insert(key, value);
        }
    }

    serde_json::to_string(&merged).ok()
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
        _ => format!("method={method}"),
    }
}

fn compact_success_metadata(method: &str, value: &Value) -> Option<String> {
    match method {
        "session.todo_write" => {
            let count = value
                .get("todos")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            Some(format!(r#"{{"outcome":"todo_write","op_count":{count}}}"#))
        }
        "fetch.web" => {
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
                r#"{{"outcome":"fetch_web","response_size":{response_size},"truncated":{truncated}}}"#
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
                r#"{{"outcome":"scaffold","status":"{status}","routed":{routed},"observation_payload_bytes":0,"budget_charged":0}}"#
            ))
        }
        _ => Some(format!(r#"{{"method":"{method}"}}"#)),
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
    events: &lancet_dispatch::EventSink,
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
    events: &lancet_dispatch::EventSink,
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
    if let Some(code) = error_code
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("error_code".to_string(), Value::String(code.to_string()));
    }
    events.emit("cep.call_finished", payload).await;
}
