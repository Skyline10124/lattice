use lancet_protocol::CepMode;
use serde::{de, Deserialize, Serialize};
use std::collections::BTreeMap;

/// Strict v1 `agent.toml` profile. Only `schema_version = 1` is accepted.
///
/// Removed sections: `[security]`, `[context]`, `[behavior]`.
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
    pub llm: LlmSection,
    #[serde(default)]
    pub lancet: LancetSection,
}

/// LANCET `[lancet]` profile section with typed policy sub-tables.
///
/// Known sub-tables (`project_root`, `observation`, `network`) deserialize into
/// typed fields. Other future sub-tables (e.g. `[lancet.command]`) remain in
/// `extensions`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LancetSection {
    #[serde(default = "default_lancet_mode")]
    pub mode: CepMode,
    #[serde(default)]
    pub project_root: Option<LancetProjectRootSection>,
    #[serde(default)]
    pub observation: Option<LancetObservationSection>,
    #[serde(default)]
    pub network: Option<LancetNetworkSection>,
    #[serde(default)]
    pub command: Option<LancetCommandSection>,
    #[serde(default)]
    pub policy: Option<LancetPolicySection>,
    #[serde(default)]
    pub invariants: Option<LancetInvariantsSection>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, toml::Value>,
}

/// `[lancet.project_root]` — effective workspace scope for observation policy.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetProjectRootSection {
    #[serde(default)]
    pub root: Option<String>,
}

/// `[lancet.observation]` — path allow roots and observation/todo budget limits.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetObservationSection {
    #[serde(default)]
    pub allowed_roots: Vec<String>,
    pub max_single_read_bytes: Option<usize>,
    pub max_turn_observation_bytes: Option<usize>,
    pub max_grep_matches: Option<usize>,
    pub max_list_entries: Option<usize>,
    pub max_find_paths: Option<usize>,
    pub max_todo_ops: Option<usize>,
}

/// `[lancet.network]` — `web.fetch` host policy and response size cap.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetNetworkSection {
    pub allow_public_https: Option<bool>,
    pub allow_localhost: Option<bool>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    pub max_web_response_bytes: Option<usize>,
}

/// `[lancet.command]` — command execution policy overrides.
///
/// All fields are `Option<T>` with `#[serde(default)]` so missing fields
/// fall back to `CommandPolicy::defaults()`. Missing `[lancet.command]` entirely
/// uses all defaults. Older profiles without `[lancet.command]` still parse.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetCommandSection {
    #[serde(default)]
    pub default_timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_timeout_ms: Option<u64>,
    #[serde(default)]
    pub max_output_bytes: Option<usize>,
    #[serde(default)]
    pub allow_inline_shell: Option<bool>,
    #[serde(default)]
    pub allowed_diagnostic_commands: Option<Vec<Vec<String>>>,
    #[serde(default)]
    pub denied_commands: Option<Vec<Vec<String>>>,
    #[serde(default)]
    pub allowed_env: Option<Vec<String>>,
    #[serde(default)]
    pub inherited_env: Option<Vec<String>>,
}

/// `[lancet.policy]` — pointer to an external structured TOML policy file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetPolicySection {
    #[serde(default)]
    pub path: Option<String>,
}

/// `[lancet.invariants]` — invariant card directory configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetInvariantsSection {
    #[serde(default)]
    pub dir: Option<String>,
}

fn default_lancet_mode() -> CepMode {
    CepMode::Enforce
}

impl Default for LancetSection {
    fn default() -> Self {
        Self {
            mode: default_lancet_mode(),
            project_root: None,
            observation: None,
            network: None,
            command: None,
            policy: None,
            invariants: None,
            extensions: BTreeMap::new(),
        }
    }
}

impl<'de> Deserialize<'de> for LancetSection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut raw = BTreeMap::<String, toml::Value>::deserialize(deserializer)?;
        let mode = match raw.remove("mode") {
            Some(value) => value.try_into().map_err(de::Error::custom)?,
            None => default_lancet_mode(),
        };

        for (key, value) in &raw {
            if !matches!(value, toml::Value::Table(_)) {
                return Err(de::Error::custom(format!(
                    "unknown scalar field `lancet.{key}`; only `mode` and future sub-tables are supported"
                )));
            }
        }

        let project_root = raw
            .remove("project_root")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let observation = raw
            .remove("observation")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let network = raw
            .remove("network")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let command = raw
            .remove("command")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let policy = raw
            .remove("policy")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let invariants = raw
            .remove("invariants")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;

        Ok(Self {
            mode,
            project_root,
            observation,
            network,
            command,
            policy,
            invariants,
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

/// Tool section: only explicit `enabled` tool names are supported.
///
/// `bundles` and any other field are rejected by `#[serde(deny_unknown_fields)]`.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolsSection {
    #[serde(default)]
    pub enabled: Vec<String>,
}

/// LLM provider execution settings.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LlmSection {
    #[serde(default)]
    pub retry: LlmRetrySection,
}

/// `[llm.retry]` — retry policy for opening provider streams.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LlmRetrySection {
    #[serde(default = "default_llm_retry_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_llm_retry_initial_delay_ms")]
    pub initial_delay_ms: u64,
    #[serde(default = "default_llm_retry_max_delay_ms")]
    pub max_delay_ms: u64,
}

impl Default for LlmRetrySection {
    fn default() -> Self {
        Self {
            max_retries: default_llm_retry_max_retries(),
            initial_delay_ms: default_llm_retry_initial_delay_ms(),
            max_delay_ms: default_llm_retry_max_delay_ms(),
        }
    }
}

fn default_llm_retry_max_retries() -> u32 {
    3
}

fn default_llm_retry_initial_delay_ms() -> u64 {
    500
}

fn default_llm_retry_max_delay_ms() -> u64 {
    3_000
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

[llm.retry]
max_retries = 2
initial_delay_ms = 250
max_delay_ms = 1000
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
        assert_eq!(profile.llm.retry.max_retries, 2);
        assert_eq!(profile.llm.retry.initial_delay_ms, 250);
        assert_eq!(profile.llm.retry.max_delay_ms, 1000);
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
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
        assert!(profile.lancet.extensions.is_empty());
    }

    #[test]
    fn cep_parses_enforce_mode() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
    }

    #[test]
    fn cep_parses_warn_mode() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "warn"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Warn);
    }

    #[test]
    fn cep_parses_off_mode() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "off"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Off);
    }

    #[test]
    fn cep_rejects_invalid_mode() {
        let toml = format!(
            r#"{prefix}

[lancet]
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

[lancet]
mod = "off"
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(err.to_string().contains("lancet.mod"), "got: {err}");
    }

    #[test]
    fn cep_parses_typed_prof02_sub_tables() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.project_root]
root = "."

[lancet.observation]
allowed_roots = ["."]
max_single_read_bytes = 4096
max_turn_observation_bytes = 8192
max_grep_matches = 500
max_list_entries = 300
max_find_paths = 400
max_todo_ops = 10

[lancet.network]
allow_public_https = true
allow_localhost = false
allowed_hosts = ["example.com"]
max_web_response_bytes = 2048
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
        let project_root = profile.lancet.project_root.as_ref().unwrap();
        assert_eq!(project_root.root.as_deref(), Some("."));
        let observation = profile.lancet.observation.as_ref().unwrap();
        assert_eq!(observation.allowed_roots, vec!["."]);
        assert_eq!(observation.max_single_read_bytes, Some(4096));
        assert_eq!(observation.max_turn_observation_bytes, Some(8192));
        assert_eq!(observation.max_grep_matches, Some(500));
        let network = profile.lancet.network.as_ref().unwrap();
        assert_eq!(network.allow_public_https, Some(true));
        assert_eq!(network.allow_localhost, Some(false));
        assert_eq!(network.allowed_hosts, vec!["example.com"]);
        assert_eq!(network.max_web_response_bytes, Some(2048));
    }

    #[test]
    fn cep_preserves_untyped_future_sub_tables_in_extensions() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.future_table]
some_field = true
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
        assert!(profile.lancet.extensions.contains_key("future_table"));
        assert!(profile.lancet.project_root.is_none());
        assert!(profile.lancet.observation.is_none());
        assert!(profile.lancet.network.is_none());
        assert!(profile.lancet.command.is_none());
    }

    #[test]
    fn cep_older_profile_mode_only_still_parses() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.lancet.mode, CepMode::Enforce);
        assert!(profile.lancet.project_root.is_none());
        assert!(profile.lancet.observation.is_none());
        assert!(profile.lancet.network.is_none());
    }

    #[test]
    fn cep_observation_rejects_network_only_field() {
        let toml = format!(
            r#"{prefix}

[lancet.observation]
max_web_response_bytes = 1024
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("max_web_response_bytes"),
            "got: {err}"
        );
    }

    // ── LancetCommandSection ────────────────────────────────────────────

    #[test]
    fn cep_command_section_parses_all_fields() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.command]
default_timeout_ms = 60000
max_timeout_ms = 180000
max_output_bytes = 100000
allow_inline_shell = true
allowed_diagnostic_commands = [["cargo", "test"], ["git", "diff"]]
denied_commands = [["rm", "-rf"]]
allowed_env = ["RUST_LOG"]
inherited_env = ["PATH", "HOME"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let cmd = profile.lancet.command.as_ref().unwrap();
        assert_eq!(cmd.default_timeout_ms, Some(60000));
        assert_eq!(cmd.max_timeout_ms, Some(180000));
        assert_eq!(cmd.max_output_bytes, Some(100000));
        assert_eq!(cmd.allow_inline_shell, Some(true));
        assert_eq!(
            cmd.allowed_diagnostic_commands.as_ref().unwrap(),
            &vec![
                vec!["cargo".to_string(), "test".to_string()],
                vec!["git".to_string(), "diff".to_string()]
            ]
        );
        assert_eq!(
            cmd.denied_commands.as_ref().unwrap(),
            &vec![vec!["rm".to_string(), "-rf".to_string()]]
        );
        assert_eq!(
            cmd.allowed_env.as_ref().unwrap(),
            &vec!["RUST_LOG".to_string()]
        );
        assert_eq!(
            cmd.inherited_env.as_ref().unwrap(),
            &vec!["PATH".to_string(), "HOME".to_string()]
        );
    }

    #[test]
    fn cep_command_section_defaults_when_absent() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(profile.lancet.command.is_none());
    }

    #[test]
    fn cep_command_section_partial_fields_use_defaults() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.command]
default_timeout_ms = 30000
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let cmd = profile.lancet.command.as_ref().unwrap();
        assert_eq!(cmd.default_timeout_ms, Some(30000));
        assert!(cmd.max_timeout_ms.is_none());
        assert!(cmd.allow_inline_shell.is_none());
        assert!(cmd.allowed_diagnostic_commands.is_none());
    }

    #[test]
    fn cep_command_section_rejects_unknown_fields() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.command]
unknown_field = true
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("unknown_field"),
            "should reject unknown field, got: {err}"
        );
    }

    #[test]
    fn cep_command_section_old_profile_without_command_still_parses() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.observation]
allowed_roots = ["."]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert!(profile.lancet.command.is_none());
        assert!(profile.lancet.observation.is_some());
    }

    #[test]
    fn cep_command_section_moved_from_extensions_to_typed() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.command]
default_timeout_ms = 60000
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        // Should be in typed field, NOT in extensions
        assert!(profile.lancet.command.is_some());
        assert!(!profile.lancet.extensions.contains_key("command"));
    }

    #[test]
    fn cep_serializes_extensions_flattened() {
        let mut future = toml::map::Map::new();
        future.insert("some_field".into(), toml::Value::Boolean(true));

        let mut extensions = BTreeMap::new();
        extensions.insert("future_table".into(), toml::Value::Table(future));

        let section = LancetSection {
            mode: CepMode::Warn,
            project_root: None,
            observation: None,
            network: None,
            command: None,
            policy: None,
            invariants: None,
            extensions,
        };
        let serialized = toml::to_string(&section).unwrap();
        assert!(serialized.contains("mode = \"warn\""), "got: {serialized}");
        assert!(serialized.contains("[future_table]"), "got: {serialized}");
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

    #[test]
    fn llm_retry_defaults_when_section_absent() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert_eq!(profile.llm.retry.max_retries, 3);
        assert_eq!(profile.llm.retry.initial_delay_ms, 500);
        assert_eq!(profile.llm.retry.max_delay_ms, 3_000);
    }

    #[test]
    fn llm_retry_parses_explicit_values() {
        let toml = format!(
            r#"{prefix}

[llm.retry]
max_retries = 0
initial_delay_ms = 1
max_delay_ms = 2
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.llm.retry.max_retries, 0);
        assert_eq!(profile.llm.retry.initial_delay_ms, 1);
        assert_eq!(profile.llm.retry.max_delay_ms, 2);
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

    // ── LancetPolicySection ────────────────────────────────────────────────

    #[test]
    fn cep_policy_section_parses_path() {
        let toml = format!(
            r#"{prefix}

[lancet.policy]
path = "policy.toml"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let policy = profile.lancet.policy.as_ref().unwrap();
        assert_eq!(policy.path.as_deref(), Some("policy.toml"));
    }

    #[test]
    fn cep_section_without_policy_defaults_to_none() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(profile.lancet.policy.is_none());
    }

    #[test]
    fn cep_policy_section_rejects_unknown_fields() {
        let toml = format!(
            r#"{prefix}

[lancet.policy]
unknown = true
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(
            err.to_string().contains("unknown"),
            "should reject unknown field, got: {err}"
        );
    }

    // ── LancetInvariantsSection ──────────────────────────────────────────

    #[test]
    fn cep_section_parses_invariants_section() {
        let toml = format!(
            r#"{prefix}

[lancet]
mode = "enforce"

[lancet.invariants]
dir = ".lancet/cards/"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let invariants = profile.lancet.invariants.as_ref().unwrap();
        assert_eq!(invariants.dir.as_deref(), Some(".lancet/cards/"));
    }

    #[test]
    fn cep_section_absent_invariants_is_none() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(profile.lancet.invariants.is_none());
    }

    #[test]
    fn cep_invariants_does_not_leak_to_extensions() {
        let toml = format!(
            r#"{prefix}

[lancet.invariants]
dir = "foo/"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert!(
            !profile.lancet.extensions.contains_key("invariants"),
            "invariants must not leak to extensions"
        );
        assert!(profile.lancet.invariants.is_some());
    }
}
