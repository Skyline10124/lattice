// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET runtime composition: spawn a native CEP server and hand back a client.

use std::sync::Arc;

use lancet_audit::{CepAuditContext, CepAuditStore};
use lancet_authority::policy::default::DefaultPolicyHook;
use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_cep::{ToolDefinition, ToolExposure, ToolExposureError};
use lancet_dispatch::{CepClient, CepTransport};
use lancet_native::{MethodRegistry, ServerRunConfig, register_production_method_registry};
use tokio::sync::RwLock;

use crate::config::{AuditBacking, LancetRuntimeConfig};
use crate::error::LancetError;

/// Default CEP transport channel capacity for the production runtime.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 256;

/// Owns a spawned native CEP server task and shuts it down on drop.
///
/// Hold this for as long as the paired [`CepClient`] is in use; dropping it (or
/// calling [`ServerLifecycle::shutdown`]) signals the server to stop.
pub struct ServerLifecycle {
    server_task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl ServerLifecycle {
    /// Signal shutdown and await the server task to drain pending work.
    pub async fn shutdown(&mut self) {
        let _ = self.shutdown.send(true);
        let task = std::mem::replace(&mut self.server_task, tokio::spawn(async {}));
        let _ = task.await;
    }

    /// A lifecycle that manages no server task.
    ///
    /// For tests and for callers that pair a [`CepClient`] with an externally
    /// managed (or absent) server.
    pub fn noop() -> Self {
        Self {
            server_task: tokio::spawn(async {}),
            shutdown: tokio::sync::watch::channel(false).0,
        }
    }
}

impl Drop for ServerLifecycle {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
    }
}

/// Low-level launcher for a native CEP server over an in-process transport.
///
/// Registers the production method registry, pairs a client/server transport,
/// and spawns the server task. Callers that need bespoke [`ServerRunConfig`]
/// values (custom base envelope, pre-opened audit store) use this directly;
/// [`LancetRuntime::build`] is the higher-level path for full turn runtimes.
pub struct LancetServer;

impl LancetServer {
    /// Spawn a native CEP server bound to `config`, returning the client and lifecycle.
    pub fn spawn(
        config: ServerRunConfig,
        channel_capacity: usize,
    ) -> Result<(CepClient, ServerLifecycle), LancetError> {
        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).map_err(|err| {
            LancetError::Internal(format!("production method registry registration: {err:?}"))
        })?;

        let (client, server) = CepTransport::pair(channel_capacity);
        let shutdown = server.shutdown_handle();
        let server_task = tokio::spawn(async move {
            let _ = server.run(registry, config).await;
        });

        Ok((
            client,
            ServerLifecycle {
                server_task,
                shutdown,
            },
        ))
    }
}

/// Decomposed runtime pieces for callers that wire their own executors on top.
pub struct LancetRuntimeParts {
    /// CEP client for typed method calls.
    pub client: CepClient,
    /// Catalog-derived tool definitions filtered to the enabled set.
    pub tool_definitions: Vec<ToolDefinition>,
    /// Audit store backing this runtime.
    pub audit: Arc<CepAuditStore>,
    /// Mutable audit context (model id, runtime version, session/turn ids).
    pub audit_context: Arc<RwLock<CepAuditContext>>,
    /// Server lifecycle; keep alive while the client is used.
    pub lifecycle: ServerLifecycle,
}

/// A fully-composed LANCET runtime: a live native CEP server plus its client.
///
/// This is the unified entry point for driving LANCET independently of any host
/// application. Build it from a [`LancetRuntimeConfig`], call typed methods via
/// [`LancetRuntime::client`], and persist turn attestations via
/// [`crate::store_turn_attestation`].
pub struct LancetRuntime {
    client: CepClient,
    audit: Arc<CepAuditStore>,
    audit_context: Arc<RwLock<CepAuditContext>>,
    tool_definitions: Vec<ToolDefinition>,
    lifecycle: ServerLifecycle,
}

impl LancetRuntime {
    /// Compose and spawn a production LANCET runtime from `config`.
    pub fn build(config: LancetRuntimeConfig) -> Result<Self, LancetError> {
        let LancetRuntimeConfig {
            project_root,
            enabled_tools,
            policy,
            cep_mode,
            audit,
            model_id,
            runtime_version,
            execution,
        } = config;

        let tool_exposure = resolve_catalog_tool_exposure(&enabled_tools)?;
        let tool_definitions = tool_exposure.definitions().to_vec();

        let cep_policy = CepPolicyConfig {
            project_root: policy.project_root.clone(),
            observation: policy.observation.clone(),
            network: policy.network.clone(),
            budget: policy.budget.clone(),
            denied_effect_kinds: policy.denied_effect_kinds.clone(),
        };

        let audit = match audit {
            AuditBacking::InMemory => Arc::new(
                CepAuditStore::in_memory()
                    .map_err(|err| LancetError::Internal(format!("audit store setup: {err}")))?,
            ),
            AuditBacking::File(path) => {
                let store = CepAuditStore::open(&path)
                    .map_err(|err| LancetError::Internal(format!("audit store setup: {err}")))?;
                let legacy_path = project_root.join(".lattice").join("cep_audit.db");
                if legacy_path.exists() {
                    store
                        .import_legacy_read_only_if_empty(&legacy_path)
                        .map_err(|err| {
                            LancetError::Internal(format!("legacy audit import: {err}"))
                        })?;
                }
                Arc::new(store)
            }
        };

        let audit_context = Arc::new(RwLock::new(CepAuditContext {
            model_id,
            runtime_version,
            ..Default::default()
        }));
        let cep_context = Arc::new(CepTurnContext::from_policy(cep_policy));
        let default_hook = Arc::new(DefaultPolicyHook::new(policy));

        let server_config = ServerRunConfig {
            audit: Arc::clone(&audit),
            audit_context: Arc::clone(&audit_context),
            cep_context,
            policy_hook: default_hook.clone(),
            default_policy_hook: Some(default_hook),
            cep_mode,
            execution,
        };

        let (client, lifecycle) = LancetServer::spawn(server_config, DEFAULT_CHANNEL_CAPACITY)?;

        Ok(Self {
            client,
            audit,
            audit_context,
            tool_definitions,
            lifecycle,
        })
    }

    /// Clone of the CEP client for typed method dispatch.
    pub fn client(&self) -> CepClient {
        self.client.clone()
    }

    /// Audit store backing this runtime.
    pub fn audit(&self) -> &Arc<CepAuditStore> {
        &self.audit
    }

    /// Mutable audit context shared with the server task.
    pub fn audit_context(&self) -> &Arc<RwLock<CepAuditContext>> {
        &self.audit_context
    }

    /// Catalog-derived tool definitions filtered to the enabled set.
    pub fn tool_definitions(&self) -> &[ToolDefinition] {
        &self.tool_definitions
    }

    /// Signal shutdown and await the server task.
    pub async fn shutdown(&mut self) {
        self.lifecycle.shutdown().await;
    }

    /// Decompose into owned parts for callers that build their own executors.
    pub fn into_parts(self) -> LancetRuntimeParts {
        LancetRuntimeParts {
            client: self.client,
            tool_definitions: self.tool_definitions,
            audit: self.audit,
            audit_context: self.audit_context,
            lifecycle: self.lifecycle,
        }
    }
}

fn resolve_catalog_tool_exposure(enabled: &[String]) -> Result<ToolExposure, LancetError> {
    ToolExposure::from_catalog_enabled(enabled).map_err(map_tool_exposure_err)
}

fn map_tool_exposure_err(err: ToolExposureError) -> LancetError {
    LancetError::UnknownTools {
        unknown: err.unknown,
        available: err.available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_config_defaults_have_null_execution() {
        let config = LancetRuntimeConfig::new(".");
        assert_eq!(config.execution.upper.execution_backend, "null");
    }

    // Issue #4 behavior acceptance: a raw CEP call to the permanently retired
    // `exec.command_run` method must reach the production dispatch layer (not be
    // rejected by a pre-dispatch policy/tool-exposure filter) and return a stable
    // JSON-RPC `Method not found` (-32601) error — never a policy-denied, verify
    // failed, or execution result. This exercises the full production path:
    // `LancetRuntime::build` -> `LancetServer::spawn` -> production registry ->
    // transport server loop -> `handle_request` -> `registry.dispatch` -> client.
    #[tokio::test]
    async fn production_runtime_rejects_retired_exec_command_run_as_method_not_found() {
        use lancet_catalog::CATALOG;
        use lancet_protocol::Retriability;
        use lancet_protocol::error::jsonrpc;

        // Sanity: the method is genuinely absent from the production catalog.
        assert!(
            !CATALOG.iter().any(|d| d.name == "exec.command_run"),
            "exec.command_run must not appear in CATALOG"
        );

        // Build a production runtime exposing every catalog method, so the call
        // is not filtered out by enabled-tools exposure before reaching dispatch.
        let enabled_tools: Vec<String> = CATALOG.iter().map(|d| d.name.to_string()).collect();
        let config = LancetRuntimeConfig {
            enabled_tools,
            ..LancetRuntimeConfig::new(".")
        };
        let mut runtime = LancetRuntime::build(config).expect("production runtime builds");

        // Send a raw CEP JSON-RPC call for the retired method. `call_raw` bypasses
        // the typed CatalogMethod trait and lets an arbitrary method string reach
        // the production registry dispatch path.
        let client = runtime.client();
        let err = client
            .call_raw("exec.command_run", serde_json::json!({}))
            .await
            .expect_err("retired method must error, not execute");

        // JSON-RPC wire code must be -32601 (Method not found), not a policy /
        // verify / execution error.
        assert_eq!(
            err.wire_code(),
            jsonrpc::METHOD_NOT_FOUND,
            "retired exec.command_run must map to JSON-RPC -32601, got {:?}",
            err
        );
        assert!(
            err.message.starts_with("Method not found:"),
            "expected Method not found message, got: {}",
            err.message
        );
        assert_eq!(
            err.retriability,
            Retriability::Permanent,
            "method_not_found must be permanent"
        );

        runtime.shutdown().await;
    }
}
