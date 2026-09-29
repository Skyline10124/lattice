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
/// 策略和工作区历史绑定由组成层冻结；调用事实由会话库持有。
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
    /// 提交端冻结的 DER 信任根；只用于本 Turn，不读取后台环境或证书路径。
    pub tls_roots_der: Vec<Vec<u8>>,
    /// Private workspace mutation history backing.
    pub workspace_history: WorkspaceHistoryBacking,
}
