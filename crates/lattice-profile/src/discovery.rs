// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Agent profile discovery — explicit path, named lookup, and default resolution.
//!
//! Preserves the `.lattice/agents/<name>/agent.toml` and
//! `~/.lattice/agents/<name>/agent.toml` package semantics.
//! Lookup order: explicit path > project-local > user-global.

use std::path::PathBuf;

use lattice_session::AgentName;

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
    let parsed =
        AgentName::parse(name).map_err(|_| ProfileError::InvalidAgentName { name: name.into() })?;
    let project_path = paths
        .project_agents_dir()
        .join(parsed.as_str())
        .join("agent.toml");
    let global_path = paths
        .global_agents_dir
        .join(parsed.as_str())
        .join("agent.toml");

    // Project-local wins over user-global.
    if project_path.exists() {
        return load_agent_profile(&project_path);
    }

    if global_path.exists() {
        return load_agent_profile(&global_path);
    }

    // Not found anywhere.
    Err(ProfileError::ProfileNotFound {
        name: parsed.as_str().to_string(),
        project_path,
        global_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::DirBuilderExt;

    #[test]
    fn named_lookup_does_not_escape_agents_dir() {
        let dir = tempfile::tempdir().unwrap();
        let configuration_dir = dir.path().join("home/.lattice");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&configuration_dir)
            .unwrap();
        let paths = DiscoveryPaths {
            project_root: dir.path().join("project"),
            global_agents_dir: configuration_dir.join("agents"),
        };
        let outside = dir.path().join("outside");
        for name in ["../outside", outside.to_str().unwrap()] {
            let target = paths.project_agents_dir().join(name).join("agent.toml");
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(
                target,
                format!(
                    "schema_version=1\n[agent]\nname='{name}'\nmodel='local/test'\n[prompt]\nsystem='valid target'\n"
                ),
            )
            .unwrap();
            assert!(
                matches!(
                    resolve_agent_profile(ProfileSource::Name(name.into()), &paths),
                    Err(ProfileError::InvalidAgentName { name: found }) if found == name
                ),
                "{name:?}"
            );
        }
    }
}
