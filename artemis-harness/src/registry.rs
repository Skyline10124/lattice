use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::profile::AgentProfile;

// ---------------------------------------------------------------------------
// AgentRegistry — loads and indexes agent profiles from a directory
// ---------------------------------------------------------------------------

pub struct AgentRegistry {
    agents: HashMap<String, AgentProfile>,
    #[allow(dead_code)]
    base_dir: PathBuf,
}

impl AgentRegistry {
    /// Load all TOML agent profiles from ~/.artemis/agents/ or a custom path.
    pub fn load_dir(dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut registry = Self {
            agents: HashMap::new(),
            base_dir: dir.to_path_buf(),
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
                            eprintln!(
                                "Warning: failed to load agent at {}: {}",
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
}
