use std::path::{Path, PathBuf};

use crate::registry::AgentRegistry;
use crate::{DeliveryPolicy, OrchestratorConfig};
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ---------------------------------------------------------------------------
// OrchestratorToml — project-level orchestrator configuration from .lattice/orchestrator.toml
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OrchestratorToml {
    pub timeout_rpc_secs: u64,
    pub delivery_policy: String,
    pub subscriber_buffer: usize,
    #[serde(alias = "max_concurrent_calls")]
    pub max_concurrent_rpc: usize,
    pub channel_buffer_size: usize,
}

impl Default for OrchestratorToml {
    fn default() -> Self {
        Self {
            timeout_rpc_secs: 30,
            delivery_policy: "at_most_once".into(),
            subscriber_buffer: 1024,
            max_concurrent_rpc: 32,
            channel_buffer_size: 1024,
        }
    }
}

impl OrchestratorToml {
    /// Load orchestrator.toml from a path.
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let content = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&content)?;
        Ok(config)
    }

    /// Convert OrchestratorToml into OrchestratorConfig for the Orchestrator.
    pub fn to_orchestrator_config(&self) -> OrchestratorConfig {
        OrchestratorConfig {
            timeout_rpc: Duration::from_secs(self.timeout_rpc_secs),
            delivery_policy: match self.delivery_policy.as_str() {
                "at_least_once" => DeliveryPolicy::AtLeastOnce,
                "at_most_once" => DeliveryPolicy::AtMostOnce,
                other => {
                    tracing::warn!(
                        "Unknown delivery_policy '{}', falling back to at_most_once",
                        other
                    );
                    DeliveryPolicy::AtMostOnce
                }
            },
            subscriber_buffer: self.subscriber_buffer,
            max_concurrent_rpc: self.max_concurrent_rpc,
            channel_buffer_size: self.channel_buffer_size,
        }
    }
}

fn load_orchestrator_config(
    lattice_dir: &Path,
) -> Result<OrchestratorToml, Box<dyn std::error::Error>> {
    let path = lattice_dir.join("orchestrator.toml");
    if path.exists() {
        OrchestratorToml::load(&path)
    } else {
        Ok(OrchestratorToml::default())
    }
}

// ---------------------------------------------------------------------------
// LatticeDir — discover and scan .lattice/ project directory
// ---------------------------------------------------------------------------

pub struct LatticeDir {
    pub root: PathBuf,
    pub orchestrator_config: OrchestratorToml,
    pub registry: AgentRegistry,
}

impl LatticeDir {
    /// Discover .lattice/ in the current working directory or a given root.
    /// Scans .lattice/agents/ for profiles and loads orchestrator.toml.
    pub fn discover(root: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let lattice_dir = root.join(".lattice");
        if !lattice_dir.exists() {
            let msg = format!(".lattice/ directory not found at {}", lattice_dir.display());
            return Err(msg.into());
        }

        let orchestrator_config = load_orchestrator_config(&lattice_dir)?;

        let agents_dir = lattice_dir.join("agents");
        let registry = AgentRegistry::load_dir(&agents_dir)?;

        Ok(Self {
            root: lattice_dir,
            orchestrator_config,
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
            let orchestrator_config = OrchestratorToml::default();
            return Ok(Self {
                root: global_dir.to_path_buf(),
                orchestrator_config,
                registry,
            });
        }

        let orchestrator_config = load_orchestrator_config(&lattice_dir)?;

        // Load global first, then overlay project-local (project wins on name collision)
        let global_registry = AgentRegistry::load_dir(global_dir)?;
        let project_agents_dir = lattice_dir.join("agents");
        let project_registry = AgentRegistry::load_dir(&project_agents_dir)?;

        // Merge: project-local overrides global
        let merged = global_registry.merge(project_registry);

        Ok(Self {
            root: lattice_dir,
            orchestrator_config,
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
    fn test_orchestrator_toml_defaults() {
        let config = OrchestratorToml::default();
        assert_eq!(config.timeout_rpc_secs, 30);
        assert_eq!(config.delivery_policy, "at_most_once");
        assert_eq!(config.subscriber_buffer, 1024);
        assert_eq!(config.max_concurrent_rpc, 32);
        assert_eq!(config.channel_buffer_size, 1024);
    }

    #[test]
    fn test_orchestrator_toml_custom_values() {
        let toml_str = r#"
        timeout_rpc_secs = 60
        delivery_policy = "at_least_once"
        subscriber_buffer = 2048
        max_concurrent_rpc = 4
        channel_buffer_size = 64
        "#;
        let config: OrchestratorToml = toml::from_str(toml_str).unwrap();
        assert_eq!(config.timeout_rpc_secs, 60);
        assert_eq!(config.delivery_policy, "at_least_once");
        assert_eq!(config.subscriber_buffer, 2048);
        assert_eq!(config.max_concurrent_rpc, 4);
        assert_eq!(config.channel_buffer_size, 64);
    }

    #[test]
    fn test_orchestrator_toml_to_orchestrator_config() {
        let config = OrchestratorToml::default();
        let orchestrator_config = config.to_orchestrator_config();
        assert_eq!(orchestrator_config.timeout_rpc, Duration::from_secs(30));
        assert_eq!(
            orchestrator_config.delivery_policy,
            DeliveryPolicy::AtMostOnce
        );
        assert_eq!(orchestrator_config.subscriber_buffer, 1024);
        assert_eq!(orchestrator_config.max_concurrent_rpc, 32);
        assert_eq!(orchestrator_config.channel_buffer_size, 1024);
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
        assert_eq!(ld.orchestrator_config.timeout_rpc_secs, 30);
    }

    #[test]
    fn test_lattice_dir_discover_with_orchestrator_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let lattice = tmp.path().join(".lattice");
        let agents = lattice.join("agents");
        fs::create_dir_all(&agents).unwrap();

        let orchestrator_toml_content = r#"
        timeout_rpc_secs = 60
        max_concurrent_calls = 8
        "#;
        fs::write(lattice.join("orchestrator.toml"), orchestrator_toml_content).unwrap();

        let ld = LatticeDir::discover(tmp.path()).unwrap();
        assert_eq!(ld.orchestrator_config.timeout_rpc_secs, 60);
        assert_eq!(ld.orchestrator_config.max_concurrent_rpc, 8);
        assert_eq!(
            ld.shared_db_path(),
            lattice.join("memory").join("shared.db")
        );
    }

    #[test]
    fn test_orchestrator_toml_load_invalid_syntax() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("orchestrator.toml"),
            "this is not valid toml {{{",
        )
        .unwrap();
        let result = OrchestratorToml::load(&tmp.path().join("orchestrator.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_orchestrator_toml_to_orchestrator_config_at_least_once() {
        let config = OrchestratorToml {
            timeout_rpc_secs: 30,
            delivery_policy: "at_least_once".into(),
            subscriber_buffer: 1024,
            max_concurrent_rpc: 32,
            channel_buffer_size: 1024,
        };
        let orchestrator_config = config.to_orchestrator_config();
        assert_eq!(
            orchestrator_config.delivery_policy,
            DeliveryPolicy::AtLeastOnce
        );
    }

    #[test]
    fn test_orchestrator_toml_to_orchestrator_config_unknown_policy() {
        let config = OrchestratorToml {
            timeout_rpc_secs: 30,
            delivery_policy: "invalid_policy".into(),
            subscriber_buffer: 1024,
            max_concurrent_rpc: 32,
            channel_buffer_size: 1024,
        };
        let orchestrator_config = config.to_orchestrator_config();
        assert_eq!(
            orchestrator_config.delivery_policy,
            DeliveryPolicy::AtMostOnce
        );
    }
}
