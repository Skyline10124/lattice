// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Thin composition of the durable InvocationStore with Component execution.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lancet_audit::invocation_store::{InvocationStore, InvocationStoreError};
use lancet_authority::{CepPolicyConfig, CepTurnContext};
use lancet_protocol::{CepError, CepErrorCode, InvocationBinding, Retriability, ToolEvent};
use lancet_runtime::{
    CancellationSender, CatalogSelectionError, ComponentRuntime, ExecutionReceipt,
    RuntimeExecution, ToolDefinition, WorkspaceHistory, WorkspaceHistoryBacking,
    enabled_tool_definitions,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::config::LancetRuntimeConfig;
use crate::error::LancetError;

/// Result of the reservation-gated handoff.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// The exact invocation never crossed durable `DispatchStarted`.
    NotExecuted(CepError),
    /// The Component returned a known result/evidence receipt after handoff.
    Executed(ExecutionReceipt),
}

/// One opaque, turn-scoped generated-Component runtime.
pub struct LancetRuntime {
    execution: RuntimeExecution,
    invocations: Arc<InvocationStore>,
    tool_definitions: Vec<ToolDefinition>,
    workspace_history: Arc<WorkspaceHistory>,
    events: Option<mpsc::UnboundedReceiver<ToolEvent>>,
}

impl LancetRuntime {
    /// Compose the generated catalog and strictly open the project stores.
    pub fn build(config: LancetRuntimeConfig) -> Result<Self, LancetError> {
        let LancetRuntimeConfig {
            local_workspace_root,
            enabled_tools,
            mut policy,
            capability_admission,
            invocations,
            workspace_history,
        } = config;

        policy.project_root = validate_project_store_bindings(
            &local_workspace_root,
            &policy.project_root,
            &invocations,
            &workspace_history,
        )?;

        let components = ComponentRuntime::shared()
            .map_err(|error| LancetError::Internal(format!("component composition: {error}")))?;
        let tool_definitions = enabled_tool_definitions(&components, &enabled_tools)
            .map_err(map_catalog_selection_error)?;
        lancet_runtime::validate_policy_against_catalog(
            components
                .descriptors()
                .flat_map(|descriptor| descriptor.capabilities.iter())
                .map(|capability| capability.name.as_str()),
            &capability_admission,
        )
        .map_err(|error| LancetError::Config(format!("capability admission: {error}")))?;

        let cep_policy = CepPolicyConfig {
            project_root: policy.project_root.clone(),
            observation: policy.observation.clone(),
            network: policy.network.clone(),
            capability_admission,
        };
        let invocations = open_invocation_store(&invocations)?;
        let workspace_history =
            Arc::new(WorkspaceHistory::open(workspace_history).map_err(|error| {
                LancetError::Internal(format!("workspace history setup: {error}"))
            })?);
        let (execution, events) = RuntimeExecution::new(
            components,
            Arc::new(CepTurnContext::from_policy(cep_policy)),
            &policy,
            Arc::clone(&workspace_history),
        )
        .map_err(LancetError::Config)?;

        Ok(Self {
            execution,
            invocations,
            tool_definitions,
            workspace_history,
            events: Some(events),
        })
    }

    /// Frozen provider-visible definitions from generated descriptors.
    pub fn tool_definitions(&self) -> &[ToolDefinition] {
        &self.tool_definitions
    }

    /// Take the sole event receiver for this Turn runtime.
    pub fn take_events(&mut self) -> mpsc::UnboundedReceiver<ToolEvent> {
        self.events
            .take()
            .expect("Tool event receiver may be taken only once")
    }

    /// Private mutation history sampled after all dispatches complete.
    pub fn workspace_history(&self) -> &Arc<WorkspaceHistory> {
        &self.workspace_history
    }

    /// Durably admit an exact reservation, then hand it to Component execution.
    pub async fn dispatch(
        &self,
        binding: InvocationBinding,
        method: String,
        params: Value,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> DispatchOutcome {
        if !binding.matches_request(&method, &params) {
            return DispatchOutcome::NotExecuted(CepError::invalid_params(
                "invocation binding does not match request method/arguments",
            ));
        }

        let store = Arc::clone(&self.invocations);
        let admission_binding = binding.clone();
        match tokio::task::spawn_blocking(move || store.admit_reserved(&admission_binding)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return DispatchOutcome::NotExecuted(admission_error(error)),
            Err(_) => return DispatchOutcome::NotExecuted(admission_worker_error()),
        }

        #[cfg(feature = "test-crash-barriers")]
        stop_after_dispatch_started(&self.invocations, binding.invocation_id);

        let (sender, cancellation) = CancellationSender::pair();
        let monitor = cancel_flag.map(|flag| {
            let sender = sender.clone();
            tokio::spawn(async move {
                while !flag.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                sender.cancel();
            })
        });
        let _guard = DispatchGuard { sender, monitor };
        DispatchOutcome::Executed(
            self.execution
                .call(binding, method, params, cancellation)
                .await,
        )
    }
}

struct DispatchGuard {
    sender: CancellationSender,
    monitor: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        self.sender.cancel();
        if let Some(monitor) = self.monitor.take() {
            monitor.abort();
        }
    }
}

fn open_invocation_store(
    backing: &WorkspaceHistoryBacking,
) -> Result<Arc<InvocationStore>, LancetError> {
    match backing {
        WorkspaceHistoryBacking::Project { layout, key } => {
            Ok(Arc::new(InvocationStore::open(layout, key).map_err(
                |error| LancetError::Internal(format!("invocation store setup: {error}")),
            )?))
        }
        WorkspaceHistoryBacking::Disabled => Err(LancetError::Config(
            "production runtime requires a project InvocationStore backing".into(),
        )),
    }
}

fn admission_worker_error() -> CepError {
    CepError::tool_execution(
        CepErrorCode::ExecutionFailed,
        "invocation",
        "admission_worker_failed",
        None,
        Retriability::Permanent,
    )
}

fn admission_error(error: InvocationStoreError) -> CepError {
    let (code, category) = match error {
        InvocationStoreError::InvalidTransition { .. }
        | InvocationStoreError::DifferentPlan { .. }
        | InvocationStoreError::DuplicateInvocation { .. }
        | InvocationStoreError::InvalidReservation { .. } => {
            (CepErrorCode::ResourceConflict, "reservation_state")
        }
        InvocationStoreError::Corruption { .. } => {
            (CepErrorCode::Reserved, "invocation_store_corruption")
        }
        _ => (
            CepErrorCode::ExecutionFailed,
            "invocation_store_unavailable",
        ),
    };
    CepError::tool_execution(code, "invocation", category, None, Retriability::Permanent)
}

fn map_catalog_selection_error(error: CatalogSelectionError) -> LancetError {
    LancetError::UnknownTools {
        unknown: error.unknown,
        available: error.available,
    }
}

fn validate_project_store_bindings(
    local_workspace_root: &std::path::Path,
    policy_root: &std::path::Path,
    invocations: &WorkspaceHistoryBacking,
    workspace_history: &WorkspaceHistoryBacking,
) -> Result<std::path::PathBuf, LancetError> {
    let canonical_workspace = std::fs::canonicalize(local_workspace_root).map_err(|error| {
        LancetError::Config(format!(
            "cannot canonicalize Local Workspace root '{}': {error}",
            local_workspace_root.display()
        ))
    })?;
    let canonical_policy = std::fs::canonicalize(policy_root).map_err(|error| {
        LancetError::Config(format!(
            "cannot canonicalize policy project root '{}': {error}",
            policy_root.display()
        ))
    })?;
    if !canonical_policy.starts_with(&canonical_workspace) {
        return Err(LancetError::Config(format!(
            "policy project root '{}' resolves outside Local Workspace root '{}'",
            canonical_policy.display(),
            canonical_workspace.display()
        )));
    }

    let expected_key =
        lancet_runtime::ProjectStoreKey::derive(&canonical_workspace).map_err(|error| {
            LancetError::Config(format!("derive Local Workspace project key: {error}"))
        })?;
    let (invocation_layout, invocation_key) = project_backing(invocations, "InvocationStore")?;
    let (history_layout, history_key) = project_backing(workspace_history, "WorkspaceHistory")?;
    if invocation_key != expected_key || history_key != expected_key {
        return Err(LancetError::Config(
            "project store key does not match canonical Local Workspace".into(),
        ));
    }
    if invocation_layout != history_layout {
        return Err(LancetError::Config(
            "InvocationStore and WorkspaceHistory use different StateLayout roots".into(),
        ));
    }
    Ok(canonical_policy)
}

fn project_backing<'a>(
    backing: &'a WorkspaceHistoryBacking,
    name: &str,
) -> Result<
    (
        &'a lancet_runtime::StateLayout,
        lancet_runtime::ProjectStoreKey,
    ),
    LancetError,
> {
    match backing {
        WorkspaceHistoryBacking::Project { layout, key } => Ok((layout, *key)),
        WorkspaceHistoryBacking::Disabled => Err(LancetError::Config(format!(
            "production runtime requires a project {name} backing"
        ))),
    }
}

#[cfg(feature = "test-crash-barriers")]
fn stop_after_dispatch_started(
    invocations: &InvocationStore,
    invocation_id: lancet_protocol::InvocationId,
) {
    use std::io::Write;

    if std::env::var_os("LATTICE_CRASH_BARRIER").as_deref()
        != Some(std::ffi::OsStr::new("dispatch_started"))
    {
        return;
    }
    let facts = invocations
        .query_invocations(&[invocation_id])
        .expect("read dispatch barrier facts");
    let path = std::env::var_os("LATTICE_CRASH_BARRIER_ACK").expect("barrier ack path");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .expect("create barrier ack");
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "invocation_id": invocation_id,
            "state": facts.get(&invocation_id).map(|record| format!("{:?}", record.state)),
        }),
    )
    .expect("write barrier ack");
    file.write_all(b"\n").expect("write barrier newline");
    file.sync_all().expect("sync barrier ack");
    unsafe { libc::raise(libc::SIGSTOP) };
}
