use std::path::Path;

use lattice_orchestrator::AgentProfile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentProfileKind {
    Legacy,
    V1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSummary {
    pub name: String,
    pub model: String,
    pub kind: AgentProfileKind,
    pub has_plugins: bool,
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLoadFailure {
    pub path: std::path::PathBuf,
    pub error: String,
}

#[derive(Clone)]
pub struct AgentInventory {
    legacy_profiles: Vec<AgentProfile>,
    v1_profiles: Vec<lattice_profile::LoadedAgentProfile>,
    failures: Vec<AgentLoadFailure>,
}

impl AgentInventory {
    pub fn profile_count(&self) -> usize {
        self.legacy_profiles.len() + self.v1_profiles.len()
    }

    pub fn legacy_registry(&self) -> lattice_orchestrator::AgentRegistry {
        lattice_orchestrator::AgentRegistry::from_profiles(self.legacy_profiles.clone())
    }

    pub fn v1_profiles(&self) -> &[lattice_profile::LoadedAgentProfile] {
        &self.v1_profiles
    }

    pub fn failures(&self) -> &[AgentLoadFailure] {
        &self.failures
    }

    pub fn summaries(&self) -> Vec<AgentSummary> {
        let mut summaries = Vec::new();

        for profile in &self.legacy_profiles {
            summaries.push(AgentSummary {
                name: profile.agent.name.clone(),
                model: profile.agent.model.clone(),
                kind: AgentProfileKind::Legacy,
                has_plugins: profile.plugins.is_some(),
                tags: profile.agent.tags.clone(),
            });
        }

        for loaded in &self.v1_profiles {
            summaries.push(AgentSummary {
                name: loaded.profile.agent.name.clone(),
                model: loaded.profile.agent.model.clone(),
                kind: AgentProfileKind::V1,
                has_plugins: false,
                tags: Vec::new(),
            });
        }

        summaries
    }
}

pub fn load_agent_inventory(dir: &Path) -> Result<AgentInventory, Box<dyn std::error::Error>> {
    let mut legacy_profiles: Vec<AgentProfile> = Vec::new();
    let mut v1_profiles = Vec::new();
    let mut failures = Vec::new();

    if !dir.exists() {
        return Ok(AgentInventory {
            legacy_profiles,
            v1_profiles,
            failures,
        });
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }

        let agent_toml = entry.path().join("agent.toml");
        if !agent_toml.exists() {
            continue;
        }

        match lattice_profile::load_agent_profile(&agent_toml) {
            Ok(profile) => v1_profiles.push(profile),
            Err(v1_error) => {
                match lattice_orchestrator::profile::load_profile_verified_read_only(&agent_toml) {
                    Ok((profile, _hash)) => legacy_profiles.push(profile),
                    Err(legacy_error) => failures.push(AgentLoadFailure {
                        path: agent_toml,
                        error: format!(
                            "v1 profile load failed: {v1_error}; legacy profile load failed: {legacy_error}"
                        ),
                    }),
                }
            }
        }
    }

    Ok(AgentInventory {
        legacy_profiles,
        v1_profiles,
        failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn write_agent(dir: &Path, name: &str, content: String) -> PathBuf {
        let agent_dir = dir.join(name);
        std::fs::create_dir_all(&agent_dir).unwrap();
        let path = agent_dir.join("agent.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn inventory_recognizes_v1_default_template_without_hash_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = write_agent(
            dir.path(),
            "p1-review-agent",
            lattice_profile::default_agent_template("p1-review-agent", "sonnet"),
        );

        let inventory = load_agent_inventory(dir.path()).unwrap();

        assert_eq!(inventory.profile_count(), 1);
        assert!(inventory.failures().is_empty());
        assert_eq!(
            inventory.v1_profiles()[0].profile.agent.name,
            "p1-review-agent"
        );
        assert!(
            !profile_path.with_extension("toml.hash").exists(),
            "v1 discovery must stay read-only and not create legacy hash sidecars"
        );
    }

    #[test]
    fn inventory_reports_failed_loads_without_hash_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = write_agent(
            dir.path(),
            "broken",
            r#"
[agent]
name = "broken"
model = "sonnet"
"#
            .to_string(),
        );

        let inventory = load_agent_inventory(dir.path()).unwrap();

        assert_eq!(inventory.profile_count(), 0);
        assert_eq!(inventory.failures().len(), 1);
        assert!(
            !profile_path.with_extension("toml.hash").exists(),
            "failed loads must not create legacy hash sidecars"
        );
    }

    #[test]
    fn inventory_loads_legacy_profile_without_creating_hash_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let profile_path = write_agent(
            dir.path(),
            "legacy",
            r#"
[agent]
name = "legacy"
model = "sonnet"

[system]
prompt = "legacy prompt"
"#
            .to_string(),
        );

        let inventory = load_agent_inventory(dir.path()).unwrap();

        assert_eq!(inventory.profile_count(), 1);
        assert!(inventory.failures().is_empty());
        assert!(
            !profile_path.with_extension("toml.hash").exists(),
            "list/validate inventory must not create hash sidecars for legacy profiles"
        );
    }
}
