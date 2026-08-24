// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Construction inputs for a LANCET runtime.

use std::path::Path;

use lancet_authority::policy::default::PolicyConfig;
use lancet_protocol::evidence::CapabilityAdmissionPolicy;
use lancet_protocol::policy::CepMode;
use lancet_runtime::{WorkspaceHistoryBacking, WorkspaceHistoryError};

/// Private workspace-history store backing under a caller-supplied state root.
pub fn workspace_history_backing_in(
    state_root: &Path,
    project_root: &Path,
) -> Result<WorkspaceHistoryBacking, WorkspaceHistoryError> {
    WorkspaceHistoryBacking::for_project_in(state_root, project_root)
}

/// Private workspace-history store backing under the Linux XDG state directory.
pub fn workspace_history_backing(
    project_root: &Path,
) -> Result<WorkspaceHistoryBacking, WorkspaceHistoryError> {
    WorkspaceHistoryBacking::for_project(project_root)
}

/// Inputs for [`crate::LancetRuntime::build`].
///
/// Policy is supplied as fully-resolved LANCET-native types. The authoritative
/// invocation ledger and workspace-history repository are both strict project
/// stores; disabled backings are valid only in lower-level tests that do not
/// build a production runtime.
pub struct LancetRuntimeConfig {
    /// Tool names exposed to the model/caller.
    pub enabled_tools: Vec<String>,
    /// Fully-resolved authority/policy configuration.
    pub policy: PolicyConfig,
    /// Component Tool admission inputs. Fail-closed by default.
    pub capability_admission: CapabilityAdmissionPolicy,
    /// CEP enforcement mode.
    pub cep_mode: CepMode,
    /// Authoritative project invocation ledger backing.
    pub invocations: WorkspaceHistoryBacking,
    /// Private workspace mutation history backing.
    pub workspace_history: WorkspaceHistoryBacking,
}

impl LancetRuntimeConfig {
    /// Minimal fail-closed config rooted at `project_root`.
    pub fn new(project_root: impl Into<std::path::PathBuf>) -> Self {
        let policy = PolicyConfig::defaults_for_project_root(project_root.into());
        Self {
            enabled_tools: Vec::new(),
            policy,
            capability_admission: CapabilityAdmissionPolicy::default(),
            cep_mode: CepMode::Enforce,
            invocations: WorkspaceHistoryBacking::Disabled,
            workspace_history: WorkspaceHistoryBacking::Disabled,
        }
    }
}
