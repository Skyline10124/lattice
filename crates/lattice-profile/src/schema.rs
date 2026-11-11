// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

use serde::Deserialize;

/// 已校验的 v1 `agent.toml` 配置，所有标量字段均已构造为领域类型。
///
/// 仅接受 `schema_version = 1`、`[agent]`、`[prompt]` 与可选的 `[llm]`（含 `[llm.retry]`）；
/// `[lancet]`、`[tools]` 及其他已移除或禁止的顶层节在加载预扫描时拒绝。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProfile {
    pub agent: AgentSection,
    pub llm: LlmSection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSection {
    pub name: lattice_session::AgentName,
    pub model: Option<lattice_conversation::ModelRef>,
    pub effort: Option<lattice_conversation::Effort>,
    pub description: Option<String>,
}

/// LLM provider execution settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmSection {
    /// 协作式单 Turn deadline；`agent.toml` 未显式配置时取
    /// [`lattice_session::TurnTimeout::default()`]。生效时长在 Turn 接受时冻结；
    /// 单调时钟 deadline 在接受事务提交后开始，限制 provider 请求、重试等待与
    /// Tool 分发，不中断必须完成的持久关闭。observer idle timeout 是独立边界。
    pub timeout_secs: lattice_session::TurnTimeout,
    pub retry: lattice_session::RetryPolicy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAgentProfile {
    pub schema_version: u64,
    pub agent: RawAgentSection,
    pub prompt: RawPromptSection,
    #[serde(default)]
    pub llm: RawLlmSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawAgentSection {
    pub name: String,
    pub model: Option<String>,
    pub effort: Option<lattice_conversation::Effort>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawPromptSection {
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub system_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLlmSection {
    /// 可选的协作式单 Turn deadline，单位为秒。
    ///
    /// `None` 表示使用 [`lattice_session::TurnTimeout::default()`]。显式值在加载时按
    /// `1..=86400` 构造 [`lattice_session::TurnTimeout`]。
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub retry: RawRetryPolicy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRetryPolicy {
    #[serde(default = "default_llm_retry_max_retries")]
    pub max_retries: u32,
    #[serde(default = "default_llm_retry_initial_delay_ms")]
    pub initial_delay_ms: u64,
    #[serde(default = "default_llm_retry_max_delay_ms")]
    pub max_delay_ms: u64,
}

impl Default for RawRetryPolicy {
    fn default() -> Self {
        let default = lattice_session::RetryPolicy::default();
        Self {
            max_retries: default.max_retries(),
            initial_delay_ms: default.initial_delay_ms(),
            max_delay_ms: default.max_delay_ms(),
        }
    }
}

fn default_llm_retry_max_retries() -> u32 {
    lattice_session::RetryPolicy::default().max_retries()
}

fn default_llm_retry_initial_delay_ms() -> u64 {
    lattice_session::RetryPolicy::default().initial_delay_ms()
}

fn default_llm_retry_max_delay_ms() -> u64 {
    lattice_session::RetryPolicy::default().max_delay_ms()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiscoveryPaths, LoadedAgentProfile, ProfileError, load_agent_profile};
    use lattice_conversation::{Effort, ModelRef};
    use lattice_session::{AgentName, RetryPolicy, TurnTimeout};
    use std::os::unix::fs::MetadataExt;

    fn load_named(name: &str, toml_str: &str) -> Result<LoadedAgentProfile, ProfileError> {
        let dir = tempfile::tempdir().unwrap();
        let uid = std::fs::symlink_metadata(dir.path()).unwrap().uid();
        let paths = DiscoveryPaths {
            project_root: dir.path().join("project"),
            global_agents_dir: dir.path().join("global-agents"),
        };
        let package = paths.project_agents_dir().join(name);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("agent.toml"), toml_str).unwrap();
        load_agent_profile(&AgentName::parse(name).unwrap(), &paths, uid)
    }

    fn parse(toml_str: &str) -> Result<AgentProfile, ProfileError> {
        load_named("t", toml_str).map(|loaded| loaded.profile)
    }

    #[test]
    fn optional_model_and_reasoning_effort_are_strict() {
        for (raw, expected) in [
            ("off", Effort::Off),
            ("minimal", Effort::Minimal),
            ("low", Effort::Low),
            ("medium", Effort::Medium),
            ("high", Effort::High),
            ("xhigh", Effort::Xhigh),
            ("max", Effort::Max),
        ] {
            let loaded = load_named(
                "test",
                &format!(
                    "schema_version=1\n[agent]\nname='test'\neffort='{raw}'\n[prompt]\nsystem='test'\n"
                ),
            )
            .unwrap();
            assert_eq!(loaded.profile.agent.model, None);
            assert_eq!(loaded.profile.agent.effort, Some(expected));
        }
        for field in [
            "effort='default'",
            "effort='HIGH'",
            "effort=1",
            "effrot='high'",
        ] {
            assert!(
                load_named(
                    "test",
                    &format!(
                        "schema_version=1\n[agent]\nname='test'\nmodel='local/test'\n{field}\n[prompt]\nsystem='test'\n"
                    ),
                )
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

[llm.retry]
max_retries = 2
initial_delay_ms = 250
max_delay_ms = 1000
"#;
        let dir = tempfile::tempdir().unwrap();
        let uid = std::fs::symlink_metadata(dir.path()).unwrap().uid();
        let paths = DiscoveryPaths {
            project_root: dir.path().join("project"),
            global_agents_dir: dir.path().join("global-agents"),
        };
        let package = paths.project_agents_dir().join("full");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("prompt.md"), "Prompt from file.").unwrap();
        std::fs::write(package.join("agent.toml"), toml).unwrap();
        let loaded = load_agent_profile(&AgentName::parse("full").unwrap(), &paths, uid).unwrap();
        let profile = &loaded.profile;
        assert_eq!(profile.agent.name, "full");
        assert_eq!(
            profile.agent.model,
            Some(ModelRef::parse("anthropic/claude-opus-4-6").unwrap())
        );
        assert_eq!(
            profile.agent.description.as_deref(),
            Some("A full-featured agent")
        );
        assert_eq!(loaded.system_prompt, "Prompt from file.");
        assert_eq!(profile.llm.timeout_secs, TurnTimeout::default());
        assert_eq!(profile.llm.retry, RetryPolicy::new(2, 250, 1000).unwrap());
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

    // ── [llm] defaults and retry policy ───────────────────────────────

    #[test]
    fn llm_retry_defaults_when_section_absent() {
        let profile = parse(minimal_profile_prefix()).unwrap();
        assert_eq!(profile.llm.retry, RetryPolicy::default());
        assert_eq!(profile.llm.timeout_secs, TurnTimeout::default());
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
        assert_eq!(profile.llm.retry, RetryPolicy::new(0, 1, 2).unwrap());
    }
}
