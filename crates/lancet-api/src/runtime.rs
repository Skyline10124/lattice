// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Thin composition of the durable InvocationStore with Component execution.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lancet_audit::invocation_store::{InvocationState, InvocationStore, InvocationStoreError};
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

#[cfg(test)]
static FAIL_NEXT_ADMISSION_REF_FSYNC: AtomicBool = AtomicBool::new(false);

/// Result of the reservation-gated handoff.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// The exact invocation never crossed durable `DispatchStarted`.
    NotExecuted(CepError),
    /// Durable state may have crossed `DispatchStarted`; preserve pending state
    /// and let the coordinator reconcile from authoritative invocation facts.
    ReconciliationRequired(CepError),
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
        let admission_store = Arc::clone(&store);
        let admission_binding = binding.clone();
        match tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if FAIL_NEXT_ADMISSION_REF_FSYNC.swap(false, Ordering::SeqCst) {
                lattice_store::fault::fail_ref_fsync(true);
            }
            admission_store.admit_reserved(&admission_binding)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return classify_admission_failure(
                    store,
                    binding.invocation_id,
                    admission_error(error),
                )
                .await;
            }
            Err(_) => {
                return classify_admission_failure(
                    store,
                    binding.invocation_id,
                    admission_worker_error(),
                )
                .await;
            }
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

async fn classify_admission_failure(
    store: Arc<InvocationStore>,
    invocation_id: lancet_protocol::InvocationId,
    error: CepError,
) -> DispatchOutcome {
    let observed = tokio::task::spawn_blocking(move || {
        store.query_invocations(std::slice::from_ref(&invocation_id))
    })
    .await;
    let definitely_not_executed = matches!(
        observed,
        Ok(Ok(ref facts))
            if facts
                .get(&invocation_id)
                .is_some_and(|record| matches!(record.state, InvocationState::Reserved))
    );
    if definitely_not_executed {
        DispatchOutcome::NotExecuted(error)
    } else {
        DispatchOutcome::ReconciliationRequired(error)
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
        Retriability::Unknown,
    )
}

fn admission_error(error: InvocationStoreError) -> CepError {
    let retriability = if error.is_durability_retryable() {
        Retriability::Transient
    } else {
        Retriability::Permanent
    };
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
    CepError::tool_execution(code, "invocation", category, None, retriability)
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

#[cfg(test)]
mod tests {
    use super::*;
    use lancet_audit::invocation_store::InvocationState;
    use lancet_protocol::{
        BindingIdentity, InvocationId, PlanKey, SessionId, TurnId, arguments_digest,
    };

    #[tokio::test]
    async fn published_dispatch_started_is_not_classified_not_executed() {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(workspace.path()).unwrap();
        let backing = WorkspaceHistoryBacking::for_project_in(state.path(), &root).unwrap();
        WorkspaceHistory::create(backing.clone()).unwrap();
        let (layout, key) = match &backing {
            WorkspaceHistoryBacking::Project { layout, key } => (layout, *key),
            WorkspaceHistoryBacking::Disabled => unreachable!(),
        };
        InvocationStore::create(layout, &key).unwrap();

        let runtime = LancetRuntime::build(LancetRuntimeConfig {
            local_workspace_root: root.clone(),
            enabled_tools: Vec::new(),
            policy: lancet_authority::policy::default::PolicyConfig::defaults_for_project_root(
                root,
            ),
            capability_admission: Default::default(),
            invocations: backing.clone(),
            workspace_history: backing,
        })
        .unwrap();
        let turn_id = TurnId::parse("22222222-2222-4222-8222-222222222222").unwrap();
        let params = serde_json::json!({"value": "test"});
        let binding = InvocationBinding {
            invocation_id: InvocationId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
            identity: BindingIdentity {
                session_id: SessionId::parse("11111111111111111111111111111111").unwrap(),
                turn_id,
                model_step_ordinal: 0,
                part_ordinal: 0,
                method: "test.echo".to_string(),
                descriptor_identity: "test-descriptor".to_string(),
                arguments_digest: arguments_digest(&params),
            },
        };
        runtime
            .invocations
            .reserve_batch(
                PlanKey {
                    turn_id,
                    model_step_ordinal: 0,
                },
                std::slice::from_ref(&binding),
            )
            .unwrap();

        FAIL_NEXT_ADMISSION_REF_FSYNC.store(true, Ordering::SeqCst);
        let outcome = runtime
            .dispatch(
                binding.clone(),
                binding.identity.method.clone(),
                params,
                None,
            )
            .await;

        let DispatchOutcome::ReconciliationRequired(error) = outcome else {
            panic!("a visible DispatchStarted fact must require reconciliation");
        };
        assert_eq!(error.retriability, Retriability::Transient);
        let facts = runtime
            .invocations
            .query_invocations(&[binding.invocation_id])
            .unwrap();
        assert!(matches!(
            facts[&binding.invocation_id].state,
            InvocationState::DispatchStarted
        ));
    }
}
