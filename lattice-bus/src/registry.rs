use std::collections::HashMap;
use std::path::Path;

use crate::profile::AgentProfile;

// ---------------------------------------------------------------------------
// AgentRegistry — loads and indexes agent profiles from a directory
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AgentRegistry {
    agents: HashMap<String, AgentProfile>,
}

impl AgentRegistry {
    /// Load all TOML agent profiles from ~/.lattice/agents/ or a custom path.
    pub fn load_dir(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut registry = Self {
            agents: HashMap::new(),
        };

        if !dir.exists() {
            return Ok(registry);
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let agent_toml = entry.path().join("agent.toml");
                if agent_toml.exists() {
                    match AgentProfile::load(&agent_toml) {
                        Ok(profile) => {
                            let name = profile.agent.name.clone();
                            registry.agents.insert(name, profile);
                        }
                        Err(e) => {
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

    /// Merge another registry into this one. Other's agents override on name collision.
    pub fn merge(mut self, other: Self) -> Self {
        for (name, profile) in other.agents {
            self.agents.insert(name, profile);
        }
        self
    }
}
