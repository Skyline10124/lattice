// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Construction inputs for a LANCET runtime.

use std::path::Path;

use lancet_authority::policy::default::PolicyConfig;
use lancet_protocol::evidence::CapabilityAdmissionPolicy;
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
/// Policy is supplied as fully resolved LANCET types. The authoritative
/// invocation ledger and workspace-history repository are both strict project
/// stores; disabled backings are valid only in lower-level tests that do not
/// build a production runtime.
pub struct LancetRuntimeConfig {
    /// Canonicalizable full Local Workspace root used for project-store
    /// addressing. `policy.project_root` may narrow authority to a subdirectory
    /// but may never identify a different workspace.
    pub local_workspace_root: std::path::PathBuf,
    /// Tool names exposed to the model/caller.
    pub enabled_tools: Vec<String>,
    /// Fully-resolved authority/policy configuration.
    pub policy: PolicyConfig,
    /// Component Tool admission inputs. Fail-closed by default.
    pub capability_admission: CapabilityAdmissionPolicy,
    /// Authoritative project invocation ledger backing.
    pub invocations: WorkspaceHistoryBacking,
    /// Private workspace mutation history backing.
    pub workspace_history: WorkspaceHistoryBacking,
}
