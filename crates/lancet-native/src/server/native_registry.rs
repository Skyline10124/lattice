// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP/22 native catalog method registration.

use crate::handlers::{
    register_audit_methods, register_command_methods, register_envelope_methods,
    register_external_methods, register_mutation_methods, register_session_state_methods,
    register_workspace_observation_methods,
};
use crate::server::registry::MethodRegistry;
use lancet_protocol::CepError;

/// Register all six LANCET native handlers (observation, external, session-state).
pub fn register_cep_native_methods(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_native_workspace_observation(registry)?;
    register_native_external(registry)?;
    register_native_session_state(registry)?;
    Ok(())
}

/// Register native CEP workspace observation handlers.
pub fn register_native_workspace_observation(
    registry: &mut MethodRegistry,
) -> Result<(), CepError> {
    register_workspace_observation_methods(registry)
}

/// Register native CEP `fetch.web` handler.
pub fn register_native_external(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_external_methods(registry)
}

/// Register native CEP `session.todo_write` handler.
pub fn register_native_session_state(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_session_state_methods(registry)
}

/// Register native CEP `session.envelope` handler.
pub fn register_native_envelope(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_envelope_methods(registry)
}

/// Register native CEP `exec.command_run` handler.
pub fn register_native_command(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_command_methods(registry)
}

/// Register native CEP workspace mutation handlers.
pub fn register_native_mutation(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_mutation_methods(registry)
}

pub fn register_production_method_registry(registry: &mut MethodRegistry) -> Result<(), CepError> {
    register_cep_native_methods(registry)?;
    register_native_envelope(registry)?;
    register_native_command(registry)?;
    register_native_mutation(registry)?;
    register_audit_methods(registry)
}

#[cfg(test)]
mod cep_native_registry_tests {
    use super::*;
    use crate::audit::{GUARD_STAGE_NATIVE, SEMANTIC_STAGE_NATIVE};
    use crate::handlers::observation::BACKEND_SAFETY_NET_TEST_PATH;
    use crate::server::registry::{Cancellation, EventSink, HandlerContext};
    use crate::{CepTransport, ServerRunConfig};
    use lancet_authority::policy::CepTurnContext;
    use lancet_catalog::catalog;
    use lancet_catalog::methods::command::Run;
    use lancet_catalog::methods::workspace::{
        ObserveWorkspace, ObserveWorkspaceParams, ReadFileParams,
    };
    use lancet_catalog::CepMethod;
    use lancet_protocol::envelope::BaseExecutionEnvelope;
    use lancet_protocol::CepErrorCode;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, watch};

    fn production_registry() -> MethodRegistry {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();
        registry
    }

    #[test]
    fn cep_native_registry_registers_full_catalog_once() {
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
    fn cep_native_registry_double_registration_fails() {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).unwrap();
        let err =
            register_cep_native_methods(&mut registry).expect_err("double native registration");
        assert_eq!(err.code, CepErrorCode::DuplicateRegistration);
    }

    #[tokio::test]
    async fn cep_native_registry_native_error_returns_guard_denied() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("probe.txt"), "x").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_cancel_tx, cancel_rx) = watch::channel(false);
        let ctx = HandlerContext {
            call_id: 1,
            events: EventSink::new(event_tx, 1),
            cancellation: Cancellation::new(cancel_rx),
            base_envelope: BaseExecutionEnvelope::for_workspace_root(root, "native error test"),
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            active_envelope: Arc::new(lancet_authority::envelope_store::ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
            authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
                lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                    std::path::PathBuf::from(root),
                ),
            )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
            default_policy_hook: None,
            audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
                ..Default::default()
            })),
            audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
        };

        let params = ReadFileParams {
            path: "../outside".into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = registry
            .dispatch(
                ObserveWorkspace::NAME,
                serde_json::to_value(ObserveWorkspaceParams::ReadFile(params)).unwrap(),
                ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::GuardDenied);
    }

    #[tokio::test]
    async fn cep_native_registry_labels_native_methods() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hi").unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope = BaseExecutionEnvelope::for_workspace_root(root, "audit routing");
        config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));
        let default_hook = Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
            lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                std::path::PathBuf::from(root),
            ),
        ));
        config.policy_hook = default_hook.clone();
        config.default_policy_hook = Some(default_hook);

        let (client, server) = CepTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        client
            .call::<ObserveWorkspace>(ObserveWorkspaceParams::ReadFile(ReadFileParams {
                path: "hello.txt".into(),
                offset: None,
                limit: None,
                line_numbers: None,
            }))
            .await
            .unwrap();

        assert_eq!(audit.row_count().unwrap(), 1);
        let read_row = audit.row_by_audit_id(1).unwrap();
        assert_eq!(read_row.method, ObserveWorkspace::NAME);
        assert_eq!(read_row.semantic_stage, SEMANTIC_STAGE_NATIVE);
        assert_eq!(read_row.guard_stage, GUARD_STAGE_NATIVE);
        assert!(read_row.policy_enforced);

        drop(client);
        server_task.abort();
    }

    #[tokio::test]
    async fn cep_native_registry_command_run_audited_as_native() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_str().unwrap();
        let registry = production_registry();
        let mut config = ServerRunConfig::in_memory_for_tests();
        config.base_envelope = BaseExecutionEnvelope::for_workspace_root(root, "command audit");
        config.cep_context = Arc::new(CepTurnContext::defaults_for_project_root(root));

        let (client, server) = CepTransport::pair(8);
        let audit = Arc::clone(&config.audit);
        let server_task = tokio::spawn(async move {
            server.run(registry, config).await.unwrap();
        });

        let _ = client
            .call::<Run>(lancet_catalog::methods::command::RunParams {
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
    async fn cep_native_registry_backend_safety_net_stays_native() {
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
            cep_context: Arc::new(CepTurnContext::defaults_for_project_root(root)),
            active_envelope: Arc::new(lancet_authority::envelope_store::ActiveEnvelopeStore::new()),
            turn_id: "test-turn".into(),
            durable_metadata: crate::server::call_durable_metadata::CallDurableMetadata::default(),
            effect_ledger: lancet_authority::effect::ledger::EffectLedger::new(),
            authorized_region: lancet_authority::effect::ledger::AuthorizedRegionSlot::new(),
            policy_hook: Arc::new(lancet_authority::policy::default::DefaultPolicyHook::new(
                lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                    std::path::PathBuf::from(root),
                ),
            )) as Arc<dyn lancet_protocol::policy::PolicyHook>,
            default_policy_hook: None,
            audit_context: Arc::new(tokio::sync::RwLock::new(crate::audit::CepAuditContext {
                turn_id: "test-turn".into(),
                session_id: "test-session".into(),
                ..Default::default()
            })),
            audit: Arc::new(crate::audit::CepAuditStore::in_memory().expect("audit store")),
            cep_mode: lancet_protocol::envelope::CepMode::Off,
            command_isolation:
                crate::enforcement::isolation::CommandIsolationRuntimeConfig::default(),
        };

        let params = ReadFileParams {
            path: BACKEND_SAFETY_NET_TEST_PATH.into(),
            offset: None,
            limit: None,
            line_numbers: None,
        };
        let err = registry
            .dispatch(
                ObserveWorkspace::NAME,
                serde_json::to_value(ObserveWorkspaceParams::ReadFile(params)).unwrap(),
                ctx,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ProjectRootViolation);
    }
}
