// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use lancet_protocol::CepMode;
use lancet_protocol::ImpactKind;
use lancet_protocol::cell::Trust;
use lancet_protocol::evidence::CapabilityAdmissionPolicy;
use serde::{Deserialize, Serialize, de};
use std::collections::BTreeMap;

/// Strict v1 `agent.toml` profile. Only `schema_version = 1` is accepted.
///
/// Removed sections: `[security]`, `[context]`, `[behavior]`, `[workspace]`.
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

impl AgentProfile {
    /// Lower the profile into the Component Tool admission inputs.
    ///
    /// `enabled_tools` is `[tools].enabled` — the exposure list is also the
    /// stage-1 capability grant. An absent `[lancet.capabilities]` is the
    /// typed default: nothing denied, `max_boundary = in`.
    pub fn capability_admission_policy(&self) -> CapabilityAdmissionPolicy {
        let capabilities = self.lancet.capabilities.as_ref();
        CapabilityAdmissionPolicy {
            enabled_tools: self.tools.enabled.iter().cloned().collect(),
            denied_capabilities: capabilities
                .map(|section| section.deny.iter().cloned().collect())
                .unwrap_or_default(),
            max_boundary: capabilities.map_or_else(default_max_boundary, |s| s.max_boundary),
        }
    }
}

/// The `[lancet.*]` sub-tables this schema types, in declaration order.
///
/// Lives next to [`LancetSection`] because it must match that struct's fields:
/// the loader uses it to reject a *case variant* of a typed name, which
/// `extensions` would otherwise swallow along with everything inside it.
/// `typed_lancet_sub_tables_match_the_struct_fields` fails if the two drift.
pub const TYPED_LANCET_SUB_TABLES: &[&str] = &[
    "project_root",
    "observation",
    "network",
    "policy",
    "effects",
    "capabilities",
    "workspace",
];

/// LANCET `[lancet]` profile section with typed policy sub-tables.
///
/// Sub-tables named in [`TYPED_LANCET_SUB_TABLES`] deserialize into typed fields
/// and are therefore strictly validated. Other future sub-tables remain in
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
    pub policy: Option<LancetPolicySection>,
    #[serde(default)]
    pub effects: Option<LancetEffectsSection>,
    #[serde(default)]
    pub capabilities: Option<LancetCapabilitiesSection>,
    #[serde(default)]
    pub workspace: Option<LancetWorkspacePolicySection>,
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

/// `[lancet.observation]` — observation and todo budget limits.
///
/// Observation *scope* is not configured here: it is
/// `[lancet.project_root].root` plus the `[lancet.workspace]` read-visibility
/// rules. A retired `allowed_roots` key was validated but never reached any
/// read decision, so it silently left the Agent's observation surface wider
/// than the profile implied (#121); `load.rs` names it in its retired-key
/// pre-scan.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetObservationSection {
    pub max_single_read_bytes: Option<usize>,
    pub max_turn_observation_bytes: Option<usize>,
    pub max_grep_matches: Option<usize>,
    pub max_list_entries: Option<usize>,
    pub max_find_paths: Option<usize>,
    pub max_todo_ops: Option<usize>,
}

/// `[lancet.network]` — `fetch.web` host policy and response size cap.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetNetworkSection {
    pub allow_public_https: Option<bool>,
    pub allow_localhost: Option<bool>,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    pub max_web_response_bytes: Option<usize>,
}

/// `[lancet.policy]` — pointer to an external structured TOML policy file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetPolicySection {
    #[serde(default)]
    pub path: Option<String>,
}

/// `[lancet.workspace]` — Local Workspace write-deny and read-visibility policy.
///
/// Write policy is a blacklist: a path is allowed unless it matches any denied
/// write pattern. Read visibility defaults to tracked and non-ignored files;
/// `included_ignored_read_patterns` may expose specific ignored paths, while
/// `denied_read_patterns` may hide paths from either set. Runtime System Path
/// Exclusions (`.git`, `.lattice`, root escape) always win over profile rules.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetWorkspacePolicySection {
    #[serde(default)]
    pub denied_write_patterns: Vec<String>,
    #[serde(default)]
    pub included_ignored_read_patterns: Vec<String>,
    #[serde(default)]
    pub denied_read_patterns: Vec<String>,
}

/// `[lancet.effects]` — runtime effect-kind deny switch.
///
/// Native-path only: it denies method *impact kinds* on the legacy
/// authorization path. Component Tool policy lives in
/// `[lancet.capabilities]` and never names method impact identity (B8); this
/// section is retired only once every consumer has migrated.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct LancetEffectsSection {
    #[serde(default)]
    pub deny: Vec<ImpactKind>,
}

/// `[lancet.capabilities]` — Component Tool capability admission policy.
///
/// Admission runs in a fixed stage order: the enabled tools' declared
/// capabilities form the grant (there is no independent allow-list, so
/// profiles stay portable), `deny` subtracts from it, `max_boundary` removes
/// every capability whose scope trust resolves to `ex`, and the concrete
/// per-operation target check runs at call time. A tool left with no
/// capability is not callable.
///
/// Every `deny` entry must name a capability of the composed Component
/// catalog. An unknown name is a hard error at server construction, never a
/// warning: a deny typo that silently keeps a grant is fail-open. On a binary
/// whose Component catalog is empty, *any* `deny` entry therefore fails
/// startup.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LancetCapabilitiesSection {
    /// Capability names subtracted from the enabled tools' declared set.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Highest trust boundary an admitted capability may cross. `in` keeps
    /// every operation inside the Tool Host's control and accounting domain;
    /// `ex` opts into capabilities that reach the external world.
    #[serde(default = "default_max_boundary")]
    pub max_boundary: Trust,
}

impl Default for LancetCapabilitiesSection {
    /// Explicit, because `Trust::default()` is `Ex`: an undeclared *scope* is
    /// fail-closed as external, while an unset *boundary* is fail-closed as
    /// `in`. A derived `Default` would admit every `ex` capability.
    fn default() -> Self {
        Self {
            deny: Vec::new(),
            max_boundary: default_max_boundary(),
        }
    }
}

fn default_lancet_mode() -> CepMode {
    CepMode::Enforce
}

fn default_max_boundary() -> Trust {
    Trust::In
}

impl Default for LancetSection {
    fn default() -> Self {
        Self {
            mode: default_lancet_mode(),
            project_root: None,
            observation: None,
            network: None,
            policy: None,
            effects: None,
            capabilities: None,
            workspace: None,
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
        let policy = raw
            .remove("policy")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let effects = raw
            .remove("effects")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        // Typed here so an admission policy can never fall into `extensions`
        // and be silently ignored.
        let capabilities = raw
            .remove("capabilities")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;
        let workspace = raw
            .remove("workspace")
            .map(|value| value.try_into().map_err(de::Error::custom))
            .transpose()?;

        Ok(Self {
            mode,
            project_root,
            observation,
            network,
            policy,
            effects,
            capabilities,
            workspace,
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
    /// Optional cooperative per-turn deadline in seconds.
    ///
    /// When set, the agent loop races LLM stream consumption and stream
    /// creation against `now + timeout_secs`. The default is `None` (no
    /// deadline); the CLI's observer idle timeout remains a separate
    /// last-resort guard. Validated nonzero and <= 3600 at load time.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
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
    use std::collections::BTreeSet;

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
enabled = ["observe.workspace", "mutate.create"]

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
        assert_eq!(tools.enabled, vec!["observe.workspace", "mutate.create"]);
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
    fn reject_workspace_section() {
        // The Upper microVM `[workspace]` section was removed with the
        // Direct-Local migration (#40). Under `deny_unknown_fields`, legacy
        // profiles that still declare it now fail to parse — the strict
        // schema neither declares nor requires `[workspace]`.
        let toml = r#"
schema_version = 1

[agent]
name = "t"
model = "sonnet"

[prompt]
system = "hi"

[workspace]
image = "docker.io/library/python:3.12-slim"
"#;
        let err = parse(toml).unwrap_err();
        assert!(
            err.to_string().contains("workspace") || err.to_string().contains("unknown field"),
            "legacy [workspace] section must be rejected, got: {err}"
        );
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
            "workspace",
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
            policy: None,
            effects: None,
            capabilities: None,
            workspace: None,
            extensions,
        };
        let serialized = toml::to_string(&section).unwrap();
        assert!(serialized.contains("mode = \"warn\""), "got: {serialized}");
        assert!(serialized.contains("[future_table]"), "got: {serialized}");
        assert!(!serialized.contains("extensions"), "got: {serialized}");
    }

    /// `TYPED_LANCET_SUB_TABLES` drives the loader's case-variant rejection, so a
    /// field added to `LancetSection` without updating it would send that
    /// sub-table's case variants back to being silently swallowed by
    /// `extensions` (#121). Derived from serialization rather than restated, so
    /// the two cannot drift.
    #[test]
    fn typed_lancet_sub_tables_match_the_struct_fields() {
        let section = LancetSection {
            mode: CepMode::Warn,
            project_root: Some(LancetProjectRootSection::default()),
            observation: Some(LancetObservationSection::default()),
            network: Some(LancetNetworkSection::default()),
            policy: Some(LancetPolicySection::default()),
            effects: Some(LancetEffectsSection::default()),
            capabilities: Some(LancetCapabilitiesSection::default()),
            workspace: Some(LancetWorkspacePolicySection::default()),
            extensions: BTreeMap::new(),
        };
        let value = toml::Value::try_from(&section).unwrap();
        let mut serialized_tables = value
            .as_table()
            .unwrap()
            .iter()
            .filter(|(_, value)| value.is_table())
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        serialized_tables.sort();

        let mut declared = TYPED_LANCET_SUB_TABLES
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>();
        declared.sort();

        assert_eq!(
            serialized_tables, declared,
            "TYPED_LANCET_SUB_TABLES must list exactly the typed sub-table fields \
             of LancetSection"
        );
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

    // ── [lancet.workspace] policy section ─────────────────────────────

    #[test]
    fn lancet_workspace_roundtrip() {
        let toml = format!(
            r#"{prefix}

[lancet.workspace]
denied_write_patterns = [".git/**", ".env"]
included_ignored_read_patterns = [".env"]
denied_read_patterns = ["secrets/**"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile: AgentProfile = parse(&toml).unwrap();
        let section = profile.lancet.workspace.as_ref().unwrap();
        assert_eq!(
            section.denied_write_patterns,
            vec![".git/**".to_string(), ".env".to_string()]
        );
        assert_eq!(
            section.included_ignored_read_patterns,
            vec![".env".to_string()]
        );
        assert_eq!(section.denied_read_patterns, vec!["secrets/**".to_string()]);
    }

    #[test]
    fn lancet_workspace_unknown_field_rejected() {
        let toml = format!(
            r#"{prefix}

[lancet.workspace]
denied_write_patterns = [".git/**"]
unexpected = true
"#,
            prefix = minimal_profile_prefix()
        );
        assert!(parse(&toml).is_err());
    }

    #[test]
    fn lancet_verify_section_is_not_a_typed_field() {
        // [lancet.verify] was retired; it must no longer deserialize into a
        // typed field. It falls through into extensions as an untyped table.
        let toml = format!(
            r#"{prefix}

[lancet.verify]
argv = ["cargo", "test"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile: AgentProfile = parse(&toml).unwrap();
        assert!(profile.lancet.extensions.contains_key("verify"));
    }
    // ── [llm.timeout_secs] ────────────────────────────────────────────

    #[test]
    fn llm_timeout_secs_parses() {
        let toml = format!(
            r#"{prefix}

[llm]
timeout_secs = 1
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert_eq!(profile.llm.timeout_secs, Some(1));
    }

    #[test]
    fn llm_timeout_secs_defaults_none_when_absent() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(profile.llm.timeout_secs.is_none());
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

    // ── [lancet.capabilities] admission policy ─────────────────────────

    #[test]
    fn lancet_capabilities_parses_deny_and_boundary() {
        let toml = format!(
            r#"{prefix}

[lancet.capabilities]
deny = ["session.events", "net.fetch"]
max_boundary = "ex"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let capabilities = profile.lancet.capabilities.as_ref().unwrap();
        assert_eq!(
            capabilities.deny,
            vec!["session.events".to_string(), "net.fetch".to_string()]
        );
        assert_eq!(capabilities.max_boundary, Trust::Ex);
    }

    #[test]
    fn lancet_capabilities_round_trips_through_toml() {
        let toml = format!(
            r#"{prefix}

[tools]
enabled = ["noop.echo"]

[lancet.capabilities]
deny = ["session.events"]
max_boundary = "ex"
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        let serialized = toml::to_string(&profile).unwrap();
        assert!(
            serialized.contains("[lancet.capabilities]"),
            "got: {serialized}"
        );
        assert_eq!(parse(&serialized).unwrap(), profile);
    }

    #[test]
    fn lancet_capabilities_max_boundary_defaults_to_in() {
        let toml = format!(
            r#"{prefix}

[lancet.capabilities]
deny = ["session.events"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        // Boundary fail-closed is `in`, the opposite of `Trust::default()`.
        assert_eq!(
            profile.lancet.capabilities.as_ref().unwrap().max_boundary,
            Trust::In
        );
        assert_eq!(LancetCapabilitiesSection::default().max_boundary, Trust::In);
    }

    #[test]
    fn lancet_capabilities_absent_section_is_none() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert!(profile.lancet.capabilities.is_none());
    }

    #[test]
    fn lancet_capabilities_rejects_unknown_field() {
        let toml = format!(
            r#"{prefix}

[lancet.capabilities]
allow = ["session.events"]
"#,
            prefix = minimal_profile_prefix()
        );
        let err = parse(&toml).unwrap_err();
        assert!(err.to_string().contains("allow"), "got: {err}");
    }

    #[test]
    fn lancet_capabilities_rejects_unknown_boundary_value() {
        let toml = format!(
            r#"{prefix}

[lancet.capabilities]
max_boundary = "upper"
"#,
            prefix = minimal_profile_prefix()
        );
        assert!(parse(&toml).is_err());
    }

    #[test]
    fn lancet_capabilities_never_lands_in_extensions() {
        let toml = format!(
            r#"{prefix}

[lancet.capabilities]
deny = ["session.events"]
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
        assert!(profile.lancet.capabilities.is_some());
        assert!(
            profile.lancet.extensions.is_empty(),
            "typed sub-table must not fall through: {:?}",
            profile.lancet.extensions
        );
    }

    #[test]
    fn capability_admission_policy_lowers_enabled_tools_and_deny() {
        let toml = format!(
            r#"{prefix}

[tools]
enabled = ["noop.echo", "fetch.web"]

[lancet.capabilities]
deny = ["session.events"]
max_boundary = "ex"
"#,
            prefix = minimal_profile_prefix()
        );
        let policy = parse(&toml).unwrap().capability_admission_policy();
        assert_eq!(
            policy.enabled_tools,
            BTreeSet::from(["fetch.web".to_string(), "noop.echo".to_string()])
        );
        assert_eq!(
            policy.denied_capabilities,
            BTreeSet::from(["session.events".to_string()])
        );
        assert_eq!(policy.max_boundary, Trust::Ex);
    }

    #[test]
    fn capability_admission_policy_without_section_keeps_in_boundary() {
        let toml = format!(
            r#"{prefix}

[tools]
enabled = ["noop.echo"]
"#,
            prefix = minimal_profile_prefix()
        );
        let policy = parse(&toml).unwrap().capability_admission_policy();
        assert_eq!(
            policy.enabled_tools,
            BTreeSet::from(["noop.echo".to_string()])
        );
        assert!(policy.denied_capabilities.is_empty());
        assert_eq!(policy.max_boundary, Trust::In);
    }

    #[test]
    fn capability_admission_policy_without_enabled_tools_admits_nothing() {
        let policy = parse(minimal_profile_prefix())
            .unwrap()
            .capability_admission_policy();
        assert!(policy.enabled_tools.is_empty());
        assert_eq!(policy, CapabilityAdmissionPolicy::default());
    }
}
