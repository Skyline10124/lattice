// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Default agent templates — generated text only, no filesystem writes.

const WORKSPACE_UPGRADE_NOTE: &str = r#"# Upper-backed observe/mutate/exec tools are intentionally absent here.
# To enable them:
#   1. Add a top-level [workspace] section with a real fully-qualified OCI image
#      or an existing project template name you have already built.
#   2. Explicitly add the desired tool names under [tools].enabled
#      (observe.workspace, mutate.create, mutate.edit, mutate.batch, exec.command_run).
# Do not guess image/template values in generated profiles.
"#;

/// Generate a strict v1 `agent.toml` template for a new agent.
///
/// The template is parseable by the strict schema and contains only supported
/// sections: `[agent]`, `[prompt]`, `[tools]`. Tools are enabled explicitly by
/// name.
pub fn default_agent_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1)
# Auto-generated template — edit for your project's role, prompt, and tools.
#
# Sections:
#   [agent]  — identity (name, model, optional description)
#   [prompt] — system prompt (inline `system` or file `system_path`, not both)
#   [tools]  — explicitly enabled tool names
#   [llm.retry] — provider stream-open retry policy
{workspace_note}
schema_version = 1

[agent]
name = {name}
model = {model}

[prompt]
system = "You are a helpful assistant. Be concise and accurate."

[tools]
enabled = ["session.todo_write"]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
"#,
        workspace_note = WORKSPACE_UPGRADE_NOTE
    )
}

/// Generate a strict v1 `agent.toml` template for a read-only agent.
///
/// Host-only read surfaces: audit query/replay. No command execution, file
/// modification, or workspace mutation without an explicit `[workspace]` section.
pub fn default_reader_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Reader
# Read-only host agent for code review and audit inspection.
# No command execution, file modification, or Upper-backed workspace tools.
{workspace_note}
schema_version = 1

[agent]
name = {name}
model = {model}
description = "Read-only agent for code understanding and review"

[prompt]
system = "You are a code reader and reviewer. Read and analyze code carefully. Do not suggest edits or run commands. Explain your reasoning clearly."

[tools]
enabled = ["observe.audit", "observe.audit_replay"]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
"#,
        workspace_note = WORKSPACE_UPGRADE_NOTE
    )
}

/// Generate a strict v1 `agent.toml` template for an editing agent.
///
/// Host-only planning tools until `[workspace]` is configured explicitly.
pub fn default_editor_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Editor
# Host-only editing planner until an explicit [workspace] section is added.
# No shell or web access — configure workspace + tools to enable Upper edits.
{workspace_note}
schema_version = 1

[agent]
name = {name}
model = {model}
description = "Editing agent for refactoring and fixing code"

[prompt]
system = "You are a code editor. Read, edit, and refactor code within the project. Use observe, create, edit, and batch tools for changes. Be precise and explain your edits."

[tools]
enabled = ["session.todo_write"]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
"#,
        workspace_note = WORKSPACE_UPGRADE_NOTE
    )
}

/// Generate a strict v1 `agent.toml` template for a development agent.
///
/// Host-only fetch/planning tools until `[workspace]` is configured explicitly.
pub fn default_dev_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Developer
# Host-only development helper until an explicit [workspace] section is added.
{workspace_note}
schema_version = 1

[agent]
name = {name}
model = {model}
description = "Full development agent for coding, planning, and troubleshooting"

[prompt]
system = "You are a full-stack developer. Read, write, edit, and refactor code. Plan changes before acting. Run shell commands for build, test, lint, and version control. Fetch web docs when needed. Be methodical and explain your reasoning."

[tools]
enabled = ["session.todo_write", "fetch.web"]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
"#,
        workspace_note = WORKSPACE_UPGRADE_NOTE
    )
}

fn toml_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            ch if ch.is_control() => {
                escaped.push_str(&format!("\\u{:04X}", ch as u32));
            }
            ch => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
const UPPER_BACKED_TOOLS: &[&str] = &[
    "observe.workspace",
    "mutate.create",
    "mutate.edit",
    "mutate.batch",
    "exec.command_run",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn is_catalog_method(name: &str) -> bool {
        matches!(
            name,
            "observe.workspace"
                | "mutate.create"
                | "mutate.edit"
                | "mutate.batch"
                | "exec.command_run"
                | "fetch.web"
                | "session.todo_write"
                | "observe.audit"
                | "observe.audit_replay"
        )
    }

    // ── Basic parse tests ─────────────────────────────────────────────

    #[test]
    fn template_parses_as_valid_v1_profile() {
        let toml_str = default_agent_template("test-agent", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("template must be valid v1 TOML");
        assert_eq!(profile.schema_version, 1);
        assert_eq!(profile.agent.name, "test-agent");
        assert_eq!(profile.agent.model, "sonnet");
        assert!(profile.agent.description.is_none());
        assert_eq!(
            profile.prompt.system.as_deref(),
            Some("You are a helpful assistant. Be concise and accurate.")
        );
        assert!(profile.prompt.system_path.is_none());
        assert!(
            !profile.tools.enabled.is_empty(),
            "default template must enable tools"
        );
    }

    #[test]
    fn template_escapes_name_and_model_as_toml_strings() {
        let toml_str = default_agent_template("weird\"agent\nname", "model\\with\ttabs");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("escaped template must parse");
        assert_eq!(profile.agent.name, "weird\"agent\nname");
        assert_eq!(profile.agent.model, "model\\with\ttabs");
    }

    #[test]
    fn template_contains_auto_generated_comment() {
        let toml_str = default_agent_template("t", "sonnet");
        let lower = toml_str.to_lowercase();
        assert!(
            lower.contains("auto-generated"),
            "must include auto-generated notice"
        );
        assert!(toml_str.contains("role"), "must mention editing role");
        assert!(toml_str.contains("prompt"), "must mention prompt");
        assert!(toml_str.contains("tools"), "must mention tools");
    }

    #[test]
    fn template_documents_workspace_upgrade_without_placeholder_image() {
        for toml_str in [
            default_agent_template("t", "sonnet"),
            default_reader_template("t", "sonnet"),
            default_editor_template("t", "sonnet"),
            default_dev_template("t", "sonnet"),
        ] {
            assert!(toml_str.contains("[workspace]"));
            assert!(toml_str.contains("observe.workspace"));
            assert!(!toml_str.contains("[workspace]\nimage ="));
            assert!(!toml_str.contains("docker.io/"));
        }
    }

    #[test]
    fn template_has_only_allowed_top_level_sections() {
        let toml_str = default_agent_template("t", "sonnet");
        let allowed = [
            "schema_version",
            "[agent]",
            "[prompt]",
            "[tools]",
            "[llm.retry]",
        ];
        for section in &allowed {
            assert!(
                toml_str.contains(section),
                "missing allowed section: {section}"
            );
        }
    }

    #[test]
    fn template_has_no_bundles_key() {
        let toml_str = default_agent_template("t", "sonnet");
        assert!(
            !toml_str.contains("bundles ="),
            "template must not contain 'bundles =' as a TOML key"
        );
    }

    #[test]
    fn template_has_no_forbidden_legacy_concepts() {
        let toml_str = default_agent_template("t", "sonnet");
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
            "[security]",
            "[context]",
            "[behavior]",
            "skippable",
            "tags",
            "behavior_type",
            "confidence_threshold",
            "enforce_sandbox",
            "code_agent",
            "bundles =",
            "sandbox_mode",
        ];
        for f in &forbidden {
            assert!(!toml_str.contains(f), "template must not contain '{f}'");
        }
    }

    #[test]
    fn template_does_not_create_files() {
        let before: Vec<_> = std::fs::read_dir(".").unwrap().collect();
        let _ = default_agent_template("test", "sonnet");
        let after: Vec<_> = std::fs::read_dir(".").unwrap().collect();
        assert_eq!(before.len(), after.len(), "template must not create files");
    }

    #[test]
    fn generated_profiles_exclude_upper_backed_tools_without_workspace() {
        for (name, toml_str) in [
            ("default", default_agent_template("t", "sonnet")),
            ("reader", default_reader_template("t", "sonnet")),
            ("editor", default_editor_template("t", "sonnet")),
            ("dev", default_dev_template("t", "sonnet")),
        ] {
            let profile: crate::schema::AgentProfile = toml::from_str(&toml_str)
                .unwrap_or_else(|e| panic!("{name} template must parse: {e}"));
            assert!(
                profile.workspace.is_none(),
                "{name} must not embed workspace"
            );
            for tool in &profile.tools.enabled {
                assert!(
                    !UPPER_BACKED_TOOLS.contains(&tool.as_str()),
                    "{name} must not enable Upper-backed tool {tool}"
                );
                assert!(
                    is_catalog_method(tool),
                    "{name} tool {tool} must resolve through catalog"
                );
            }
        }
    }

    // ── Reader template tests ──────────────────────────────────────────

    #[test]
    fn reader_template_parses_as_valid_v1_profile() {
        let toml_str = default_reader_template("reader", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("reader template must be valid v1 TOML");
        assert_eq!(profile.agent.name, "reader");
        assert_eq!(profile.agent.model, "sonnet");
        assert_eq!(
            profile.agent.description.as_deref(),
            Some("Read-only agent for code understanding and review")
        );
        assert!(!profile.tools.enabled.is_empty());
    }

    #[test]
    fn reader_template_enabled_tools_are_resolvable() {
        let toml_str = default_reader_template("r", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("reader template must parse");
        let expected = ["observe.audit", "observe.audit_replay"];
        let mut enabled = profile.tools.enabled.clone();
        enabled.sort();
        let mut expected_sorted: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected_sorted.sort();
        assert_eq!(enabled, expected_sorted);
    }

    // ── Editor template tests ──────────────────────────────────────────

    #[test]
    fn editor_template_parses_as_valid_v1_profile() {
        let toml_str = default_editor_template("editor", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("editor template must be valid v1 TOML");
        assert_eq!(profile.agent.name, "editor");
        assert!(!profile.tools.enabled.is_empty());
        assert!(
            profile
                .tools
                .enabled
                .contains(&"session.todo_write".to_string())
        );
    }

    #[test]
    fn editor_template_has_no_web_fetch_tool() {
        let toml_str = default_editor_template("e", "sonnet");
        assert!(!toml_str.contains("fetch.web"));
    }

    // ── Developer template tests ───────────────────────────────────────

    #[test]
    fn dev_template_parses_as_valid_v1_profile() {
        let toml_str = default_dev_template("dev", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("dev template must be valid v1 TOML");
        assert!(profile.tools.enabled.contains(&"fetch.web".to_string()));
        assert!(
            profile
                .tools
                .enabled
                .contains(&"session.todo_write".to_string())
        );
    }

    #[test]
    fn dev_template_enabled_tools_are_resolvable() {
        let toml_str = default_dev_template("d", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("dev template must parse");
        let expected = ["fetch.web", "session.todo_write"];
        let mut enabled = profile.tools.enabled.clone();
        enabled.sort();
        let mut expected_sorted: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected_sorted.sort();
        assert_eq!(enabled, expected_sorted);
    }

    #[test]
    fn reader_editor_dev_templates_have_no_forbidden_legacy_concepts() {
        for toml_str in &[
            default_reader_template("t", "sonnet"),
            default_editor_template("t", "sonnet"),
            default_dev_template("t", "sonnet"),
        ] {
            let forbidden = ["bundles =", "sandbox_mode", "[capabilities]"];
            for f in &forbidden {
                assert!(!toml_str.contains(f), "template must not contain '{f}'");
            }
        }
    }

    #[test]
    fn all_templates_have_no_bundles_key() {
        for toml_str in &[
            default_agent_template("t", "sonnet"),
            default_reader_template("t", "sonnet"),
            default_editor_template("t", "sonnet"),
            default_dev_template("t", "sonnet"),
        ] {
            assert!(!toml_str.contains("bundles ="));
        }
    }

    #[test]
    fn all_templates_have_non_empty_enabled_tools() {
        for (name, toml_str) in [
            ("default", default_agent_template("t", "sonnet")),
            ("reader", default_reader_template("t", "sonnet")),
            ("editor", default_editor_template("t", "sonnet")),
            ("dev", default_dev_template("t", "sonnet")),
        ] {
            let profile: crate::schema::AgentProfile = toml::from_str(&toml_str)
                .unwrap_or_else(|e| panic!("{name} template must parse: {e}"));
            assert!(
                !profile.tools.enabled.is_empty(),
                "{name} must enable tools"
            );
        }
    }

    #[test]
    fn default_templates_use_only_dotted_catalog_names() {
        for (name, toml_str) in [
            ("default", default_agent_template("t", "sonnet")),
            ("reader", default_reader_template("t", "sonnet")),
            ("editor", default_editor_template("t", "sonnet")),
            ("dev", default_dev_template("t", "sonnet")),
        ] {
            let profile: crate::schema::AgentProfile = toml::from_str(&toml_str)
                .unwrap_or_else(|e| panic!("{name} template must parse: {e}"));
            for tool in &profile.tools.enabled {
                assert!(tool.contains('.'), "{name} tool '{tool}' must be dotted");
            }
        }
    }

    #[test]
    fn reader_editor_dev_templates_do_not_create_files() {
        let before: Vec<_> = std::fs::read_dir(".").unwrap().collect();
        let _ = default_reader_template("t", "sonnet");
        let _ = default_editor_template("t", "sonnet");
        let _ = default_dev_template("t", "sonnet");
        let after: Vec<_> = std::fs::read_dir(".").unwrap().collect();
        assert_eq!(before.len(), after.len(), "templates must not create files");
    }
}
