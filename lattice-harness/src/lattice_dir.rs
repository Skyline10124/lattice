use std::path::{Path, PathBuf};

use crate::registry::AgentRegistry;
use lattice_bus::BusConfig;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ---------------------------------------------------------------------------
// BusToml — project-level bus configuration from .lattice/bus.toml
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BusToml {
    #[serde(default = "default_timeout_rpc")]
    pub timeout_rpc_secs: u64,
    #[serde(default = "default_delivery_policy")]
    pub delivery_policy: String,
    #[serde(default = "default_subscriber_buffer")]
    pub subscriber_buffer: usize,
    #[serde(default = "default_max_concurrent_calls")]
    pub max_concurrent_calls: usize,
}

fn default_timeout_rpc() -> u64 {
    30
}
fn default_delivery_policy() -> String {
    "at_most_once".into()
}
fn default_subscriber_buffer() -> usize {
    1024
}
fn default_max_concurrent_calls() -> usize {
    1
}

impl Default for BusToml {
    fn default() -> Self {
        Self {
            timeout_rpc_secs: default_timeout_rpc(),
            delivery_policy: default_delivery_policy(),
            subscriber_buffer: default_subscriber_buffer(),
            max_concurrent_calls: default_max_concurrent_calls(),
        }
    }
}

impl BusToml {
    /// Load bus.toml from a path.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    /// Convert BusToml into BusConfig for the Bus.
    pub fn to_bus_config(&self) -> BusConfig {
        BusConfig {
            timeout_rpc: Duration::from_secs(self.timeout_rpc_secs),
            delivery_policy: match self.delivery_policy.as_str() {
                "at_least_once" => lattice_bus::DeliveryPolicy::AtLeastOnce,
                "at_most_once" => lattice_bus::DeliveryPolicy::AtMostOnce,
                other => {
                    tracing::warn!(
                        "Unknown delivery_policy '{}', falling back to at_most_once",
                        other
                    );
                    lattice_bus::DeliveryPolicy::AtMostOnce
                }
            },
            subscriber_buffer: self.subscriber_buffer,
            max_concurrent_calls: self.max_concurrent_calls,
        }
    }
}

// ---------------------------------------------------------------------------
// LatticeDir — discover and scan .lattice/ project directory
// ---------------------------------------------------------------------------

pub struct LatticeDir {
    pub root: PathBuf,
    pub bus_config: BusToml,
    pub registry: AgentRegistry,
}

impl LatticeDir {
    /// Discover .lattice/ in the current working directory or a given root.
    /// Scans .lattice/agents/ for profiles and loads bus.toml.
    pub fn discover(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let lattice_dir = root.join(".lattice");
        if !lattice_dir.exists() {
            let msg = format!(".lattice/ directory not found at {}", lattice_dir.display());
            return Err(msg.into());
        }

        let bus_toml_path = lattice_dir.join("bus.toml");
        let bus_config = if bus_toml_path.exists() {
            BusToml::load(&bus_toml_path)?
        } else {
            BusToml::default()
        };

        let agents_dir = lattice_dir.join("agents");
        let registry = AgentRegistry::load_dir(&agents_dir)?;

        Ok(Self {
            root: lattice_dir,
            bus_config,
            registry,
        })
    }

    /// Combine project-local agents with global ~/.lattice/agents/.
    /// Project-local agents take precedence (same name overrides global).
    pub fn discover_with_global(
        project_root: &Path,
        global_dir: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let lattice_dir = project_root.join(".lattice");
        if !lattice_dir.exists() {
            // Fall back to global-only
            let registry = AgentRegistry::load_dir(global_dir)?;
            let bus_config = BusToml::default();
            return Ok(Self {
                root: global_dir.to_path_buf(),
                bus_config,
                registry,
            });
        }

        let bus_toml_path = lattice_dir.join("bus.toml");
        let bus_config = if bus_toml_path.exists() {
            BusToml::load(&bus_toml_path)?
        } else {
            BusToml::default()
        };

        // Load global first, then overlay project-local (project wins on name collision)
        let global_registry = AgentRegistry::load_dir(global_dir)?;
        let project_agents_dir = lattice_dir.join("agents");
        let project_registry = AgentRegistry::load_dir(&project_agents_dir)?;

        // Merge: project-local overrides global
        let merged = global_registry.merge(project_registry);

        Ok(Self {
            root: lattice_dir,
            bus_config,
            registry: merged,
        })
    }

    /// Path to the shared memory database (.lattice/memory/shared.db).
    pub fn shared_db_path(&self) -> PathBuf {
        self.root.join("memory").join("shared.db")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_bus_toml_defaults() {
        let config = BusToml::default();
        assert_eq!(config.timeout_rpc_secs, 30);
        assert_eq!(config.delivery_policy, "at_most_once");
        assert_eq!(config.subscriber_buffer, 1024);
        assert_eq!(config.max_concurrent_calls, 1);
    }

    #[test]
    fn test_bus_toml_custom_values() {
        let toml_str = r#"
        timeout_rpc_secs = 60
        delivery_policy = "at_least_once"
        subscriber_buffer = 2048
        max_concurrent_calls = 4
        "#;
        let config: BusToml = toml::from_str(toml_str).unwrap();
        assert_eq!(config.timeout_rpc_secs, 60);
        assert_eq!(config.delivery_policy, "at_least_once");
        assert_eq!(config.subscriber_buffer, 2048);
        assert_eq!(config.max_concurrent_calls, 4);
    }

    #[test]
    fn test_bus_toml_to_bus_config() {
        let config = BusToml::default();
        let bus_config = config.to_bus_config();
        assert_eq!(bus_config.timeout_rpc, Duration::from_secs(30));
        assert_eq!(
            bus_config.delivery_policy,
            lattice_bus::DeliveryPolicy::AtMostOnce
        );
        assert_eq!(bus_config.subscriber_buffer, 1024);
    }

    #[test]
    fn test_lattice_dir_discover_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let result = LatticeDir::discover(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_lattice_dir_discover_with_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let lattice = tmp.path().join(".lattice");
        let agents = lattice.join("agents").join("test-agent");
        fs::create_dir_all(&agents).unwrap();

        let agent_toml = r#"
        [agent]
        name = "test-agent"
        model = "sonnet"

        [system]
        prompt = "Test"
        "#;
        fs::write(agents.join("agent.toml"), agent_toml).unwrap();

        let ld = LatticeDir::discover(tmp.path()).unwrap();
        assert!(ld.registry.get("test-agent").is_some());
        assert_eq!(ld.bus_config.timeout_rpc_secs, 30);
    }

    #[test]
    fn test_lattice_dir_discover_with_bus_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let lattice = tmp.path().join(".lattice");
        let agents = lattice.join("agents");
        fs::create_dir_all(&agents).unwrap();

        let bus_toml = r#"
        timeout_rpc_secs = 60
        max_concurrent_calls = 8
        "#;
        fs::write(lattice.join("bus.toml"), bus_toml).unwrap();

        let ld = LatticeDir::discover(tmp.path()).unwrap();
        assert_eq!(ld.bus_config.timeout_rpc_secs, 60);
        assert_eq!(ld.bus_config.max_concurrent_calls, 8);
        assert_eq!(
            ld.shared_db_path(),
            lattice.join("memory").join("shared.db")
        );
    }

    #[test]
    fn test_bus_toml_load_invalid_syntax() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("bus.toml"), "this is not valid toml {{{").unwrap();
        let result = BusToml::load(&tmp.path().join("bus.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_bus_toml_to_bus_config_at_least_once() {
        let config = BusToml {
            timeout_rpc_secs: 30,
            delivery_policy: "at_least_once".into(),
            subscriber_buffer: 1024,
            max_concurrent_calls: 1,
        };
        let bus_config = config.to_bus_config();
        assert_eq!(
            bus_config.delivery_policy,
            lattice_bus::DeliveryPolicy::AtLeastOnce
        );
    }

    #[test]
    fn test_bus_toml_to_bus_config_unknown_policy() {
        let config = BusToml {
            timeout_rpc_secs: 30,
            delivery_policy: "invalid_policy".into(),
            subscriber_buffer: 1024,
            max_concurrent_calls: 1,
        };
        let bus_config = config.to_bus_config();
        assert_eq!(
            bus_config.delivery_policy,
            lattice_bus::DeliveryPolicy::AtMostOnce
        );
    }
}
