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
                let template = crate::defaults::default_agent_template("default", "sonnet");
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn make_discovery_paths() -> (tempfile::TempDir, tempfile::TempDir, DiscoveryPaths) {
        let project = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        let paths = DiscoveryPaths {
            project_root: project.path().to_path_buf(),
            global_agents_dir: global.path().to_path_buf(),
        };
        (project, global, paths)
    }

    fn write_project_profile(project_root: &Path, name: &str, model: &str) {
        let agent_dir = project_root.join(".lattice").join("agents").join(name);
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_agent_toml(&agent_dir, name, model);
    }

    fn write_global_profile(global_agents_dir: &Path, name: &str, model: &str) {
        let agent_dir = global_agents_dir.join(name);
        std::fs::create_dir_all(&agent_dir).unwrap();
        write_agent_toml(&agent_dir, name, model);
    }

    fn write_agent_toml(dir: &Path, name: &str, model: &str) {
        let toml = format!(
            r#"
schema_version = 1

[agent]
name = "{name}"
model = "{model}"

[prompt]
system = "hello"
"#
        );
        std::fs::write(dir.join("agent.toml"), toml).unwrap();
    }

    #[test]
    fn explicit_path_wins_over_named_and_default() {
        let (project, global, paths) = make_discovery_paths();
        write_project_profile(project.path(), "foo", "project-model");
        write_global_profile(global.path(), "foo", "global-model");

        let explicit_path = project
            .path()
            .join(".lattice")
            .join("agents")
            .join("foo")
            .join("agent.toml");
        let loaded = resolve_agent_profile(ProfileSource::Path(explicit_path), &paths).unwrap();
        assert_eq!(loaded.profile.agent.model, "project-model");
    }

    #[test]
    fn named_lookup_prefers_project_local() {
        let (project, global, paths) = make_discovery_paths();
        write_project_profile(project.path(), "helper", "project-model");
        write_global_profile(global.path(), "helper", "global-model");

        let loaded = resolve_agent_profile(ProfileSource::Name("helper".into()), &paths).unwrap();
        assert_eq!(loaded.profile.agent.model, "project-model");
    }

    #[test]
    fn named_lookup_falls_back_to_global() {
        let (_project, global, paths) = make_discovery_paths();
        write_global_profile(global.path(), "helper", "global-model");

        let loaded = resolve_agent_profile(ProfileSource::Name("helper".into()), &paths).unwrap();
        assert_eq!(loaded.profile.agent.model, "global-model");
    }

    #[test]
    fn default_checks_project_first_then_global() {
        let (project, global, paths) = make_discovery_paths();
        write_project_profile(project.path(), "default", "project-default");

        let loaded = resolve_agent_profile(ProfileSource::Default, &paths).unwrap();
        assert_eq!(loaded.profile.agent.model, "project-default");

        // Delete project default; should fall back to global.
        let project_default_dir = project.path().join(".lattice/agents/default");
        std::fs::remove_dir_all(&project_default_dir).unwrap();

        write_global_profile(global.path(), "default", "global-default");
        let loaded = resolve_agent_profile(ProfileSource::Default, &paths).unwrap();
        assert_eq!(loaded.profile.agent.model, "global-default");
    }

    #[test]
    fn missing_default_returns_typed_error_with_template() {
        let (_project, _global, paths) = make_discovery_paths();
        let err = resolve_agent_profile(ProfileSource::Default, &paths).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no default agent profile"), "got: {err}");
        assert!(
            msg.contains("lattice new agent default"),
            "must include creation hint"
        );
    }

    #[test]
    fn missing_default_creates_no_files() {
        let (_project, _global, paths) = make_discovery_paths();
        let agents_dir = paths.project_agents_dir();

        let _ = resolve_agent_profile(ProfileSource::Default, &paths);

        assert!(
            !agents_dir.exists(),
            "discovery must not create files or directories"
        );
    }

    #[test]
    fn named_missing_returns_profile_not_found_error() {
        let (_project, _global, paths) = make_discovery_paths();
        let err =
            resolve_agent_profile(ProfileSource::Name("nonexistent".into()), &paths).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent"),
            "expected name in error, got: {err}"
        );
    }

    #[test]
    fn nonexistent_agent_dir_is_handled_as_missing() {
        let (_project, _global, paths) = make_discovery_paths();
        let result = resolve_agent_profile(ProfileSource::Name("nope".into()), &paths);
        assert!(result.is_err());
    }
}
