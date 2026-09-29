// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Resolved operator policy configuration consumed by the Component Tool Host.

use std::path::{Path, PathBuf};

use super::network::NetworkPolicyConfig;
use super::observation::ObservationPolicyConfig;

/// Immutable workspace write-deny and read-visibility policy.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WorkspacePolicy {
    pub denied_write_patterns: Vec<String>,
    pub included_ignored_read_patterns: Vec<String>,
    pub denied_read_patterns: Vec<String>,
}

impl WorkspacePolicy {
    /// Validate every project-root-relative path pattern.
    pub fn validate(&self) -> Result<(), String> {
        use crate::guard::normalize_write_path_pattern;

        for (field, patterns) in [
            ("denied_write_patterns", &self.denied_write_patterns),
            (
                "included_ignored_read_patterns",
                &self.included_ignored_read_patterns,
            ),
            ("denied_read_patterns", &self.denied_read_patterns),
        ] {
            for pattern in patterns {
                if pattern.is_empty() {
                    return Err(format!("{field} entry must not be empty"));
                }
                if pattern.contains('\0') {
                    return Err(format!("{field} entry must not contain NUL bytes"));
                }
                if pattern.contains("..") {
                    return Err(format!("{field} entry must not contain '..' traversal"));
                }
                if Path::new(pattern).is_absolute() {
                    return Err(format!("{field} entry must be project-root-relative"));
                }
                normalize_write_path_pattern(pattern).map_err(|error| error.message)?;
            }
        }
        Ok(())
    }

    pub fn read_visibility_policy(
        &self,
    ) -> Result<super::visibility::ReadVisibilityPolicy, String> {
        super::visibility::ReadVisibilityPolicy::from_patterns(
            &self.included_ignored_read_patterns,
            &self.denied_read_patterns,
        )
    }
}

/// Fully resolved, turn-scoped operator policy.
#[derive(Clone, Debug)]
pub struct PolicyConfig {
    pub network: NetworkPolicyConfig,
    pub observation: ObservationPolicyConfig,
    pub workspace: WorkspacePolicy,
    pub project_root: PathBuf,
}

impl PolicyConfig {
    pub fn defaults_for_project_root(project_root: PathBuf) -> Self {
        Self {
            network: NetworkPolicyConfig::safe_defaults(),
            observation: ObservationPolicyConfig::defaults(),
            workspace: WorkspacePolicy::default(),
            project_root,
        }
    }

    /// Merge one strict external policy file.
    pub fn merge_external(&mut self, external: &ExternalPolicyFile) -> Result<(), String> {
        if let Some(network) = &external.network {
            if let Some(value) = network.allow_public_https {
                self.network.allow_public_https = value;
            }
            if let Some(value) = network.allow_localhost {
                self.network.allow_localhost = value;
            }
            if let Some(value) = &network.allowed_hosts {
                self.network.allowed_hosts = value.clone();
            }
            if let Some(value) = network.max_web_response_bytes {
                self.network.max_web_response_bytes = value;
            }
        }
        // The deprecated `[budget]` spelling remains an alias of the one live
        // observation limit set; `[observation]` wins on conflicts.
        if let Some(budget) = &external.budget {
            apply_external_limits(&mut self.observation, budget);
        }
        if let Some(workspace) = &external.workspace {
            if let Some(patterns) = &workspace.denied_write_patterns {
                self.workspace.denied_write_patterns = patterns.clone();
            }
            if let Some(patterns) = &workspace.included_ignored_read_patterns {
                self.workspace.included_ignored_read_patterns = patterns.clone();
            }
            if let Some(patterns) = &workspace.denied_read_patterns {
                self.workspace.denied_read_patterns = patterns.clone();
            }
            self.workspace.validate()?;
        }
        if let Some(observation) = &external.observation {
            apply_external_limits(&mut self.observation, observation);
        }
        Ok(())
    }
}

/// Strict external policy file. Removed execution/effect tables are rejected.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalPolicyFile {
    pub network: Option<ExternalNetworkPolicy>,
    pub budget: Option<ExternalObservationPolicy>,
    pub observation: Option<ExternalObservationPolicy>,
    pub workspace: Option<ExternalWorkspacePolicy>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalWorkspacePolicy {
    pub denied_write_patterns: Option<Vec<String>>,
    pub included_ignored_read_patterns: Option<Vec<String>>,
    pub denied_read_patterns: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalNetworkPolicy {
    pub allow_public_https: Option<bool>,
    pub allow_localhost: Option<bool>,
    pub allowed_hosts: Option<Vec<String>>,
    pub max_web_response_bytes: Option<usize>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalObservationPolicy {
    pub max_single_read_bytes: Option<usize>,
    pub max_turn_observation_bytes: Option<usize>,
    pub max_grep_matches: Option<usize>,
    pub max_list_entries: Option<usize>,
    pub max_find_paths: Option<usize>,
}

fn apply_external_limits(
    observation: &mut ObservationPolicyConfig,
    external: &ExternalObservationPolicy,
) {
    if let Some(value) = external.max_single_read_bytes {
        observation.max_single_read_bytes = value;
    }
    if let Some(value) = external.max_turn_observation_bytes {
        observation.max_turn_observation_bytes = value;
    }
    if let Some(value) = external.max_grep_matches {
        observation.max_grep_matches = value;
    }
    if let Some(value) = external.max_list_entries {
        observation.max_list_entries = value;
    }
    if let Some(value) = external.max_find_paths {
        observation.max_find_paths = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_patterns_stay_strict() {
        let policy = WorkspacePolicy {
            denied_write_patterns: vec!["../outside".into()],
            ..WorkspacePolicy::default()
        };
        assert!(policy.validate().is_err());
    }
}
