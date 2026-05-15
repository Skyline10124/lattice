use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSummary {
    pub name: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLoadFailure {
    pub path: std::path::PathBuf,
    pub error: String,
}

#[derive(Clone)]
pub struct AgentInventory {
    v1_profiles: Vec<lattice_profile::LoadedAgentProfile>,
    failures: Vec<AgentLoadFailure>,
}

impl AgentInventory {
    pub fn profile_count(&self) -> usize {
        self.v1_profiles.len()
    }

    pub fn v1_profiles(&self) -> &[lattice_profile::LoadedAgentProfile] {
        &self.v1_profiles
    }

    pub fn failures(&self) -> &[AgentLoadFailure] {
        &self.failures
    }

    pub fn summaries(&self) -> Vec<AgentSummary> {
        let mut summaries = Vec::new();

        for loaded in &self.v1_profiles {
            summaries.push(AgentSummary {
                name: loaded.profile.agent.name.clone(),
                model: loaded.profile.agent.model.clone(),
            });
        }

        summaries
    }
}

pub fn load_agent_inventory(dir: &Path) -> Result<AgentInventory, Box<dyn std::error::Error>> {
    let mut v1_profiles = Vec::new();
    let mut failures = Vec::new();

    if !dir.exists() {
        return Ok(AgentInventory {
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
            Err(error) => failures.push(AgentLoadFailure {
                path: agent_toml,
                error: error.to_string(),
            }),
        }
    }

    Ok(AgentInventory {
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
    fn agent_inventory_loads_only_v1_profiles() {
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

        assert_eq!(inventory.profile_count(), 0);
        assert_eq!(inventory.failures().len(), 1);
        assert!(
            !profile_path.with_extension("toml.hash").exists(),
            "v1-only inventory must not create legacy hash sidecars"
        );
    }
}
