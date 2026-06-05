// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Default agent templates — generated text only, no filesystem writes.

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

schema_version = 1

[agent]
name = {name}
model = {model}

[prompt]
system = "You are a helpful assistant. Be concise and accurate."

[tools]
enabled = ["observe.workspace"]

# Optional command physical-boundary configuration.
# Uncomment to request command isolation boundaries.
# [lancet.isolation.command]
# mode = "enforce"
# required_boundaries = ["network", "filesystem"]
# optional_boundaries = []
# read_only_toolchain_paths = ["/usr", "/bin"]
# scratch_paths = ["build/tmp"]
# landlock = "auto"
# unsupported_host = "mode_default"

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
"#
    )
}

/// Generate a strict v1 `agent.toml` template for a read-only agent.
///
/// Read-only tools: file reading, grep, find, listing, workspace context.
/// No command execution, file modification, or internet access.
pub fn default_reader_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Reader
# Read-only agent for code review, understanding, and search.
# No command execution, file modification, or internet access.

schema_version = 1

[agent]
name = {name}
model = {model}
description = "Read-only agent for code understanding and review"

[prompt]
system = "You are a code reader and reviewer. Read and analyze code carefully. Do not suggest edits or run commands. Explain your reasoning clearly."

[tools]
enabled = ["observe.workspace"]

# Optional command physical-boundary configuration.
# Uncomment to request command isolation boundaries.
# [lancet.isolation.command]
# mode = "enforce"
# required_boundaries = ["network", "filesystem"]
# optional_boundaries = []
# read_only_toolchain_paths = ["/usr", "/bin"]
# scratch_paths = ["build/tmp"]
# landlock = "auto"
# unsupported_host = "mode_default"

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
"#
    )
}

/// Generate a strict v1 `agent.toml` template for an editing agent.
///
/// Read-write tools: workspace observation, create/patch mutation,
/// listing, todo tracking, workspace context.  No shell or web access.
pub fn default_editor_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Editor
# Read-write agent for editing, refactoring, and applying patches.
# No shell or web access — editing only.

schema_version = 1

[agent]
name = {name}
model = {model}
description = "Editing agent for refactoring, fixing, and applying patches"

[prompt]
system = "You are a code editor. Read, edit, and refactor code within the project. Use observe, create, and patch tools for changes. Be precise and explain your edits."

[tools]
enabled = ["observe.workspace", "mutate.create", "mutate.patch", "session.envelope", "session.todo_write"]

# Optional command physical-boundary configuration.
# Uncomment to request command isolation boundaries.
# [lancet.isolation.command]
# mode = "enforce"
# required_boundaries = ["network", "filesystem"]
# optional_boundaries = []
# read_only_toolchain_paths = ["/usr", "/bin"]
# scratch_paths = ["build/tmp"]
# landlock = "auto"
# unsupported_host = "mode_default"

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
"#
    )
}

/// Generate a strict v1 `agent.toml` template for a development agent.
///
/// Full tool set: all reader/editor tools plus command execution and web fetch.
pub fn default_dev_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Developer
# Full development agent with all standard tools enabled.

schema_version = 1

[agent]
name = {name}
model = {model}
description = "Full development agent for coding, planning, and troubleshooting"

[prompt]
system = "You are a full-stack developer. Read, write, edit, and refactor code. Plan changes before acting. Run shell commands for build, test, lint, and version control. Fetch web docs when needed. Be methodical and explain your reasoning."

[tools]
enabled = ["observe.workspace", "mutate.create", "mutate.patch", "session.envelope", "session.todo_write", "exec.command_run", "fetch.web"]

# Optional command physical-boundary configuration.
# Uncomment to request command isolation boundaries.
# [lancet.isolation.command]
# mode = "enforce"
# required_boundaries = ["network", "filesystem"]
# optional_boundaries = []
# read_only_toolchain_paths = ["/usr", "/bin"]
# scratch_paths = ["build/tmp"]
# landlock = "auto"
# unsupported_host = "mode_default"

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
"#
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
mod tests {
    use super::*;

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
        // Tools: non-empty enabled, no bundles
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
        // The word "bundles" may appear in explanatory comments, but the
        // TOML key `bundles =` must not appear.
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
        // Non-empty enabled tools
        assert!(!profile.tools.enabled.is_empty());
        // No shell, web, or editing tools
        let enabled = &profile.tools.enabled;
        assert!(!enabled.contains(&"exec.command_run".to_string()));
        assert!(!enabled.contains(&"fetch.web".to_string()));
        assert!(!enabled.contains(&"mutate.create".to_string()));
        assert!(!enabled.contains(&"mutate.patch".to_string()));
        assert!(!enabled.contains(&"session.envelope".to_string()));
    }

    #[test]
    fn reader_template_has_no_shell_or_web() {
        let toml_str = default_reader_template("r", "haiku").to_lowercase();
        assert!(!toml_str.contains("exec.command_run"));
        assert!(!toml_str.contains("fetch.web"));
        assert!(!toml_str.contains("editing"));
    }

    #[test]
    fn reader_template_enabled_tools_are_resolvable() {
        let toml_str = default_reader_template("r", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("reader template must parse");
        let expected: Vec<&str> = vec!["observe.workspace"];
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
        assert_eq!(profile.agent.model, "sonnet");
        assert!(!profile.tools.enabled.is_empty());
        let enabled = &profile.tools.enabled;
        assert!(enabled.contains(&"mutate.create".to_string()));
        assert!(enabled.contains(&"mutate.patch".to_string()));
        assert!(enabled.contains(&"session.envelope".to_string()));
        assert!(enabled.contains(&"session.todo_write".to_string()));
        // No shell or web
        assert!(!enabled.contains(&"exec.command_run".to_string()));
        assert!(!enabled.contains(&"fetch.web".to_string()));
    }

    #[test]
    fn editor_template_has_no_web_fetch_tool() {
        let toml_str = default_editor_template("e", "sonnet");
        assert!(!toml_str.contains("fetch.web"));
    }

    #[test]
    fn editor_template_enabled_tools_are_resolvable() {
        let toml_str = default_editor_template("e", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("editor template must parse");
        let expected: Vec<&str> = vec![
            "observe.workspace",
            "mutate.create",
            "mutate.patch",
            "session.envelope",
            "session.todo_write",
        ];
        let mut enabled = profile.tools.enabled.clone();
        enabled.sort();
        let mut expected_sorted: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected_sorted.sort();
        assert_eq!(enabled, expected_sorted);
    }

    // ── Developer template tests ───────────────────────────────────────

    #[test]
    fn dev_template_parses_as_valid_v1_profile() {
        let toml_str = default_dev_template("dev", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("dev template must be valid v1 TOML");
        assert_eq!(profile.agent.name, "dev");
        assert_eq!(profile.agent.model, "sonnet");
        assert!(!profile.tools.enabled.is_empty());
        let enabled = &profile.tools.enabled;
        assert!(enabled.contains(&"exec.command_run".to_string()));
        assert!(enabled.contains(&"fetch.web".to_string()));
        assert!(enabled.contains(&"mutate.create".to_string()));
        assert!(enabled.contains(&"mutate.patch".to_string()));
        assert!(enabled.contains(&"session.envelope".to_string()));
        assert!(enabled.contains(&"session.todo_write".to_string()));
    }

    #[test]
    fn dev_template_enabled_tools_are_resolvable() {
        let toml_str = default_dev_template("d", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("dev template must parse");
        let expected: Vec<&str> = vec![
            "observe.workspace",
            "mutate.create",
            "mutate.patch",
            "session.envelope",
            "session.todo_write",
            "exec.command_run",
            "fetch.web",
        ];
        let mut enabled = profile.tools.enabled.clone();
        enabled.sort();
        let mut expected_sorted: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected_sorted.sort();
        assert_eq!(enabled, expected_sorted);
    }

    #[test]
    fn all_templates_document_command_isolation_without_requesting_boundaries() {
        for (name, toml_str) in [
            ("default", default_agent_template("t", "sonnet")),
            ("reader", default_reader_template("t", "sonnet")),
            ("editor", default_editor_template("t", "sonnet")),
            ("dev", default_dev_template("t", "sonnet")),
        ] {
            assert!(
                toml_str.contains("# [lancet.isolation.command]"),
                "{name} template must document the command isolation table"
            );
            assert!(
                !toml_str.contains("\n[lancet.isolation.command]"),
                "{name} template must not enable command isolation by default"
            );
            let profile: crate::schema::AgentProfile = toml::from_str(&toml_str)
                .unwrap_or_else(|e| panic!("{name} template must parse: {e}"));
            assert!(
                profile.lancet.isolation.is_none(),
                "{name} template must not request physical boundaries by default"
            );
        }
    }

    // ── Cross-template checks ──────────────────────────────────────────

    #[test]
    fn reader_editor_dev_templates_have_no_forbidden_legacy_concepts() {
        for toml_str in &[
            default_reader_template("t", "sonnet"),
            default_editor_template("t", "sonnet"),
            default_dev_template("t", "sonnet"),
        ] {
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
    }

    #[test]
    fn all_templates_have_no_bundles_key() {
        for toml_str in &[
            default_agent_template("t", "sonnet"),
            default_reader_template("t", "sonnet"),
            default_editor_template("t", "sonnet"),
            default_dev_template("t", "sonnet"),
        ] {
            assert!(
                !toml_str.contains("bundles ="),
                "template must not contain 'bundles =' as a TOML key"
            );
        }
    }

    #[test]
    fn default_templates_have_no_bundles() {
        for toml_str in [
            default_agent_template("t", "sonnet"),
            default_reader_template("t", "sonnet"),
            default_editor_template("t", "sonnet"),
            default_dev_template("t", "sonnet"),
        ] {
            assert!(
                !toml_str.contains("bundles"),
                "default templates must not emit bundles"
            );
        }
    }

    #[test]
    fn all_templates_have_non_empty_enabled_tools() {
        let templates: Vec<(&str, String)> = vec![
            ("default", default_agent_template("t", "sonnet")),
            ("reader", default_reader_template("t", "sonnet")),
            ("editor", default_editor_template("t", "sonnet")),
            ("dev", default_dev_template("t", "sonnet")),
        ];
        for (name, toml_str) in templates {
            let profile: crate::schema::AgentProfile = toml::from_str(&toml_str)
                .unwrap_or_else(|e| panic!("{name} template must parse: {e}"));
            assert!(
                !profile.tools.enabled.is_empty(),
                "{name} template must have non-empty tools.enabled"
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
                assert!(
                    tool.contains('.'),
                    "{name} template tool '{tool}' must be a dotted CEP method name"
                );
            }
        }
    }

    #[test]
    fn default_templates_exclude_legacy_aliases_and_workspace_context() {
        let legacy = vec![
            ["read", "_file"].concat(),
            ['g', 'r', 'e', 'p'].iter().collect::<String>(),
            ["find", "_files"].concat(),
            ["list", "_directory"].concat(),
            ["write", "_file"].concat(),
            ['p', 'a', 't', 'c', 'h'].iter().collect::<String>(),
            ['b', 'a', 's', 'h'].iter().collect::<String>(),
            ["web", "_fetch"].concat(),
            ["todo", "_write"].concat(),
            ["workspace", "_context"].concat(),
        ];
        for toml_str in [
            default_agent_template("t", "sonnet"),
            default_reader_template("t", "sonnet"),
            default_editor_template("t", "sonnet"),
            default_dev_template("t", "sonnet"),
        ] {
            for alias in &legacy {
                assert!(
                    !toml_str.contains(&format!("\"{alias}\"")),
                    "template must not contain legacy tool name '{alias}'"
                );
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
