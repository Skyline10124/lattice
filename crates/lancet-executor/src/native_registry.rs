//! Phase 21/22 native catalog method registration.

use crate::handlers::{
    register_command_methods, register_envelope_methods, register_external_methods,
    register_mutation_methods, register_session_state_methods,
    register_workspace_observation_methods,
};
use crate::registry::MethodRegistry;
use lancet_protocol::CepError;

/// Register all six Phase 21 native handlers (observation, external, session-state).
pub fn register_phase21_native_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_phase21_workspace_observation(registry)?;
    register_phase21_external(registry)?;
    register_phase21_session_state(registry)?;
    Ok(())
}

/// Register native Phase 21 workspace observation handlers.
pub fn register_phase21_workspace_observation(
    registry: &mut MethodRegistry,
) -> Result<(), CepError> {
    register_workspace_observation_methods(registry)
}

/// Register native Phase 21 `web.fetch` handler.
pub fn register_phase21_external(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_external_methods(registry)
}

/// Register native Phase 21 `session.todo_write` handler.
pub fn register_phase21_session_state(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_session_state_methods(registry)
}

/// Register native Phase 22 `envelope.declare` handler.
pub fn register_phase22_native_envelope(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_envelope_methods(registry)
}

/// Register native Phase 22 `command.run` handler.
pub fn register_phase22_native_command(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_command_methods(registry)
}

/// Register native Phase 22 `workspace.write_file` and `workspace.patch` handlers.
pub fn register_phase22_native_mutation(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_mutation_methods(registry)
}

/// Production registry: all catalog methods served by Phase 21/22 native handlers.
///
/// Phase 21 native: `workspace.*` observation, `web.fetch`, `session.todo_write`.
/// Phase 22 native: `envelope.declare`, `command.run`, `workspace.write_file`, `workspace.patch`.
/// The legacy CEP bridge is no longer part of the production registry.
pub fn register_production_method_registry(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_phase21_native_methods(registry)?;
    register_phase22_native_envelope(registry)?;
    register_phase22_native_command(registry)?;
    register_phase22_native_mutation(registry)
}

#[cfg(test)]
mod phase21_native_registry_tests {
    use super::*;
    use crate::audit::{GUARD_STAGE_NATIVE, SEMANTIC_STAGE_NATIVE};
    use crate::policy::observation::BACKEND_SAFETY_NET_TEST_PATH;
    use crate::policy::Phase21TurnContext;
    use crate::registry::{Cancellation, EventSink, HandlerContext};
    use crate::transport::{LancetTransport, ServerRunConfig};
    use lancet_protocol::catalog;
    use lancet_protocol::envelope::BaseExecutionEnvelope;
    use lancet_protocol::methods::command::Run;
    use lancet_protocol::methods::workspace::{ReadFile, ReadFileParams};
    use lancet_protocol::{CepErrorCode, CepMethod};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn production_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();
        registry
    }

    #[test]
    fn phase21_native_registry_registers_full_catalog_once() {
        let registry = production_registry();
        for descriptor in catalog() {
            assert!(
                registry.contains(descriptor.name),
                "missing catalog method {}",
                descriptor.name
            );
        }
    }

    #[test]
    fn phase21_native_registry_double_registration_fails() {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();
        let err =
            register_phase21_native_methods(&mut registry).expect_err("double native registration");
        assert_eq!(err.code, CepErrorCode::DuplicateRegistration);
    }

    #[tokio::test]
    async fn phase21_native_registry_native_error_does_not_emit_bridge_metadata() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("probe.txt"), "x").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let ctx = HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "native error test"),
            phase21: Arc::new(Phase21TurnContext::defaults_for_project_root(root)),
            active_envelope: Arc::new(crate::envelope_store::ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: crate::effect::ledger::EffectLedger::new(),
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
                    std::path::PathBuf::from(root),
                ),
            )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
            default_policy_hook: None,
            audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
            })),
        };

        let params = ReadFileParams {
            path: "../outside".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = registry
            .dispatch(ReadFile::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);

        while let Ok(event) = event_rx.try_recv() {
            assert_ne!(
                event.method, "cep.bridge_metadata",
                "native failure must not fall back to legacy bridge metadata"
            );
        }
    }

    #[tokio::test]
    async fn phase21_native_registry_production_audit_routing_labels_native_methods() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope = BaseExecutionEnvelope::for_workspace_root(root, "audit routing");
        config.phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root(root));
        config.production_audit_routing = true;
        let default_hook = Arc::new(crate::policy::default::DefaultPolicyHook::new(
            crate::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        ));
        config.policy_hook = default_hook.clone();
        config.default_policy_hook = Some(default_hook);

        let (client, server) = LancetTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        client
            .call::<ReadFile>(ReadFileParams {
                path: "hello.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            })
            .await
            .unwrap();

        assert_eq!(audit.row_count().unwrap(), 1);
        let read_row = audit.row_by_audit_id(1).unwrap();
        assert_eq!(read_row.method, ReadFile::NAME);
        assert_eq!(read_row.semantic_stage, SEMANTIC_STAGE_NATIVE);
        assert_eq!(read_row.guard_stage, GUARD_STAGE_NATIVE);
        assert!(read_row.policy_enforced);

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn phase21_native_registry_command_run_audited_as_native() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope = BaseExecutionEnvelope::for_workspace_root(root, "command audit");
        config.phase21 = Arc::new(Phase21TurnContext::defaults_for_project_root(root));
        config.production_audit_routing = true;

        let (client, server) = LancetTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        let _ = client
            .call::<Run>(lancet_protocol::methods::command::RunParams {
                argv: vec!["echo".into(), "native".into()],
                cwd: None,
                env: Default::default(),
                timeout_ms: None,
                declared_impact: None,
            })
            .await;

        assert_eq!(audit.row_count().unwrap(), 1);
        let run_row = audit.row_by_audit_id(1).unwrap();
        assert_eq!(run_row.method, Run::NAME);
        assert_eq!(run_row.semantic_stage, SEMANTIC_STAGE_NATIVE);
        assert_eq!(run_row.guard_stage, GUARD_STAGE_NATIVE);
        assert!(run_row.policy_enforced);

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn phase21_native_registry_backend_safety_net_stays_native() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join(BACKEND_SAFETY_NET_TEST_PATH), "probe").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let ctx = HandlerContext {
            call_id: 2,
            events: EventSink::new(event_tx, 2),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "safety net"),
            phase21: Arc::new(Phase21TurnContext::defaults_for_project_root(root)),
            active_envelope: Arc::new(crate::envelope_store::ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: crate::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: crate::effect::ledger::EffectLedger::new(),
            authorized_region: crate::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(crate::policy::default::DefaultPolicyHook::new(
                crate::policy::default::PolicyConfig::defaults_for_project_root(
                    std::path::PathBuf::from(root),
                ),
            )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
            default_policy_hook: None,
            audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
            })),
        };

        let params = ReadFileParams {
            path: BACKEND_SAFETY_NET_TEST_PATH.into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = registry
            .dispatch(ReadFile::NAME, serde_json::to_value(&params).unwrap(), ctx)
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ProjectRootViolation);
    }
}
