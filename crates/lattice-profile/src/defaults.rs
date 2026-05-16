//! v1 default agent template — generated text only, no filesystem writes.

/// Generate a strict v1 `agent.toml` template for a new agent.
///
/// The template is parseable by the strict schema and contains only allowed
/// v1 sections. It includes comments telling the user to edit role, prompt,
/// tools, and security settings (D-08).
pub fn default_agent_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1)
# Auto-generated template — edit for your project's role, prompt, tools, and security.
#
# Sections:
#   [agent]    — identity (name, model, optional description)
#   [prompt]   — system prompt (inline `system` or file `system_path`, not both)
#   [tools]    — tool bundles and explicitly enabled tool names
#   [security] — sandbox_mode: "project", "strict", or "permissive"
#   [context]  — context provider toggles
#   [behavior] — turn-level controls

schema_version = 1

[agent]
name = {name}
model = {model}

[prompt]
system = "You are a helpful assistant. Be concise and accurate."

[tools]
bundles = ["workspace", "filesystem"]
enabled = []

[security]
sandbox_mode = "project"

[context]
project_instructions = true
workspace_context = true

[behavior]
max_turns = 10
reasoning_effort = "medium"
"#
    )
}

/// Generate a strict v1 `agent.toml` template for a read-only agent.
///
/// The reader profile enables only workspace and filesystem bundles (no shell,
/// no editing, no web). Uses project sandbox with low reasoning effort and
/// modest turn limits — suitable for code review, understanding, and search.
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
bundles = ["workspace", "filesystem"]
enabled = ["grep"]

[security]
sandbox_mode = "project"

[context]
project_instructions = true
workspace_context = true

[behavior]
max_turns = 5
reasoning_effort = "medium"
"#
    )
}

/// Generate a strict v1 `agent.toml` template for an editing agent.
///
/// The editor profile enables workspace, filesystem, editing, and shell
/// bundles. Uses project sandbox with medium turn limits — suitable for
/// refactoring, fixing, and applying patches.
pub fn default_editor_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Editor
# Read-write agent for editing, refactoring, and applying patches.
# Shell access enabled for build/format/lint commands.

schema_version = 1

[agent]
name = {name}
model = {model}
description = "Editing agent for refactoring, fixing, and applying patches"

[prompt]
system = "You are a code editor. Read, edit, and refactor code within the project. Run build, format, and lint commands to verify your changes. Be precise and explain your edits."

[tools]
bundles = ["workspace", "filesystem", "editing", "shell"]
enabled = ["grep"]

[security]
sandbox_mode = "project"

[context]
project_instructions = true
workspace_context = true

[behavior]
max_turns = 20
reasoning_effort = "medium"
"#
    )
}

/// Generate a strict v1 `agent.toml` template for a development agent.
///
/// The dev profile enables all tool bundles (workspace, filesystem,
/// editing, shell, planning, web). Uses strict sandbox with higher turn
/// limits — suitable for full development workflows.
pub fn default_dev_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Developer
# Full development agent with all tool bundles enabled.
# Strict sandbox limits operations to the project directory tree.

schema_version = 1

[agent]
name = {name}
model = {model}
description = "Full development agent for coding, planning, and troubleshooting"

[prompt]
system = "You are a full-stack developer. Read, write, edit, and refactor code. Plan changes before acting. Run shell commands for build, test, lint, and version control. Search the web when documentation is needed. Be methodical and explain your reasoning."

[tools]
bundles = ["workspace", "filesystem", "editing", "shell", "planning", "web"]
enabled = ["grep"]

[security]
sandbox_mode = "strict"

[context]
project_instructions = true
workspace_context = true

[behavior]
max_turns = 50
reasoning_effort = "high"
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
        assert_eq!(profile.tools.bundles, vec!["workspace", "filesystem"]);
        assert!(profile.tools.enabled.is_empty());
        let sec = profile.security.unwrap();
        assert!(matches!(
            sec.sandbox_mode,
            crate::schema::SandboxMode::Project
        ));
        let ctx = profile.context.unwrap();
        assert!(ctx.project_instructions);
        assert!(ctx.workspace_context);
        let bhv = profile.behavior.unwrap();
        assert_eq!(bhv.max_turns, Some(10));
        assert_eq!(bhv.reasoning_effort.as_deref(), Some("medium"));
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
        assert!(toml_str.contains("security"), "must mention security");
    }

    #[test]
    fn template_has_only_allowed_top_level_sections() {
        let toml_str = default_agent_template("t", "sonnet");
        let allowed = [
            "schema_version",
            "[agent]",
            "[prompt]",
            "[tools]",
            "[security]",
            "[context]",
            "[behavior]",
        ];
        for section in &allowed {
            assert!(
                toml_str.contains(section),
                "missing allowed section: {section}"
            );
        }
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
            "skippable",
            "tags",
            "behavior_type",
            "confidence_threshold",
            "max_retries",
            "enforce_sandbox",
            "code_agent",
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
        assert_eq!(profile.tools.bundles, vec!["workspace", "filesystem"]);
        assert!(!profile.tools.bundles.contains(&"shell".to_string()));
        assert!(!profile.tools.bundles.contains(&"editing".to_string()));
        assert!(!profile.tools.bundles.contains(&"web".to_string()));
        let sec = profile.security.unwrap();
        assert!(matches!(
            sec.sandbox_mode,
            crate::schema::SandboxMode::Project
        ));
        let bhv = profile.behavior.unwrap();
        assert_eq!(bhv.max_turns, Some(5));
        assert_eq!(bhv.reasoning_effort.as_deref(), Some("medium"));
    }

    #[test]
    fn reader_template_has_no_shell_or_web() {
        let toml_str = default_reader_template("r", "haiku").to_lowercase();
        assert!(!toml_str.contains("shell"));
        assert!(!toml_str.contains("web"));
        assert!(!toml_str.contains("editing"));
    }

    // ── Editor template tests ──────────────────────────────────────────

    #[test]
    fn editor_template_parses_as_valid_v1_profile() {
        let toml_str = default_editor_template("editor", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("editor template must be valid v1 TOML");
        assert_eq!(profile.agent.name, "editor");
        assert_eq!(profile.agent.model, "sonnet");
        assert!(profile.tools.bundles.contains(&"workspace".to_string()));
        assert!(profile.tools.bundles.contains(&"filesystem".to_string()));
        assert!(profile.tools.bundles.contains(&"editing".to_string()));
        assert!(profile.tools.bundles.contains(&"shell".to_string()));
        assert!(!profile.tools.bundles.contains(&"web".to_string()));
        let bhv = profile.behavior.unwrap();
        assert_eq!(bhv.max_turns, Some(20));
    }

    #[test]
    fn editor_template_has_shell_no_web() {
        let toml_str = default_editor_template("e", "sonnet").to_lowercase();
        assert!(toml_str.contains("shell"));
        assert!(!toml_str.contains("web"));
    }

    // ── Developer template tests ───────────────────────────────────────

    #[test]
    fn dev_template_parses_as_valid_v1_profile() {
        let toml_str = default_dev_template("dev", "sonnet");
        let profile: crate::schema::AgentProfile =
            toml::from_str(&toml_str).expect("dev template must be valid v1 TOML");
        assert_eq!(profile.agent.name, "dev");
        assert_eq!(profile.agent.model, "sonnet");
        assert_eq!(profile.tools.bundles.len(), 6);
        assert!(profile.tools.bundles.contains(&"web".to_string()));
        assert!(profile.tools.bundles.contains(&"shell".to_string()));
        assert!(profile.tools.bundles.contains(&"editing".to_string()));
        assert!(profile.tools.bundles.contains(&"planning".to_string()));
        let sec = profile.security.unwrap();
        assert!(matches!(
            sec.sandbox_mode,
            crate::schema::SandboxMode::Strict
        ));
        let bhv = profile.behavior.unwrap();
        assert_eq!(bhv.max_turns, Some(50));
        assert_eq!(bhv.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn dev_template_uses_strict_sandbox() {
        let toml_str = default_dev_template("d", "sonnet").to_lowercase();
        assert!(toml_str.contains(r#"sandbox_mode = "strict""#));
    }

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
                "skippable",
                "tags",
                "behavior_type",
                "confidence_threshold",
                "max_retries",
                "enforce_sandbox",
                "code_agent",
            ];
            for f in &forbidden {
                assert!(!toml_str.contains(f), "template must not contain '{f}'");
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
