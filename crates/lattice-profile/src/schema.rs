use serde::{Deserialize, Serialize};

/// Strict v1 `agent.toml` profile. Only `schema_version = 1` is accepted.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    pub schema_version: u64,
    pub agent: AgentSection,
    pub prompt: PromptSection,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub security: Option<SecuritySection>,
    #[serde(default)]
    pub context: Option<ContextSection>,
    #[serde(default)]
    pub behavior: Option<BehaviorSection>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSection {
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub system_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    #[serde(default)]
    pub bundles: Vec<String>,
    #[serde(default)]
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    Project,
    Strict,
    Permissive,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SecuritySection {
    pub sandbox_mode: SandboxMode,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSection {
    #[serde(default = "default_true")]
    pub project_instructions: bool,
    #[serde(default = "default_true")]
    pub workspace_context: bool,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BehaviorSection {
    #[serde(default)]
    pub max_turns: Option<u32>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> Result<AgentProfile, toml::de::Error> {
        toml::from_str::<AgentProfile>(toml_str)
    }

    #[test]
    fn valid_minimal_profile() {
        let toml = r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "You are a helpful assistant."
"#;
        let profile = parse(toml).unwrap();
        assert_eq!(profile.schema_version, 1);
        assert_eq!(profile.agent.name, "test");
        assert_eq!(profile.agent.model, "sonnet");
        assert!(profile.agent.description.is_none());
        assert_eq!(
            profile.prompt.system.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert!(profile.prompt.system_path.is_none());
    }

    #[test]
    fn valid_full_profile() {
        let toml = r#"
schema_version = 1

[agent]
name = "full"
model = "opus"
description = "A full-featured agent"

[prompt]
system_path = "prompt.md"

[tools]
bundles = ["workspace", "filesystem"]
enabled = ["grep"]

[security]
sandbox_mode = "strict"

[context]
project_instructions = true
workspace_context = false

[behavior]
max_turns = 42
reasoning_effort = "high"
"#;
        let profile = parse(toml).unwrap();
        assert_eq!(
            profile.agent.description.as_deref(),
            Some("A full-featured agent")
        );
        assert_eq!(profile.prompt.system_path.as_deref(), Some("prompt.md"));
        let tools = &profile.tools;
        assert_eq!(tools.bundles, vec!["workspace", "filesystem"]);
        assert_eq!(tools.enabled, vec!["grep"]);
        let sec = profile.security.unwrap();
        assert!(matches!(sec.sandbox_mode, SandboxMode::Strict));
        let ctx = profile.context.unwrap();
        assert!(ctx.project_instructions);
        assert!(!ctx.workspace_context);
        let bhv = profile.behavior.unwrap();
        assert_eq!(bhv.max_turns, Some(42));
        assert_eq!(bhv.reasoning_effort.as_deref(), Some("high"));
    }

    // schema_version rejection is tested in load.rs (loader-level validation)

    #[test]
    fn reject_unknown_top_level_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[orchestrator]
some_field = "bad"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("orchestrator"), "got: {err}");
    }

    #[test]
    fn reject_capabilities_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[capabilities]
presets = ["coding"]
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("capabilities"), "got: {err}");
    }

    #[test]
    fn reject_handoff_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[handoff]
enabled = true
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_all_forbidden_top_level_sections() {
        let forbidden = [
            "capabilities",
            "handoff",
            "orchestrator",
            "plugins",
            "pipeline",
            "subagents",
            "micro_agents",
            "memory",
        ];
        for section in forbidden {
            let toml = format!(
                r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[{section}]
x = "y"
"#
            );
            assert!(
                parse(&toml).is_err(),
                "section [{section}] should be rejected"
            );
        }
    }

    #[test]
    fn reject_agent_preset_field() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"
preset = "coding"

[prompt]
system = "hi"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("preset"), "got: {err}");
    }

    #[test]
    fn reject_agent_type_code_agent() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"
type = "code_agent"

[prompt]
system = "hi"
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_agent_skippable() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"
skippable = true

[prompt]
system = "hi"
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_agent_tags() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"
tags = ["coding"]

[prompt]
system = "hi"
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_behavior_behavior_type() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[behavior]
behavior_type = "code_agent"
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_behavior_confidence_threshold() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[behavior]
confidence_threshold = 0.9
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_behavior_max_retries() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[behavior]
max_retries = 3
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_behavior_enforce_sandbox() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[behavior]
enforce_sandbox = true
"#;
        assert!(parse(toml).is_err());
    }

    #[test]
    fn reject_invalid_sandbox_mode() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[security]
sandbox_mode = "unknown"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("sandbox_mode"), "got: {err}");
    }

    #[test]
    fn accept_all_valid_sandbox_modes() {
        for mode in &["project", "strict", "permissive"] {
            let toml = format!(
                r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[security]
sandbox_mode = "{mode}"
"#
            );
            parse(&toml).unwrap();
        }
    }

    /// Guard: no legacy symbols in production source.
    #[test]
    fn source_free_of_forbidden_symbols() {
        let source = include_str!("schema.rs");
        let test_start = source.find("#[cfg(test)]").unwrap_or(source.len());
        let prod = &source[..test_start];
        let forbidden_patterns = [
            "preset",
            "skippable",
            "tags",
            "behavior_type",
            "confidence_threshold",
            "max_retries",
            "enforce_sandbox",
            "code_agent",
            "code-agent",
            "AgentPreset",
            "ToolPreset",
        ];
        for pattern in forbidden_patterns {
            if prod.contains(pattern) {
                panic!("schema.rs production code must not contain '{pattern}'");
            }
        }
    }

    #[test]
    fn tools_section_defaults_to_empty_arrays() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"
"#;
        let profile = parse(toml).unwrap();
        assert!(profile.tools.bundles.is_empty());
        assert!(profile.tools.enabled.is_empty());
    }

    #[test]
    fn context_defaults_to_true_when_absent() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"
"#;
        let profile = parse(toml).unwrap();
        assert!(profile.context.is_none()); // section absent → None
    }

    #[test]
    fn context_defaults_to_true_when_section_present_but_fields_missing() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[context]
"#;
        let profile = parse(toml).unwrap();
        let ctx = profile.context.unwrap();
        assert!(ctx.project_instructions);
        assert!(ctx.workspace_context);
    }
}
