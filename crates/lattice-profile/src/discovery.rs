// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Agent profile 发现路径与内部查找顺序。
//!
//! 保持 `.lattice/agents/<name>/agent.toml` 与
//! `~/.lattice/agents/<name>/agent.toml` 的包布局，项目内优先于用户全局。

use std::path::{Path, PathBuf};

use lattice_session::AgentName;

use crate::error::ProfileError;

/// Filesystem paths that drive discovery.
#[derive(Debug, Clone)]
pub struct DiscoveryPaths {
    /// Project root (for `.lattice/agents/<name>/agent.toml`).
    pub project_root: PathBuf,
    /// User-global agents directory (`~/.lattice/agents`).
    pub global_agents_dir: PathBuf,
}

impl DiscoveryPaths {
    pub fn project_agents_dir(&self) -> PathBuf {
        self.project_root.join(".lattice").join("agents")
    }
}

pub(crate) struct DiscoveredProfile {
    pub path: PathBuf,
    pub package_dir: PathBuf,
    pub is_global: bool,
}

pub(crate) fn discover_profile_path(
    name: &AgentName,
    paths: &DiscoveryPaths,
) -> Result<DiscoveredProfile, ProfileError> {
    let project_agents_dir = paths.project_agents_dir();
    let project_path = project_agents_dir.join(name.as_str()).join("agent.toml");
    let global_path = paths
        .global_agents_dir
        .join(name.as_str())
        .join("agent.toml");

    // 项目内 profile 优先于用户全局目录。
    let path = if project_path.exists() {
        project_path
    } else if global_path.exists() {
        global_path
    } else {
        return Err(ProfileError::ProfileNotFound {
            name: name.as_str().to_owned(),
            project_path,
            global_path,
        });
    };

    // 测试里配置目录可能就放在项目的 .lattice 下；只约束用户全局目录，不把仓库里的 agent.toml 当成全局配置。
    let is_global =
        path.starts_with(&paths.global_agents_dir) && !path.starts_with(&project_agents_dir);
    let package_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    Ok(DiscoveredProfile {
        path,
        package_dir,
        is_global,
    })
}
