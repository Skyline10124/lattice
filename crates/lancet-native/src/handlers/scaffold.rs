// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Feature-gated `workspace.inspect` / `workspace.navigate` scaffold stubs (OBS-05).

use crate::server::registry::{HandlerContext, MethodRegistry};
use lancet_catalog::CepMethod;
use lancet_catalog::methods::scaffold::{
    Inspect, InspectParams, Navigate, NavigateParams, ScaffoldStubResult,
};
use lancet_protocol::CepError;
use serde_json::json;

/// Register feature-gated inspect/navigate scaffold handlers.
///
/// Not called from [`crate::register_production_method_registry`]; production
/// returns JSON-RPC `-32601` method not found for these methods when disabled.
#[cfg(feature = "inspect-scaffold")]
pub fn register_inspect_scaffold_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    registry.register_method::<Inspect, _, _>(handle_inspect)?;
    registry.register_method::<Navigate, _, _>(handle_navigate)?;
    Ok(())
}

#[cfg(feature = "inspect-scaffold")]
async fn handle_inspect(
    _params: InspectParams,
    ctx: HandlerContext,
) -> Result<ScaffoldStubResult, CepError> {
    let result = ScaffoldStubResult::not_yet_routed(Inspect::NAME);
    emit_scaffold_metadata(&ctx, Inspect::NAME, &result).await;
    Ok(result)
}

#[cfg(feature = "inspect-scaffold")]
async fn handle_navigate(
    _params: NavigateParams,
    ctx: HandlerContext,
) -> Result<ScaffoldStubResult, CepError> {
    let result = ScaffoldStubResult::not_yet_routed(Navigate::NAME);
    emit_scaffold_metadata(&ctx, Navigate::NAME, &result).await;
    Ok(result)
}

#[cfg(feature = "inspect-scaffold")]
async fn emit_scaffold_metadata(ctx: &HandlerContext, method: &str, result: &ScaffoldStubResult) {
    ctx.events
        .emit(
            "cep.scaffold_metadata",
            json!({
                "method": method,
                "metadata": {
                    "status": result.status,
                    "semantic_stage": result.semantic_stage,
                    "routed": result.routed,
                    "observation_payload_bytes": 0,
                    "budget_charged": 0,
                    "policy_enforced": true,
                    "backend_invoked": false,
                }
            }),
        )
        .await;
}

#[cfg(all(test, feature = "inspect-scaffold"))]
mod scaffold_tests {
    use super::*;
    use crate::audit::{CepAuditContext, CepAuditStore, SEMANTIC_STAGE_SCAFFOLD};
    use crate::server::registry::{Cancellation, EventSink};
    use crate::wire_code_for_error;
    use crate::{CepTransport, ServerRunConfig};
    use lancet_authority::policy::CepTurnContext;
    use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
    use lancet_catalog::methods::scaffold::InspectParams;
    use lancet_protocol::ToolEvent;
    use lancet_protocol::policy::CepMode;
    use lancet_protocol::error::jsonrpc;
    use lancet_protocol::policy::PolicyHook;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{RwLock, mpsc, watch};

    fn scaffold_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_inspect_scaffold_methods(&mut registry).expect("register scaffold");
        registry
    }

    fn make_test_ctx(root: &str, event_tx: mpsc::Sender<ToolEvent>) -> HandlerContext {
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let default_hook = Arc::new(DefaultPolicyHook::new(
            PolicyConfig::defaults_for_project_root(std::path::PathBuf::from(root)),
        ));
        let cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
        HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            cep_context,
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
            authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: default_hook.clone() as Arc<dyn PolicyHook>,
            default_policy_hook: Some(default_hook),
            audit_context: Arc::new(RwLock::new(CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
                ..Default::default()
            })),
            audit: Arc::new(CepAuditStore::in_memory().expect("audit store")),
            cep_mode: CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
            execution: None,
            direct_verify: None,
            routing_invocation: crate::execution_backend::RoutingInvocationState::default(),
        }
    }

    #[test]
    fn production_registry_excludes_scaffold_methods() {
        let mut registry = MethodRegistry::new();
        crate::register_production_method_registry(&mut registry).unwrap();
        assert!(!registry.contains("workspace.inspect"));
        assert!(!registry.contains("workspace.navigate"));
    }

    #[tokio::test]
    async fn scaffold_disabled_dispatch_returns_method_not_found() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let mut registry = MethodRegistry::new();
        crate::register_production_method_registry(&mut registry).unwrap();
        let (event_tx, _rx) = mpsc::channel(8);
        let ctx = make_test_ctx(root, event_tx);
        let err = registry
            .dispatch(Inspect::NAME, serde_json::json!({ "path": "src" }), ctx)
            .await
            .unwrap_err();
        assert_eq!(wire_code_for_error(&err), jsonrpc::METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn scaffold_enabled_returns_not_yet_routed_without_budget_charge() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = scaffold_registry();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let ctx = make_test_ctx(root, event_tx);
        let cep_context = Arc::clone(&ctx.cep_context);
        let before = cep_context.budget.snapshot();
        let value = registry
            .dispatch(Inspect::NAME, serde_json::json!({ "path": "src" }), ctx)
            .await
            .unwrap();
        assert_eq!(value["status"], "not_yet_routed");
        assert_eq!(value["semantic_stage"], "scaffold");
        assert_eq!(value["routed"], false);
        assert_eq!(cep_context.budget.snapshot(), before);

        let event = event_rx.recv().await.expect("scaffold metadata");
        assert_eq!(event.method, "cep.scaffold_metadata");
        assert_eq!(event.params["metadata"]["budget_charged"], 0);
        assert_eq!(event.params["metadata"]["observation_payload_bytes"], 0);
        assert_eq!(event.params["metadata"]["routed"], false);
    }

    #[tokio::test]
    async fn scaffold_transport_audit_uses_scaffold_stage_and_zero_budget() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = scaffold_registry();
        let mut config = ServerRunConfig::in_memory_for_tests();
        config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));

        let (client, server) = CepTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        let result = client
            .call::<Inspect>(InspectParams { path: None })
            .await
            .unwrap();
        assert_eq!(result.status, "not_yet_routed");
        assert_eq!(result.semantic_stage, SEMANTIC_STAGE_SCAFFOLD);
        assert!(!result.routed);

        let row = audit.row_by_audit_id(1).unwrap();
        assert_eq!(row.method, Inspect::NAME);
        assert_eq!(row.semantic_stage, SEMANTIC_STAGE_SCAFFOLD);
        assert!(row.policy_enforced);
        let metadata = row.metadata_json.expect("scaffold metadata");
        let metadata: serde_json::Value =
            serde_json::from_str(&metadata).expect("metadata should be JSON");
        assert_eq!(metadata["routed"], false);
        assert_eq!(metadata["budget_charged"], 0);
        assert_eq!(metadata["observation_payload_bytes"], 0);

        drop(client);
        server_task.abort();
    }
}
