// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET runtime composition: spawn a native CEP server and hand back a client.

use std::path::Path;
use std::sync::Arc;

use lancet_audit::{CepAuditContext, CepAuditStore};
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_authority::{ActiveEnvelopeStore, CepPolicyConfig, CepTurnContext};
use lancet_catalog::methods::envelope::Envelope;
use lancet_catalog::methods::external::Fetch;
use lancet_catalog::methods::session::TodoWrite;
use lancet_catalog::CepMethod;
use lancet_dispatch::{CepClient, CepTransport};
use lancet_invariants::{load_invariant_index_from_dir, resolve_invariants_dir};
use lancet_native::{register_production_method_registry, MethodRegistry, ServerRunConfig};
use lancet_protocol::envelope::BaseExecutionEnvelope;
use lattice_llm::ToolDefinition;
use tokio::sync::RwLock;

use crate::config::{AuditBacking, LancetRuntimeConfig};
use crate::error::LancetError;
use crate::tools::resolve_catalog_tool_definitions;

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
            command_isolation,
            cep_mode,
            audit,
            invariants_dir,
            model_id,
            runtime_version,
        } = config;

        let project_root_str = project_root
            .to_str()
            .ok_or_else(|| LancetError::Config("project root path is not valid UTF-8".into()))?
            .to_string();
        // Best-effort canonicalization so the base envelope's capability patterns
        // match the canonical targets the guard compares against (e.g. symlinked
        // temp roots). Falls back to the raw path on failure.
        let workspace_root = std::fs::canonicalize(&project_root)
            .ok()
            .and_then(|p| p.to_str().map(str::to_owned))
            .unwrap_or(project_root_str);

        let tool_definitions = resolve_catalog_tool_definitions(&enabled_tools)?;

        let cep_policy = cep_policy_from_policy_config(&policy);
        let base_envelope =
            base_observation_envelope(&workspace_root, &enabled_tools, &cep_policy)?;

        let audit = match audit {
            AuditBacking::InMemory => Arc::new(
                CepAuditStore::in_memory()
                    .map_err(|err| LancetError::Internal(format!("audit store setup: {err}")))?,
            ),
            AuditBacking::File(path) => Arc::new(
                CepAuditStore::open(&path)
                    .map_err(|err| LancetError::Internal(format!("audit store setup: {err}")))?,
            ),
        };

        let audit_context = Arc::new(RwLock::new(CepAuditContext {
            model_id,
            runtime_version,
            ..Default::default()
        }));
        let cep_context = Arc::new(CepTurnContext::from_policy(cep_policy));
        let active_envelope = Arc::new(ActiveEnvelopeStore::new());
        let default_hook = Arc::new(DefaultPolicyHook::new(policy));

        let invariants_dir = invariants_dir.as_deref().and_then(Path::to_str);
        let invariant_index = load_invariant_index_from_dir(&resolve_invariants_dir(
            Path::new(&workspace_root),
            invariants_dir,
        ));

        let server_config = ServerRunConfig {
            audit: Arc::clone(&audit),
            audit_context: Arc::clone(&audit_context),
            base_envelope,
            cep_context,
            active_envelope,
            policy_hook: default_hook.clone(),
            default_policy_hook: Some(default_hook),
            cep_mode,
            command_isolation,
            invariant_index,
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

/// Project a unified [`PolicyConfig`] onto the executor-facing [`CepPolicyConfig`].
pub fn cep_policy_from_policy_config(config: &PolicyConfig) -> CepPolicyConfig {
    CepPolicyConfig {
        project_root: config.project_root.clone(),
        observation: config.observation.clone(),
        network: config.network.clone(),
        budget: config.budget.clone(),
        command: config.command.clone(),
        denied_effect_kinds: config.denied_effect_kinds.clone(),
    }
}

/// Build the turn base observation envelope from policy and the enabled tools.
///
/// Grants session-state for envelope/todo methods, per-host fetch capabilities
/// for allowed network hosts, and read-audit; never write or command scopes.
pub fn base_observation_envelope(
    project_root: &str,
    enabled_tools: &[String],
    policy: &CepPolicyConfig,
) -> Result<BaseExecutionEnvelope, LancetError> {
    let mut base_envelope =
        BaseExecutionEnvelope::for_workspace_root(project_root, "cep_context turn observation");
    if enabled_tools
        .iter()
        .any(|name| name == TodoWrite::NAME || name == Envelope::NAME)
    {
        base_envelope = base_envelope.with_session_state();
    }
    if enabled_tools.iter().any(|name| name == Fetch::NAME) {
        for host in &policy.network.allowed_hosts {
            let host = host.trim();
            if !host.is_empty() {
                base_envelope = base_envelope.with_fetch_host(host);
            }
        }
    }
    base_envelope = base_envelope.with_read_audit();
    base_envelope
        .validate_no_write_or_command()
        .map_err(|err| LancetError::Internal(format!("invalid base envelope: {err}")))?;
    Ok(base_envelope)
}
