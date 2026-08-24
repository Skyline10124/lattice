// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! LANCET runtime composition: spawn a native CEP server and hand back a client.

use std::sync::Arc;

use lancet_authority::policy::default::DefaultPolicyHook;
use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_catalog::catalog;
use lancet_cep::{
    ToolDefinition, ToolExposureError, project_tool_definitions, resolve_enabled_tools,
};
use lancet_dispatch::{CepClient, CepTransport};
use lancet_native::{
    CepServerNativeExt, MethodRegistry, ServerRunConfig, register_production_method_registry,
};
use lancet_runtime::{ComponentRuntime, WorkspaceHistory, WorkspaceHistoryBacking};

use crate::config::LancetRuntimeConfig;
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
    ///
    /// Composes the production Component runtime once from embedded inventory
    /// and the generated `EXPECTED_COMPONENTS` identity list, then delegates
    /// to [`LancetServer::spawn_composed`]. There is no public path to inject
    /// Component bytes, paths, or handlers; the catalog is closed at build
    /// time (ADR 0004).
    pub fn spawn(
        config: ServerRunConfig,
        channel_capacity: usize,
    ) -> Result<(CepClient, ServerLifecycle), LancetError> {
        let components = Arc::new(
            ComponentRuntime::from_embedded()
                .map_err(|err| LancetError::Internal(format!("component composition: {err}")))?,
        );
        Self::spawn_composed(config, channel_capacity, components)
    }

    /// Spawn a native CEP server with a pre-composed Component runtime.
    ///
    /// `pub(crate)`: the only public entry ([`LancetServer::spawn`]) composes
    /// from embedded inventory. Tests inject a fixed no-op fixture through
    /// this seam (D.6). The same `Arc<ComponentRuntime>` is threaded into
    /// `ServerRunConfig.components` so `handle_request` can branch Component
    /// vs native dispatch within the existing audit lifecycle (D.3).
    pub(crate) fn spawn_composed(
        mut config: ServerRunConfig,
        channel_capacity: usize,
        components: Arc<ComponentRuntime>,
    ) -> Result<(CepClient, ServerLifecycle), LancetError> {
        config.components = Some(Arc::clone(&components));

        // #28 C2: catalog and admission policy first coexist here. The server
        // run refuses an unknown deny name too, but surfacing it during
        // composition is what turns it into a build error instead of a server
        // task that dies after the client is already handed out.
        config.validate_capability_admission().map_err(|err| {
            LancetError::Internal(format!("capability admission: {}", err.message))
        })?;

        // #28 design §11: one method name must not resolve to both a Component
        // Tool and a native catalog method. Surfacing it here makes it a build
        // error instead of a server task that dies after the client is handed
        // out.
        config
            .validate_no_component_native_collision()
            .map_err(|err| LancetError::Internal(format!("catalog collision: {}", err.message)))?;

        let mut registry = MethodRegistry::new();
        register_production_method_registry(&mut registry).map_err(|err| {
            LancetError::Internal(format!("production method registry registration: {err:?}"))
        })?;

        let (client, server) = CepTransport::pair(channel_capacity);
        let shutdown = server.shutdown_handle();
        let server_task = tokio::spawn(async move {
            let _ = server.run_native(registry, config).await;
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
    /// Private workspace mutation history backing this runtime (#63).
    pub workspace_history: Arc<WorkspaceHistory>,
    /// Server lifecycle; keep alive while the client is used.
    pub lifecycle: ServerLifecycle,
}

/// A fully-composed LANCET runtime: a live native CEP server plus its client.
///
/// This is the unified entry point for driving LANCET independently of any host
/// application. Build it from a [`LancetRuntimeConfig`], call typed methods via
/// [`LancetRuntime::client`], and read finalized operator audit rows through
/// the direct-store API.
pub struct LancetRuntime {
    client: CepClient,
    tool_definitions: Vec<ToolDefinition>,
    workspace_history: Arc<WorkspaceHistory>,
    lifecycle: ServerLifecycle,
}

impl LancetRuntime {
    /// Compose and spawn a production LANCET runtime from `config`.
    ///
    /// D.2: composes the Component runtime once from embedded inventory +
    /// generated `EXPECTED_COMPONENTS`. The same `Arc<ComponentRuntime>`
    /// is threaded into `ServerRunConfig.components` for dispatch (D.3)
    /// and projected into LLM tool definitions (D.4).
    pub fn build(config: LancetRuntimeConfig) -> Result<Self, LancetError> {
        let LancetRuntimeConfig {
            enabled_tools,
            policy,
            capability_admission,
            cep_mode,
            invocations,
            workspace_history,
        } = config;

        // D.2: compose the Component runtime once from embedded inventory +
        // the generated `EXPECTED_COMPONENTS`. Since #29 that is non-empty: it
        // embeds `session.todo_write` (#30–#32 add the rest). The same instance
        // serves dispatch (D.3) and LLM projection (D.4).
        let components = Arc::new(
            ComponentRuntime::from_embedded()
                .map_err(|err| LancetError::Internal(format!("component composition: {err}")))?,
        );

        // D.4: project Component descriptors into LLM tool definitions.
        // The available set is native catalog methods + Component Tools;
        // enabled_tools is resolved against the combined set (spec D.4:
        // profile enabled_tools ⊆ (5 native + built Components): LLM exposure).
        let mut available = project_tool_definitions(catalog());
        available.extend(project_component_definitions(&components));
        let tool_definitions =
            resolve_enabled_tools(&enabled_tools, &available).map_err(map_tool_exposure_err)?;

        let cep_policy = CepPolicyConfig {
            project_root: policy.project_root.clone(),
            observation: policy.observation.clone(),
            network: policy.network.clone(),
            denied_effect_kinds: policy.denied_effect_kinds.clone(),
            capability_admission,
        };

        // ADR 0003 / #38: the runtime carries no execution-routing field.
        // The native server constructs the one concrete Local Workspace from
        // the resolved policy's canonical project root when the run starts;
        // embedders cannot inject an alternative workspace implementation.
        let invocations = match invocations {
            WorkspaceHistoryBacking::Project { layout, key } => Arc::new(
                lancet_audit::invocation_store::InvocationStore::open(&layout, &key).map_err(
                    |err| LancetError::Internal(format!("invocation store setup: {err}")),
                )?,
            ),
            WorkspaceHistoryBacking::Disabled => {
                return Err(LancetError::Internal(
                    "production runtime requires a project InvocationStore backing".into(),
                ));
            }
        };

        // #63: the private mutation history repository is opened once per
        // runtime, before the server starts, so a turn can never install a
        // mutation the history is not ready to record.
        let workspace_history = Arc::new(
            WorkspaceHistory::open(workspace_history)
                .map_err(|err| LancetError::Internal(format!("workspace history setup: {err}")))?,
        );

        let cep_context = Arc::new(CepTurnContext::from_policy(cep_policy));
        let default_hook = Arc::new(DefaultPolicyHook::new(policy));

        let server_config = ServerRunConfig {
            invocations,
            test_state_guard: None,
            cep_context,
            policy_hook: default_hook.clone(),
            default_policy_hook: Some(default_hook),
            cep_mode,
            components: None, // set by spawn_composed
            workspace_history: Arc::clone(&workspace_history),
        };

        let (client, lifecycle) =
            LancetServer::spawn_composed(server_config, DEFAULT_CHANNEL_CAPACITY, components)?;

        Ok(Self {
            client,
            tool_definitions,
            workspace_history,
            lifecycle,
        })
    }

    /// Clone of the CEP client for typed method dispatch.
    pub fn client(&self) -> CepClient {
        self.client.clone()
    }

    /// Catalog-derived tool definitions filtered to the enabled set.
    pub fn tool_definitions(&self) -> &[ToolDefinition] {
        &self.tool_definitions
    }

    /// Private durable history of successfully installed Atomic Local
    /// Mutations (#63).
    ///
    /// Hosts read [`WorkspaceHistory::current`] at the end of a Turn to record
    /// the workspace-history object id the Turn arrived at.
    pub fn workspace_history(&self) -> &Arc<WorkspaceHistory> {
        &self.workspace_history
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
            workspace_history: self.workspace_history,
            lifecycle: self.lifecycle,
        }
    }
}

fn map_tool_exposure_err(err: ToolExposureError) -> LancetError {
    LancetError::UnknownTools {
        unknown: err.unknown,
        available: err.available,
    }
}

/// D.4: Project Component Tool descriptors into LLM-facing tool definitions.
///
/// The name/description/params schema come only from the generated descriptor
/// JSON — the adapter does not hand-write method/schema matches. Since #29 a
/// production build projects `session.todo_write` from its manifest-generated
/// descriptor; tests may inject a no-op fixture instead.
fn project_component_definitions(components: &ComponentRuntime) -> Vec<ToolDefinition> {
    components
        .descriptor_identities()
        .map(|(desc, descriptor_identity)| {
            ToolDefinition::new(
                desc.name.clone(),
                desc.description.clone(),
                desc.params_schema.clone(),
                descriptor_identity,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn production_config() -> (tempfile::TempDir, tempfile::TempDir, LancetRuntimeConfig) {
        let state = tempfile::TempDir::new().expect("state root");
        let project = tempfile::TempDir::new().expect("project root");
        let backing = WorkspaceHistoryBacking::for_project_in(state.path(), project.path())
            .expect("project backing");
        let (layout, key) = match &backing {
            WorkspaceHistoryBacking::Project { layout, key } => (layout.clone(), *key),
            WorkspaceHistoryBacking::Disabled => unreachable!(),
        };
        WorkspaceHistory::create(backing.clone()).expect("workspace history");
        lancet_audit::invocation_store::InvocationStore::create(&layout, &key)
            .expect("invocation store");
        let mut config = LancetRuntimeConfig::new(project.path());
        config.invocations = backing.clone();
        config.workspace_history = backing;
        (state, project, config)
    }

    #[test]
    fn runtime_requires_authoritative_project_stores() {
        let project = tempfile::TempDir::new().expect("project root");
        let err = match LancetRuntime::build(LancetRuntimeConfig::new(project.path())) {
            Ok(_) => panic!("disabled stores must fail closed"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("InvocationStore"));
    }

    #[tokio::test]
    async fn runtime_projects_manifest_tools_without_sqlite_authority() {
        let (_state, _project, mut config) = production_config();
        config.enabled_tools = vec![
            "observe.workspace".into(),
            "session.todo_write".into(),
            "fetch.web".into(),
        ];
        let mut runtime = LancetRuntime::build(config).expect("runtime");
        let names = runtime
            .tool_definitions()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert!(names.contains("observe.workspace"));
        assert!(names.contains("session.todo_write"));
        assert!(names.contains("fetch.web"));
        runtime.shutdown().await;
    }
}
