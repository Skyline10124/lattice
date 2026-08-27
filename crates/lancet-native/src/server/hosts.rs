// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lancet_authority::policy::CepTurnContext;
use lancet_authority::policy::default::DefaultPolicyHook;
use lancet_protocol::CepError;
use lancet_protocol::cell::ToolDescriptor;
use lancet_runtime::{
    BudgetCharge, BudgetRefusal, BudgetRemaining, ComponentRuntime, LocalWorkspace,
    NetworkFetchHost, WorkspaceHistory, WorkspaceMutationHost, WorkspaceObservationHost,
};
use std::sync::Arc;

pub(super) fn component_native_collisions<'a>(
    native_names: impl IntoIterator<Item = &'a str>,
    component_names: &[&str],
) -> Result<(), CepError> {
    let collisions: Vec<&str> = native_names
        .into_iter()
        .filter(|name| component_names.contains(name))
        .collect();
    if collisions.is_empty() {
        return Ok(());
    }
    Err(CepError::internal_error(format!(
        "method name(s) {collisions:?} resolve to both a Component Tool and a native \
         catalog method; a migrated method must be removed from the native catalog \
         (ADR 0004: no permanent native/WASM dual dispatch)"
    )))
}

pub(super) fn is_component_method(components: Option<&ComponentRuntime>, method: &str) -> bool {
    components.is_some_and(|rt| rt.descriptor(method).is_some())
}

pub(super) fn workspace_observation_host(
    descriptor: &ToolDescriptor,
    workspace: &Arc<LocalWorkspace>,
    cep_context: &Arc<CepTurnContext>,
    default_policy_hook: Option<&DefaultPolicyHook>,
) -> Option<Arc<WorkspaceObservationHost>> {
    use lancet_protocol::cell::ChannelKind;

    let workspace_scopes: Vec<&lancet_protocol::cell::ScopeDecl> = descriptor
        .scopes
        .iter()
        .filter(|scope| scope.channel == ChannelKind::Workspace)
        .collect();
    let [scope] = workspace_scopes.as_slice() else {
        return None;
    };

    let visibility_policy = default_policy_hook
        .and_then(|hook| hook.config().workspace.read_visibility_policy().ok())
        .unwrap_or_default();

    let budget = Arc::clone(&cep_context.budget);
    let charge: BudgetCharge = Arc::new(move |bytes: usize| {
        budget
            .try_charge_observation(bytes)
            .map_err(|exhausted| BudgetRefusal {
                limit: exhausted.limit,
                requested: exhausted.requested,
                remaining: exhausted.remaining,
            })
    });

    Some(Arc::new(WorkspaceObservationHost::new(
        Arc::clone(workspace),
        visibility_policy,
        cep_context.policy.observation.clone(),
        charge,
        scope.name.clone(),
    )))
}

/// Build the GET-only network host for the migrated `fetch.web` Component.
pub(super) fn network_fetch_host(
    descriptor: &ToolDescriptor,
    method: &str,
    cep_context: &Arc<CepTurnContext>,
) -> Option<Arc<NetworkFetchHost>> {
    use lancet_protocol::cell::ChannelKind;

    if method != "fetch.web" {
        return None;
    }
    let network_scope = descriptor
        .scopes
        .iter()
        .find(|scope| scope.channel == ChannelKind::Network)?;
    let charge_budget = Arc::clone(&cep_context.budget);
    let charge: BudgetCharge = Arc::new(move |bytes: usize| {
        charge_budget
            .try_charge_observation(bytes)
            .map_err(|exhausted| BudgetRefusal {
                limit: exhausted.limit,
                requested: exhausted.requested,
                remaining: exhausted.remaining,
            })
    });
    let remaining_budget = Arc::clone(&cep_context.budget);
    let remaining: BudgetRemaining = Arc::new(move || {
        let snapshot = remaining_budget.snapshot();
        snapshot
            .max_turn_observation_bytes
            .saturating_sub(snapshot.turn_observation_bytes_used)
    });
    Some(Arc::new(NetworkFetchHost::new(
        cep_context.policy.network.clone(),
        charge,
        remaining,
        network_scope.name.clone(),
    )))
}

pub(super) fn workspace_mutation_host(
    descriptor: &ToolDescriptor,
    method: &str,
    workspace: &Arc<LocalWorkspace>,
    cep_context: &Arc<CepTurnContext>,
    default_policy_hook: Option<&DefaultPolicyHook>,
    history: Arc<WorkspaceHistory>,
) -> Option<Arc<WorkspaceMutationHost>> {
    use lancet_protocol::cell::ChannelKind;

    if !matches!(method, "mutate.create" | "mutate.edit") {
        return None;
    }

    let workspace_scopes: Vec<&lancet_protocol::cell::ScopeDecl> = descriptor
        .scopes
        .iter()
        .filter(|scope| scope.channel == ChannelKind::Workspace)
        .collect();
    let [scope] = workspace_scopes.as_slice() else {
        return None;
    };

    let region = default_policy_hook
        .map(|hook| {
            hook.config()
                .workspace
                .authorized_region_for(method, &cep_context.policy)
        })
        .unwrap_or_else(lancet_authority::effect::region::AuthorizedEffectRegion::empty);

    Some(Arc::new(WorkspaceMutationHost::new(
        Arc::clone(workspace),
        region,
        Some(history),
        scope.name.clone(),
    )))
}
