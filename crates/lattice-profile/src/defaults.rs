//! v1.1 default agent templates — generated text only, no filesystem writes.

/// Generate a strict v1 `agent.toml` template for a new agent.
///
/// The template is parseable by the strict schema and contains only v1.1
/// sections: `[agent]`, `[prompt]`, `[tools]`.  Tools are enabled
/// explicitly by name.
pub fn default_agent_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1.1)
# Auto-generated template — edit for your project's role, prompt, and tools.
#
# Sections:
#   [agent]  — identity (name, model, optional description)
#   [prompt] — system prompt (inline `system` or file `system_path`, not both)
#   [tools]  — explicitly enabled tool names

schema_version = 1

[agent]
name = {name}
model = {model}

[prompt]
system = "You are a helpful assistant. Be concise and accurate."

[tools]
enabled = ["read_file", "read_files", "grep", "find_files", "list_directory", "workspace_context"]
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
        r#"# LATTICE Agent Profile (v1.1) — Reader
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
enabled = ["read_file", "read_files", "grep", "find_files", "list_directory", "workspace_context"]
"#
    )
}

/// Generate a strict v1 `agent.toml` template for an editing agent.
///
/// Read-write tools: file reading, editing (write/patch), grep, find,
/// listing, todo tracking, workspace context.  No shell or web access.
pub fn default_editor_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1.1) — Editor
# Read-write agent for editing, refactoring, and applying patches.
# No shell or web access — editing only.

schema_version = 1

[agent]
name = {name}
model = {model}
description = "Editing agent for refactoring, fixing, and applying patches"

[prompt]
system = "You are a code editor. Read, edit, and refactor code within the project. Use read, write, and patch tools for changes. Be precise and explain your edits."

[tools]
enabled = ["read_file", "read_files", "write_file", "patch", "grep", "find_files", "list_directory", "todo_write", "workspace_context"]
"#
    )
}

/// Generate a strict v1 `agent.toml` template for a development agent.
///
/// Full tool set: all reader/editor tools plus bash and web_search.
pub fn default_dev_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1.1) — Developer
# Full development agent with all standard tools enabled.

schema_version = 1

[agent]
name = {name}
model = {model}
description = "Full development agent for coding, planning, and troubleshooting"

[prompt]
system = "You are a full-stack developer. Read, write, edit, and refactor code. Plan changes before acting. Run shell commands for build, test, lint, and version control. Search the web when documentation is needed. Be methodical and explain your reasoning."

[tools]
enabled = ["read_file", "read_files", "write_file", "patch", "grep", "find_files", "list_directory", "todo_write", "workspace_context", "bash", "web_search"]
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
        let allowed = ["schema_version", "[agent]", "[prompt]", "[tools]"];
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
            "max_retries",
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
        assert!(!enabled.contains(&"bash".to_string()));
        assert!(!enabled.contains(&"web_search".to_string()));
        assert!(!enabled.contains(&"write_file".to_string()));
        assert!(!enabled.contains(&"patch".to_string()));
    }

    #[test]
    fn reader_template_has_no_shell_or_web() {
        let toml_str = default_reader_template("r", "haiku").to_lowercase();
        assert!(!toml_str.contains("shell"));
        assert!(!toml_str.contains("web"));
        assert!(!toml_str.contains("editing"));
        assert!(!toml_str.contains("bash"));
    }

    #[test]
    fn reader_template_enabled_tools_are_resolvable() {
        let toml_str = default_reader_template("r", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("reader template must parse");
        let expected: Vec<&str> = vec![
            "read_file",
            "read_files",
            "grep",
            "find_files",
            "list_directory",
            "workspace_context",
        ];
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
        assert!(enabled.contains(&"write_file".to_string()));
        assert!(enabled.contains(&"patch".to_string()));
        assert!(enabled.contains(&"todo_write".to_string()));
        // No shell or web
        assert!(!enabled.contains(&"bash".to_string()));
        assert!(!enabled.contains(&"web_search".to_string()));
    }

    #[test]
    fn editor_template_has_no_web_search_tool() {
        let toml_str = default_editor_template("e", "sonnet");
        // The word "web" may appear in comments, but the tool name
        // "web_search" must not appear as an enabled tool.
        assert!(!toml_str.contains("web_search"));
    }

    #[test]
    fn editor_template_enabled_tools_are_resolvable() {
        let toml_str = default_editor_template("e", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("editor template must parse");
        let expected: Vec<&str> = vec![
            "read_file",
            "read_files",
            "write_file",
            "patch",
            "grep",
            "find_files",
            "list_directory",
            "todo_write",
            "workspace_context",
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
        assert!(enabled.contains(&"bash".to_string()));
        assert!(enabled.contains(&"web_search".to_string()));
        assert!(enabled.contains(&"write_file".to_string()));
        assert!(enabled.contains(&"patch".to_string()));
        assert!(enabled.contains(&"todo_write".to_string()));
    }

    #[test]
    fn dev_template_enabled_tools_are_resolvable() {
        let toml_str = default_dev_template("d", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("dev template must parse");
        let expected: Vec<&str> = vec![
            "read_file",
            "read_files",
            "write_file",
            "patch",
            "grep",
            "find_files",
            "list_directory",
            "todo_write",
            "workspace_context",
            "bash",
            "web_search",
        ];
        let mut enabled = profile.tools.enabled.clone();
        enabled.sort();
        let mut expected_sorted: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
        expected_sorted.sort();
        assert_eq!(enabled, expected_sorted);
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
                "max_retries",
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
    fn reader_editor_dev_templates_do_not_create_files() {
        let before: Vec<_> = std::fs::read_dir(".").unwrap().collect();
        let _ = default_reader_template("t", "sonnet");
        let _ = default_editor_template("t", "sonnet");
        let _ = default_dev_template("t", "sonnet");
        let after: Vec<_> = std::fs::read_dir(".").unwrap().collect();
        assert_eq!(before.len(), after.len(), "templates must not create files");
    }
}
