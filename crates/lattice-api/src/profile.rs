// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public profile loading API.

use std::path::Path;

use crate::session::{ApiError, Result};

pub use lattice_profile::LoadedAgentProfile;

/// 校验 agents 目录、按名称查找 profile，并核对声明名称与所选名称一致。
pub fn resolve_named_agent_profile(
    agent_name: &str,
    project_root: &Path,
    global_agents_dir: &Path,
) -> Result<LoadedAgentProfile> {
    crate::validate_agents_dir(global_agents_dir)?;
    let paths = lattice_profile::DiscoveryPaths {
        project_root: project_root.to_path_buf(),
        global_agents_dir: global_agents_dir.to_path_buf(),
    };
    let loaded = lattice_profile::resolve_agent_profile(
        lattice_profile::ProfileSource::Name(agent_name.to_string()),
        &paths,
    )
    .map_err(|error| crate::ConfigRejection::profile(error, agent_name))?;
    if loaded.profile.agent.name != agent_name {
        return Err(ApiError::Configuration(crate::ConfigRejection(
            crate::config_rejection::Reason::AgentName {
                path: loaded.path.clone(),
                selected: agent_name.into(),
            },
        )));
    }
    Ok(loaded)
}
