// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! ServerRunConfig with InvocationStore (#139-B).

use lancet_audit::invocation_store::InvocationStore;
use lancet_authority::policy::CepTurnContext;
use lancet_authority::policy::default::{DefaultPolicyHook, PolicyConfig};
use lancet_protocol::CepError;
use lancet_protocol::evidence::CapabilityAdmissionPolicy;
use lancet_protocol::policy::{CepMode, PolicyHook};
use lancet_runtime::{
    ComponentRuntime, ProjectStoreKey, StateLayout, WorkspaceHistory,
    validate_policy_against_catalog,
};
use std::sync::Arc;
use tempfile::TempDir;

/// Server-side audit and lifecycle configuration for a turn-scoped native runtime.
#[derive(Clone)]
pub struct ServerRunConfig {
    /// Project InvocationStore; server admission advances Reserved -> DispatchStarted.
    pub invocations: Arc<InvocationStore>,
    /// Keeps the temp state root alive for [`Self::in_memory_for_tests`].
    pub test_state_guard: Option<Arc<TempDir>>,
    pub cep_context: Arc<CepTurnContext>,
    pub policy_hook: Arc<dyn PolicyHook>,
    pub default_policy_hook: Option<Arc<DefaultPolicyHook>>,
    pub cep_mode: CepMode,
    pub components: Option<Arc<ComponentRuntime>>,
    pub workspace_history: Arc<WorkspaceHistory>,
}

impl ServerRunConfig {
    /// In-memory audit + temp InvocationStore for tests.
    pub fn in_memory_for_tests() -> Self {
        let tmp = TempDir::new().expect("temp state root");
        let layout = StateLayout::in_state_root(tmp.path());
        let key = ProjectStoreKey::derive(tmp.path()).expect("project key");
        let invocations =
            Arc::new(InvocationStore::create(&layout, &key).expect("create invocation store"));
        let cep_context = Arc::new(CepTurnContext::defaults_for_project_root("."));
        let default_hook = Arc::new(DefaultPolicyHook::new(
            PolicyConfig::defaults_for_project_root(std::path::PathBuf::from(".")),
        ));
        Self {
            invocations,
            test_state_guard: Some(Arc::new(tmp)),
            cep_context,
            policy_hook: default_hook.clone(),
            default_policy_hook: Some(default_hook),
            cep_mode: CepMode::Off,
            components: None,
            workspace_history: Arc::new(WorkspaceHistory::disabled()),
        }
    }

    /// Install the Component Tool admission policy on the turn context.
    pub fn with_capability_admission(mut self, policy: CapabilityAdmissionPolicy) -> Self {
        let mut context = (*self.cep_context).clone();
        context.policy.capability_admission = policy;
        self.cep_context = Arc::new(context);
        self
    }

    /// Refuse a policy whose deny list names capabilities no composed Tool declares.
    pub fn validate_capability_admission(&self) -> Result<(), CepError> {
        let declared: Vec<&str> = self
            .components
            .iter()
            .flat_map(|components| components.descriptors())
            .flat_map(|descriptor| descriptor.capabilities.iter())
            .map(|capability| capability.name.as_str())
            .collect();
        validate_policy_against_catalog(declared, &self.cep_context.policy.capability_admission)
            .map_err(|err| CepError::internal_error(err.to_string()))
    }

    /// Refuse Component/native method-name collisions.
    pub fn validate_no_component_native_collision(&self) -> Result<(), CepError> {
        let Some(components) = self.components.as_ref() else {
            return Ok(());
        };
        let component_names: Vec<&str> = components
            .descriptors()
            .map(|descriptor| descriptor.name.as_str())
            .collect();
        let native_names = lancet_catalog::catalog()
            .iter()
            .map(|descriptor| descriptor.name);
        super::hosts::component_native_collisions(native_names, &component_names)
    }
}
