use std::collections::HashMap;
use std::path::Path;

use crate::profile::AgentProfile;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedLoad {
    pub path: std::path::PathBuf,
    pub error: String,
}

// ---------------------------------------------------------------------------
// AgentRegistry — loads and indexes agent profiles from a directory
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AgentRegistry {
    agents: HashMap<String, AgentProfile>,
    failed_loads: Vec<FailedLoad>,
}

impl AgentRegistry {
    pub fn empty() -> Self {
        Self {
            agents: HashMap::new(),
            failed_loads: Vec::new(),
        }
    }

    pub fn from_profiles(profiles: impl IntoIterator<Item = AgentProfile>) -> Self {
        let mut registry = Self::empty();
        for profile in profiles {
            registry.agents.insert(profile.agent.name.clone(), profile);
        }
        registry
    }

    /// Load all TOML agent profiles from ~/.lattice/agents/ or a custom path.
    pub fn load_dir(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut registry = Self::empty();

        if !dir.exists() {
            return Ok(registry);
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let agent_toml = entry.path().join("agent.toml");
                if agent_toml.exists() {
                    match crate::profile::load_profile_verified(&agent_toml) {
                        Ok((profile, _hash)) => {
                            let name = profile.agent.name.clone();
                            registry.agents.insert(name, profile);
                        }
                        Err(e) => {
                            registry.failed_loads.push(FailedLoad {
                                path: agent_toml.clone(),
                                error: e.to_string(),
                            });
                            tracing::warn!(
                                "failed to load agent at {}: {}",
                                agent_toml.display(),
                                e
                            );
                        }
                    }
                }
            }
        }

        Ok(registry)
    }

    pub fn get(&self, name: &str) -> Option<&AgentProfile> {
        self.agents.get(name)
    }

    pub fn list(&self) -> Vec<&AgentProfile> {
        self.agents.values().collect()
    }

    pub fn failed_loads(&self) -> &[FailedLoad] {
        &self.failed_loads
    }

    /// Merge another registry into this one. Other's agents override on name collision.
    pub fn merge(mut self, other: Self) -> Self {
        for (name, profile) in other.agents {
            self.agents.insert(name, profile);
        }
        self.failed_loads.extend(other.failed_loads);
        self
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::empty()
    }
}
