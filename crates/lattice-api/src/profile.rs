// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public profile loading API.

use std::path::Path;

use crate::session::Result;

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
    .map_err(|error| {
        crate::ApiError::FrontendConfiguration(crate::ConfigurationDiagnostic::new(
            profile_error_message(&error),
        ))
    })?;
    if loaded.profile.agent.name != agent_name {
        return Err(crate::ApiError::FrontendConfiguration(
            crate::ConfigurationDiagnostic::new(format!(
                "[agent].name at {} declares '{}'; must match selected agent '{agent_name}'",
                loaded.path.display(),
                loaded.profile.agent.name,
            )),
        ));
    }
    Ok(loaded)
}

// TOML 的 Display 会附上多行源码；保留解析器说明及位置，省去源码摘录。
pub(crate) fn profile_error_message(error: &lattice_profile::ProfileError) -> String {
    match error {
        lattice_profile::ProfileError::Toml { path, source, line } => {
            let location = line
                .map(|line| format!("line {line}: "))
                .unwrap_or_default();
            format!(
                "TOML parse error at {}: {location}{}",
                path.display(),
                source.message()
            )
        }
        other => other.to_string(),
    }
}
