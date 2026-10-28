// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Agent profile discovery — explicit path, named lookup, and default resolution.
//!
//! Preserves the `.lattice/agents/<name>/agent.toml` and
//! `~/.lattice/agents/<name>/agent.toml` package semantics.
//! Lookup order: explicit path > project-local > user-global.

use std::path::PathBuf;

use crate::error::ProfileError;
use crate::load::{LoadedAgentProfile, load_agent_profile};

/// How the caller identifies which profile to load.
#[derive(Debug, Clone)]
pub enum ProfileSource {
    /// An exact filesystem path to an `agent.toml`.
    Path(PathBuf),
    /// A named agent looked up in project-local then user-global directories.
    Name(String),
    /// Use the `default` agent; checked project-local then user-global.
    Default,
}

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

/// Resolve a `ProfileSource` to a loaded and validated profile.
///
/// Explicit paths are loaded directly. Named lookups check project-local before
/// user-global. The `Default` source uses name `"default"`. Missing default
/// returns a typed error with template text and proposed create path; no files
/// are written.
pub fn resolve_agent_profile(
    source: ProfileSource,
    paths: &DiscoveryPaths,
) -> Result<LoadedAgentProfile, ProfileError> {
    match source {
        ProfileSource::Path(p) => load_agent_profile(&p),
        ProfileSource::Name(name) => resolve_named(&name, paths),
        ProfileSource::Default => match resolve_named("default", paths) {
            Ok(loaded) => Ok(loaded),
            Err(ProfileError::ProfileNotFound { .. }) => {
                let template = crate::defaults::default_agent_template(
                    "default",
                    "anthropic/claude-sonnet-4-6",
                );
                Err(ProfileError::MissingDefault {
                    dir: paths.project_agents_dir().join("default"),
                    template,
                })
            }
            Err(other) => Err(other),
        },
    }
}

fn resolve_named(name: &str, paths: &DiscoveryPaths) -> Result<LoadedAgentProfile, ProfileError> {
    let project_path = paths.project_agents_dir().join(name).join("agent.toml");
    let global_path = paths.global_agents_dir.join(name).join("agent.toml");

    // Project-local wins over user-global.
    if project_path.exists() {
        return load_agent_profile(&project_path);
    }

    if global_path.exists() {
        return load_agent_profile(&global_path);
    }

    // Not found anywhere.
    Err(ProfileError::ProfileNotFound {
        name: name.to_string(),
        project_path,
        global_path,
    })
}
