// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Component-only Tool execution over the generated closed catalog.

use std::sync::Arc;

use lancet_authority::policy::default::{PolicyConfig, WorkspacePolicy};
use lancet_authority::{CepTurnContext, ReadVisibilityPolicy};
use lancet_protocol::cell::{ChannelKind, ToolDescriptor};
use lancet_protocol::error::PermissionErrorData;
use lancet_protocol::{
    CellEvidenceTriple, CepError, CepErrorCode, CepErrorData, InvocationBinding, Retriability,
    ToolEvent,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::{
    BudgetCharge, BudgetRefusal, BudgetRemaining, Cancellation, CellEvidenceLedger,
    ComponentCallContext, ComponentRuntime, EventSink, ExecutionReceipt, LocalWorkspace,
    NetworkFetchHost, WorkspaceHistory, WorkspaceMutationHost, WorkspaceObservationHost,
    admit_tool,
};

/// Immutable per-Turn execution state for the generated Component catalog.
pub struct RuntimeExecution {
    components: Arc<ComponentRuntime>,
    context: Arc<CepTurnContext>,
    workspace_policy: WorkspacePolicy,
    visibility_policy: ReadVisibilityPolicy,
    workspace: Arc<LocalWorkspace>,
    history: Arc<WorkspaceHistory>,
    tls: Arc<rustls::ClientConfig>,
    events: mpsc::UnboundedSender<ToolEvent>,
}

impl RuntimeExecution {
    /// 在接受前构造工作区与 TLS；所有调用共享本 Turn 的配置，不读取进程环境。
    pub fn new(
        components: Arc<ComponentRuntime>,
        context: Arc<CepTurnContext>,
        policy: &PolicyConfig,
        history: Arc<WorkspaceHistory>,
        tls_roots_der: &[Vec<u8>],
    ) -> Result<(Self, mpsc::UnboundedReceiver<ToolEvent>), String> {
        let tls = crate::host_network::tls_config(tls_roots_der)?;
        let visibility_policy = policy.workspace.read_visibility_policy()?;
        let workspace = Arc::new(LocalWorkspace::with_history(
            &context.policy.project_root,
            Arc::clone(&history),
        ));
        let (events, receiver) = mpsc::unbounded_channel();
        Ok((
            Self {
                components,
                context,
                workspace_policy: policy.workspace.clone(),
                visibility_policy,
                workspace,
                history,
                tls,
                events,
            },
            receiver,
        ))
    }

    /// Admit and execute one exact, already-reserved invocation.
    pub async fn call(
        &self,
        binding: InvocationBinding,
        method: String,
        params: Value,
        cancellation: Cancellation,
    ) -> ExecutionReceipt {
        let invocation_id = binding.invocation_id;
        let evidence = CellEvidenceLedger::new();
        let events = EventSink::new(self.events.clone(), invocation_id);

        if !binding.matches_request(&method, &params) {
            return ExecutionReceipt {
                result: Err(CepError::invalid_params(
                    "invocation binding does not match request method/arguments",
                )),
                evidence: CellEvidenceTriple::refused_before_admission(&method),
            };
        }

        let Some(descriptor) = self.components.descriptor(&method) else {
            return ExecutionReceipt {
                result: Err(lancet_protocol::error::method_not_found(&method)),
                evidence: CellEvidenceTriple::refused_before_admission(&method),
            };
        };

        emit_call_started(&events, invocation_id, &method);
        let result = match admit_tool(descriptor, &self.context.policy.capability_admission) {
            Ok(admitted) => {
                evidence.set_admission(admitted.evidence.clone());
                let workspace = self.workspace_host(&admitted.descriptor, cancellation.clone());
                let mutate = self.mutation_host(&admitted.descriptor, &method);
                let network = self.network_host(&admitted.descriptor, &method);
                let call = ComponentCallContext {
                    invocation_id,
                    turn_id: binding.identity.turn_id.to_string(),
                    events: events.clone(),
                    cancellation,
                    evidence: evidence.clone(),
                    admitted: Arc::new(admitted),
                    workspace,
                    mutate,
                    network,
                };
                self.components.call(&method, params, call).await
            }
            Err(denial) => {
                evidence.set_admission(denial.evidence.clone());
                Err(CepError::new(
                    CepErrorCode::CapabilityNotGranted,
                    format!("{method}: {denial}"),
                    CepErrorData::Permission(PermissionErrorData {
                        required_capability: method.clone(),
                        granted_capabilities: denial.evidence.approved_capabilities,
                    }),
                    Retriability::Permanent,
                ))
            }
        };

        emit_call_finished(&events, invocation_id, &method, &result);
        let (admission, effects, denied) = evidence.snapshot();
        let evidence = admission.map_or_else(
            || CellEvidenceTriple::refused_before_admission(&method),
            |admission| CellEvidenceTriple::new(admission, effects, denied),
        );
        ExecutionReceipt { result, evidence }
    }

    fn workspace_host(
        &self,
        descriptor: &ToolDescriptor,
        cancellation: Cancellation,
    ) -> Option<Arc<WorkspaceObservationHost>> {
        let scope = single_scope(descriptor, ChannelKind::Workspace)?;
        let budget = Arc::clone(&self.context.budget);
        let charge: BudgetCharge = Arc::new(move |bytes| {
            budget
                .try_charge_observation(bytes)
                .map_err(|exhausted| BudgetRefusal {
                    limit: exhausted.limit,
                    requested: exhausted.requested,
                    remaining: exhausted.remaining,
                })
        });
        Some(Arc::new(WorkspaceObservationHost::new_cancellable(
            Arc::clone(&self.workspace),
            self.visibility_policy.clone(),
            self.context.policy.observation.clone(),
            charge,
            scope,
            cancellation,
        )))
    }

    fn mutation_host(
        &self,
        descriptor: &ToolDescriptor,
        method: &str,
    ) -> Option<Arc<WorkspaceMutationHost>> {
        if !matches!(method, "mutate.create" | "mutate.edit") {
            return None;
        }
        let scope = single_scope(descriptor, ChannelKind::Workspace)?;
        Some(Arc::new(WorkspaceMutationHost::new(
            Arc::clone(&self.workspace),
            self.workspace_policy.denied_write_patterns.clone(),
            Some(Arc::clone(&self.history)),
            scope,
        )))
    }

    fn network_host(
        &self,
        descriptor: &ToolDescriptor,
        method: &str,
    ) -> Option<Arc<NetworkFetchHost>> {
        if method != "fetch.web" {
            return None;
        }
        let scope = single_scope(descriptor, ChannelKind::Network)?;
        let charge_budget = Arc::clone(&self.context.budget);
        let charge: BudgetCharge = Arc::new(move |bytes| {
            charge_budget
                .try_charge_observation(bytes)
                .map_err(|exhausted| BudgetRefusal {
                    limit: exhausted.limit,
                    requested: exhausted.requested,
                    remaining: exhausted.remaining,
                })
        });
        let remaining_budget = Arc::clone(&self.context.budget);
        let remaining: BudgetRemaining = Arc::new(move || {
            let snapshot = remaining_budget.snapshot();
            snapshot
                .max_turn_observation_bytes
                .saturating_sub(snapshot.turn_observation_bytes_used)
        });
        Some(Arc::new(NetworkFetchHost::new(
            self.context.policy.network.clone(),
            charge,
            remaining,
            scope,
            Arc::clone(&self.tls),
        )))
    }
}

fn single_scope(descriptor: &ToolDescriptor, channel: ChannelKind) -> Option<String> {
    let mut scopes = descriptor
        .scopes
        .iter()
        .filter(|scope| scope.channel == channel);
    let scope = scopes.next()?;
    scopes.next().is_none().then(|| scope.name.clone())
}

fn emit_call_started(
    events: &EventSink,
    invocation_id: lancet_protocol::InvocationId,
    method: &str,
) {
    events.emit(
        "cep.call_started",
        json!({
            "invocation_id": invocation_id,
            "method": method,
            "semantic_stage": "cell_component",
            "guard_stage": "cell_admission",
            "policy_enforced": true,
        }),
    );
}

fn emit_call_finished(
    events: &EventSink,
    invocation_id: lancet_protocol::InvocationId,
    method: &str,
    result: &Result<Value, CepError>,
) {
    let mut payload = json!({
        "invocation_id": invocation_id,
        "method": method,
        "ok": result.is_ok(),
        "semantic_stage": "cell_component",
        "guard_stage": "cell_admission",
        "policy_enforced": true,
    });
    if let Some(error) = result.as_ref().err()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert(
            "error_code".into(),
            Value::String(format!("{:?}", error.code)),
        );
    }
    events.emit("cep.call_finished", payload);
}
