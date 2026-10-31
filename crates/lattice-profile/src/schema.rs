// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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
    "capabilities",
    "workspace",
];

/// LANCET `[lancet]` profile section with typed policy sub-tables.
///
/// Sub-tables named in [`TYPED_LANCET_SUB_TABLES`] deserialize into typed fields
/// and are therefore strictly validated. Other future sub-tables remain in
/// `extensions`.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct LancetSection {
    #[serde(default)]
    pub project_root: Option<LancetProjectRootSection>,
    #[serde(default)]
    pub observation: Option<LancetObservationSection>,
    #[serde(default)]
    pub network: Option<LancetNetworkSection>,
    #[serde(default)]
    pub policy: Option<LancetPolicySection>,
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

/// `[lancet.observation]` — observation limits.
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
}

/// `[lancet.network]` — `fetch__web` host policy and response size cap.
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
/// catalog. An unknown name is a hard error at runtime construction, never a
/// warning: a deny typo that silently keeps a grant is fail-open.
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

fn default_max_boundary() -> Trust {
    Trust::In
}

impl<'de> Deserialize<'de> for LancetSection {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut raw = BTreeMap::<String, toml::Value>::deserialize(deserializer)?;
        if raw.remove("mode").is_some() {
            return Err(de::Error::custom(
                "`lancet.mode` is retired; Component Cell admission is always enforced",
            ));
        }
        if raw.remove("effects").is_some() {
            return Err(de::Error::custom(
                "`lancet.effects` is retired; use `[lancet.capabilities] deny`",
            ));
        }

        for (key, value) in &raw {
            if !matches!(value, toml::Value::Table(_)) {
                return Err(de::Error::custom(format!(
                    "unknown scalar field `lancet.{key}`; only typed or future sub-tables are supported"
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
            project_root,
            observation,
            network,
            policy,
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
    pub model: Option<String>,
    pub effort: Option<lattice_conversation::Effort>,
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
    /// 可选的协作式单 Turn deadline，单位为秒。
    ///
    /// `None` 表示使用协调层内置的 300 秒默认值。生效时长在 Turn 接受时
    /// 冻结；单调时钟 deadline 在持久 `TurnStarted` 发布后开始，限制
    /// provider 请求、重试等待与 Tool 分发，不中断必须完成的持久关闭。
    /// observer idle timeout 是独立边界。显式值在加载时按 1..=3600 校验。
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
    fn optional_model_and_reasoning_effort_are_strict() {
        for effort in ["off", "minimal", "low", "medium", "high", "xhigh", "max"] {
            let profile = parse(&format!(
                "schema_version=1\n[agent]\nname='test'\neffort='{effort}'\n[prompt]\nsystem='test'\n"
            ))
            .unwrap();
            let value = serde_json::to_value(profile).unwrap();
            assert_eq!(value["agent"]["model"], serde_json::Value::Null);
            assert_eq!(value["agent"]["effort"], effort);
        }
        for field in [
            "effort='default'",
            "effort='HIGH'",
            "effort=1",
            "effrot='high'",
        ] {
            assert!(
                parse(&format!(
                    "schema_version=1\n[agent]\nname='test'\nmodel='local/test'\n{field}\n[prompt]\nsystem='test'\n"
                ))
                .is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn valid_full_profile() {
        let toml = r#"
schema_version = 1

[agent]
name = "full"
model = "anthropic/claude-opus-4-6"
description = "A full-featured agent"

[prompt]
system_path = "prompt.md"

[tools]
enabled = ["observe__workspace", "mutate__create"]

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
        assert_eq!(tools.enabled, vec!["observe__workspace", "mutate__create"]);
        assert_eq!(profile.llm.retry.max_retries, 2);
        assert_eq!(profile.llm.retry.initial_delay_ms, 250);
        assert_eq!(profile.llm.retry.max_delay_ms, 1000);
    }

    // ── Top-level section rejection ───────────────────────────────────

    #[test]
    fn rejects_unsupported_top_level_sections() {
        let rows: &[(&str, &str, &str, &[&str])] = &[
            (
                "reject_unknown_top_level_section",
                "orchestrator",
                "some_field = \"bad\"",
                &["orchestrator"],
            ),
            (
                "reject_security_section",
                "security",
                "sandbox_mode = \"project\"",
                &["security"],
            ),
            (
                "reject_context_section",
                "context",
                "project_instructions = true",
                &["context"],
            ),
            (
                "reject_behavior_section",
                "behavior",
                "max_turns = 10",
                &["behavior"],
            ),
            (
                "reject_capabilities_section",
                "capabilities",
                "presets = [\"coding\"]",
                &["capabilities"],
            ),
            ("reject_handoff_section", "handoff", "enabled = true", &[]),
            // The Upper microVM `[workspace]` section was removed with the
            // Direct-Local migration (#40). Under `deny_unknown_fields`, legacy
            // profiles must fail to parse: the schema neither declares nor
            // requires `[workspace]`.
            (
                "reject_workspace_section",
                "workspace",
                "image = \"docker.io/library/python:3.12-slim\"",
                &["workspace", "unknown field"],
            ),
            (
                "reject_all_forbidden_top_level_sections/capabilities",
                "capabilities",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/handoff",
                "handoff",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/orchestrator",
                "orchestrator",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/plugins",
                "plugins",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/pipeline",
                "pipeline",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/subagents",
                "subagents",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/micro_agents",
                "micro_agents",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/memory",
                "memory",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/security",
                "security",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/context",
                "context",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/behavior",
                "behavior",
                "x = \"y\"",
                &[],
            ),
            (
                "reject_all_forbidden_top_level_sections/workspace",
                "workspace",
                "x = \"y\"",
                &[],
            ),
        ];
        for &(name, section, body, expected_error_contains_any) in rows {
            let toml = format!("{}\n[{section}]\n{body}\n", minimal_profile_prefix());
            let result = parse(&toml);
            if expected_error_contains_any.is_empty() {
                assert!(
                    result.is_err(),
                    "{name}: section [{section}] should be rejected"
                );
            } else {
                let err = result.expect_err(name);
                let message = err.to_string();
                assert!(
                    expected_error_contains_any
                        .iter()
                        .any(|text| message.contains(text)),
                    "{name}: got: {err}"
                );
            }
        }
    }

    // ── Tools section rejection ───────────────────────────────────────

    #[test]
    fn rejects_unsupported_tools_and_lancet_fields() {
        let rows: &[(&str, &str, &[&str])] = &[
            (
                "reject_tools_bundles",
                "[tools]\nbundles = [\"workspace\"]",
                &["bundles", "unknown field"],
            ),
            (
                "cep_rejects_scalar_extension_typos",
                "[lancet]\nmod = \"off\"",
                &["lancet.mod"],
            ),
            (
                "cep_observation_rejects_network_only_field",
                "[lancet.observation]\nmax_web_response_bytes = 1024",
                &["max_web_response_bytes"],
            ),
            (
                "lancet_workspace_unknown_field_rejected",
                "[lancet.workspace]\ndenied_write_patterns = [\".git/**\"]\nunexpected = true",
                &[],
            ),
            (
                "cep_policy_section_rejects_unknown_fields",
                "[lancet.policy]\nunknown = true",
                &["unknown"],
            ),
            (
                "lancet_capabilities_rejects_unknown_field",
                "[lancet.capabilities]\nallow = [\"session.events\"]",
                &["allow"],
            ),
            (
                "lancet_capabilities_rejects_unknown_boundary_value",
                "[lancet.capabilities]\nmax_boundary = \"upper\"",
                &[],
            ),
        ];
        for &(name, toml_suffix, expected_error_contains_any) in rows {
            let toml = format!("{}\n{toml_suffix}\n", minimal_profile_prefix());
            let result = parse(&toml);
            if expected_error_contains_any.is_empty() {
                assert!(result.is_err(), "{name}");
            } else {
                let err = result.expect_err(name);
                let message = err.to_string();
                assert!(
                    expected_error_contains_any
                        .iter()
                        .any(|text| message.contains(text)),
                    "{name}: got: {err}"
                );
            }
        }
    }

    // ── Agent field rejection ─────────────────────────────────────────

    #[test]
    fn rejects_unsupported_agent_fields() {
        let rows: &[(&str, &str, &[&str])] = &[
            (
                "reject_agent_preset_field",
                "preset = \"coding\"",
                &["preset"],
            ),
            ("reject_agent_type_code_agent", "type = \"code_agent\"", &[]),
            ("reject_agent_skippable", "skippable = true", &[]),
            ("reject_agent_tags", "tags = [\"coding\"]", &[]),
        ];
        for &(name, agent_field, expected_error_contains_any) in rows {
            let toml = format!(
                r#"
schema_version = 1

[agent]
name = "t"
model = "anthropic/claude-sonnet-4-6"
{agent_field}

[prompt]
system = "hi"
"#
            );
            let result = parse(&toml);
            if expected_error_contains_any.is_empty() {
                assert!(result.is_err(), "{name}");
            } else {
                let err = result.expect_err(name);
                let message = err.to_string();
                assert!(
                    expected_error_contains_any
                        .iter()
                        .any(|text| message.contains(text)),
                    "{name}: got: {err}"
                );
            }
        }
    }

    // ── CEP section ───────────────────────────────────────────────────

    fn minimal_profile_prefix() -> &'static str {
        r#"
schema_version = 1

[agent]
name = "t"
model = "anthropic/claude-sonnet-4-6"

[prompt]
system = "hi"
"#
    }

    #[test]
    fn cep_parses_typed_prof02_sub_tables() {
        let toml = format!(
            r#"{prefix}

[lancet]
[lancet.project_root]
root = "."

[lancet.observation]
max_single_read_bytes = 4096
max_turn_observation_bytes = 8192
max_grep_matches = 500
max_list_entries = 300
max_find_paths = 400

[lancet.network]
allow_public_https = true
allow_localhost = false
allowed_hosts = ["example.com"]
max_web_response_bytes = 2048
"#,
            prefix = minimal_profile_prefix()
        );
        let profile = parse(&toml).unwrap();
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

    /// `TYPED_LANCET_SUB_TABLES` drives the loader's case-variant rejection, so a
    /// field added to `LancetSection` without updating it would send that
    /// sub-table's case variants back to being silently swallowed by
    /// `extensions` (#121). Derived from serialization rather than restated, so
    /// the two cannot drift.
    #[test]
    fn typed_lancet_sub_tables_match_the_struct_fields() {
        let section = LancetSection {
            project_root: Some(LancetProjectRootSection::default()),
            observation: Some(LancetObservationSection::default()),
            network: Some(LancetNetworkSection::default()),
            policy: Some(LancetPolicySection::default()),
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

    // ── [llm.timeout_secs] ────────────────────────────────────────────

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
    fn capability_admission_policy_without_section_keeps_in_boundary() {
        let toml = format!(
            r#"{prefix}

[tools]
enabled = ["noop__echo"]
"#,
            prefix = minimal_profile_prefix()
        );
        let policy = parse(&toml).unwrap().capability_admission_policy();
        assert_eq!(
            policy.enabled_tools,
            BTreeSet::from(["noop__echo".to_string()])
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
