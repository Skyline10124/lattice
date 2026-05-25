use lancet_protocol::CepMode;
use serde::{de, Deserialize, Serialize};
use std::collections::BTreeMap;

/// Strict v1 `agent.toml` profile. Only `schema_version = 1` is accepted.
///
/// Sections removed in v1.1: `[security]`, `[context]`, `[behavior]`.
/// The `[tools]` section only supports explicit `enabled` — no `bundles`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentProfile {
    pub schema_version: u64,
    pub agent: AgentSection,
    pub prompt: PromptSection,
    #[serde(default)]
    pub tools: ToolsSection,
    #[serde(default)]
    pub cep: CepSection,
}

/// Phase 19 `[cep]` profile section. Only `mode` is typed; future sub-tables
/// (e.g. `[cep.command]`, `[cep.network]`) deserialize into `extensions`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CepSection {
    #[serde(default = "default_cep_mode")]
    pub mode: CepMode,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, toml::Value>,
}

fn default_cep_mode() -> CepMode {
    CepMode::Enforce
}

impl Default for CepSection {
    fn default() -> Self {
        Self {
            mode: default_cep_mode(),
            extensions: BTreeMap::new(),
        }
    }
}

impl<'de> Deserialize<'de> for CepSection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut raw = BTreeMap::<String, toml::Value>::deserialize(deserializer)?;
        let mode = match raw.remove("mode") {
            Some(value) => value.try_into().map_err(de::Error::custom)?,
            None => default_cep_mode(),
        };

        for (key, value) in &raw {
            if !matches!(value, toml::Value::Table(_)) {
                return Err(de::Error::custom(format!(
                    "unknown scalar field `cep.{key}`; only `mode` and future sub-tables are supported"
                )));
            }
        }

        Ok(Self {
            mode,
            extensions: raw,
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    pub name: String,
    pub model: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PromptSection {
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub system_path: Option<String>,
}

/// v1.1 tool section: only explicit `enabled` tool names are supported.
///
/// `bundles` and any other field are rejected by `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    #[serde(default)]
    pub enabled: Vec<String>,
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
enabled = ["workspace.grep", "workspace.read_file", "command.run"]
"#;
        let profile = parse(toml).unwrap();
        assert_eq!(
            profile.agent.description.as_deref(),
            Some("A full-featured agent")
        );
        assert_eq!(profile.prompt.system_path.as_deref(), Some("prompt.md"));
        let tools = &profile.tools;
        assert_eq!(
            tools.enabled,
            vec!["workspace.grep", "workspace.read_file", "command.run"]
        );
    }

    // ── Top-level section rejection ───────────────────────────────────

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
    fn reject_security_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[security]
sandbox_mode = "project"
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("security"), "got: {err}");
    }

    #[test]
    fn reject_context_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[context]
project_instructions = true
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("context"), "got: {err}");
    }

    #[test]
    fn reject_behavior_section() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[behavior]
max_turns = 10
"#;
        let err = parse(toml).unwrap_err();
        assert!(err.to_string().contains("behavior"), "got: {err}");
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
            "security",
            "context",
            "behavior",
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

    // ── Tools section rejection ───────────────────────────────────────

    #[test]
    fn reject_tools_bundles() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[tools]
bundles = ["workspace"]
"#;
        let err = parse(toml).unwrap_err();
        assert!(
            err.to_string().contains("bundles") || err.to_string().contains("unknown field"),
            "bundles must be rejected, got: {err}"
        );
    }

    // ── Agent field rejection ─────────────────────────────────────────

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

    // ── CEP section ───────────────────────────────────────────────────

    fn minimal_profile_prefix() -> &'static str {
        r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"
"#
    }

    #[test]
    fn cep_defaults_to_enforce_when_section_absent() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert_eq!(profile.cep.mode, CepMode::Enforce);
        assert!(profile.cep.extensions.is_empty());
    }

    #[test]
    fn cep_parses_enforce_mode() {
        let toml = format!(
            r#"{prefix}

[cep]
mode = "enforce"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.cep.mode, CepMode::Enforce);
    }

    #[test]
    fn cep_parses_warn_mode() {
        let toml = format!(
            r#"{prefix}

[cep]
mode = "warn"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.cep.mode, CepMode::Warn);
    }

    #[test]
    fn cep_parses_off_mode() {
        let toml = format!(
            r#"{prefix}

[cep]
mode = "off"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.cep.mode, CepMode::Off);
    }

    #[test]
    fn cep_rejects_invalid_mode() {
        let toml = format!(
            r#"{prefix}

[cep]
mode = "permissive"
"#,
            prefix = minimal_profile_prefix()
        );
        assert!(parse(&toml).is_err());
    }

    #[test]
    fn cep_rejects_scalar_extension_typos() {
        let toml = format!(
            r#"{prefix}

[cep]
mod = "off"
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(err.to_string().contains("cep.mod"), "got: {err}");
    }

    #[test]
    fn cep_preserves_future_sub_tables_in_extensions() {
        let toml = format!(
            r#"{prefix}

[cep]
mode = "enforce"

[cep.command]
allow_shell = true

[cep.network]
host_allowlist = ["example.com"]

[cep.observation]
max_read_bytes = 4096
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.cep.mode, CepMode::Enforce);
        assert!(profile.cep.extensions.contains_key("command"));
        assert!(profile.cep.extensions.contains_key("network"));
        assert!(profile.cep.extensions.contains_key("observation"));
    }

    #[test]
    fn cep_serializes_future_sub_tables_flattened() {
        let mut command = toml::map::Map::new();
        command.insert("allow_shell".into(), toml::Value::Boolean(true));

        let mut extensions = BTreeMap::new();
        extensions.insert("command".into(), toml::Value::Table(command));

        let section = CepSection {
            mode: CepMode::Warn,
            extensions,
        };
        let serialized = toml::to_string(&section).unwrap();
        assert!(serialized.contains("mode = \"warn\""), "got: {serialized}");
        assert!(serialized.contains("[command]"), "got: {serialized}");
        assert!(!serialized.contains("extensions"), "got: {serialized}");
    }

    // ── Tools section defaults ────────────────────────────────────────

    #[test]
    fn tools_section_defaults_to_empty_enabled() {
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"
"#;
        let profile = parse(toml).unwrap();
        assert!(profile.tools.enabled.is_empty());
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
            "SandboxMode",
            "SecuritySection",
            "ContextSection",
            "BehaviorSection",
        ];
        // Note: "bundles" is allowed in doc comments explaining its removal.
        for pattern in forbidden_patterns {
            if prod.contains(pattern) {
                panic!("schema.rs production code must not contain '{pattern}'");
            }
        }
    }
}
