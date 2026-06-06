// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Public profile loading API.

use std::path::Path;

use crate::session::{ApiError, Result};

pub use lattice_profile::LoadedAgentProfile;

/// Load an agent profile from an explicit TOML path.
pub fn load_agent_profile_path(path: &Path) -> Result<LoadedAgentProfile> {
    lattice_profile::load_agent_profile(path).map_err(|err| ApiError::BadRequest(err.to_string()))
}

/// Resolve a named agent profile from project/global discovery roots.
pub fn resolve_named_agent_profile(
    agent_name: &str,
    project_root: &Path,
    global_agents_dir: &Path,
) -> Result<LoadedAgentProfile> {
    let paths = lattice_profile::DiscoveryPaths {
        project_root: project_root.to_path_buf(),
        global_agents_dir: global_agents_dir.to_path_buf(),
    };
    lattice_profile::resolve_agent_profile(
        lattice_profile::ProfileSource::Name(agent_name.to_string()),
        &paths,
    )
    .map_err(|err| ApiError::BadRequest(err.to_string()))
}
