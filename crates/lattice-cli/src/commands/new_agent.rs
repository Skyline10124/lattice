use anyhow::{bail, Result};

use crate::config::Config;

pub fn run(name: String) -> Result<()> {
    let agents_dir = super::safe_agents_dir(None).map_err(anyhow::Error::msg)?;
    let dir = agents_dir.join(&name);

    if dir.exists() {
        bail!("Agent '{}' already exists at '{}'", name, dir.display());
    }

    std::fs::create_dir_all(&dir)?;

    let config = Config::load(None)?;
    let default_model = config.default_model();

    let template = lattice_profile::default_agent_template(&name, &default_model);
    let toml_path = dir.join("agent.toml");
    std::fs::write(&toml_path, template)?;

    println!("Created agent '{}' at {}", name, dir.display());
    println!();
    println!("  {}", toml_path.display());
    println!();
    println!("Edit the file to configure your agent, then run:");
    println!("  lattice run --agent {} \"hello\"", name);
    println!();

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn template_uses_lattice_profile_v1_default() {
        let template = lattice_profile::default_agent_template("test-agent", "sonnet");

        // Must contain allowed v1 sections.
        assert!(template.contains("schema_version = 1"));
        assert!(template.contains("[agent]"));
        assert!(template.contains("[prompt]"));
        assert!(template.contains("[tools]"));
        assert!(template.contains("[security]"));
        assert!(template.contains("[context]"));
        assert!(template.contains("[behavior]"));

        // Must contain auto-generated comment.
        let lower = template.to_lowercase();
        assert!(lower.contains("auto-generated"));
        assert!(lower.contains("role"));
        assert!(lower.contains("prompt"));
        assert!(lower.contains("tools"));
        assert!(lower.contains("security"));
    }

    #[test]
    fn template_has_no_legacy_fields() {
        let template = lattice_profile::default_agent_template("t", "sonnet");
        let forbidden = [
            "[capabilities]",
            "presets",
            "[handoff]",
            "[orchestrator]",
            "[plugins]",
            "[pipeline]",
            "[subagents]",
            "[micro_agents]",
            "[memory]",
            "skippable",
            "tags",
            "behavior_type",
            "confidence_threshold",
            "max_retries",
            "enforce_sandbox",
        ];
        for f in &forbidden {
            assert!(!template.contains(f), "template must not contain '{f}'");
        }
    }
}
