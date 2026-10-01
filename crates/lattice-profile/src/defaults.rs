// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Default agent templates — generated text only, no filesystem writes.

const LANCET_WORKSPACE_SECTION: &str = r#"
[lancet.workspace]
denied_write_patterns = [".git/**"]
"#;

const LANCET_WORKSPACE_NOTE: &str = r#"# [lancet.workspace].denied_write_patterns is a blacklist — paths matching any
# pattern are rejected.
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
{lancet_workspace_note}
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
{lancet_workspace_section}
"#,
        lancet_workspace_note = LANCET_WORKSPACE_NOTE,
        lancet_workspace_section = LANCET_WORKSPACE_SECTION
    )
}

/// Generate a strict v1 `agent.toml` template for a read-only agent.
///
/// Host-only audit inspection uses `lattice audit`; agent catalog has no audit tools.
pub fn default_reader_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Reader
# Read-only agent for code review and audit inspection.
{lancet_workspace_note}
schema_version = 1

[agent]
name = {name}
model = {model}
description = "Read-only agent for code understanding and review"

[prompt]
system = "You are a code reader and reviewer. Read and analyze code carefully. Do not suggest edits or run commands. Explain your reasoning clearly."

[tools]
enabled = ["observe.workspace"]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
{lancet_workspace_section}
"#,
        lancet_workspace_note = LANCET_WORKSPACE_NOTE,
        lancet_workspace_section = LANCET_WORKSPACE_SECTION
    )
}

/// Generate a strict v1 `agent.toml` template for an editing agent.
pub fn default_editor_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Editor
# Editing agent for refactoring and fixing code.
{lancet_workspace_note}
schema_version = 1

[agent]
name = {name}
model = {model}
description = "Editing agent for refactoring and fixing code"

[prompt]
system = "You are a code editor. Read, edit, and refactor code within the project. Use observe, create, and edit tools for changes. Be precise and explain your edits."

[tools]
enabled = ["session.todo_write"]

[llm.retry]
max_retries = 3
initial_delay_ms = 500
max_delay_ms = 3000
{lancet_workspace_section}
"#,
        lancet_workspace_note = LANCET_WORKSPACE_NOTE,
        lancet_workspace_section = LANCET_WORKSPACE_SECTION
    )
}

/// Generate a strict v1 `agent.toml` template for a development agent.
pub fn default_dev_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1) — Developer
# Full development agent for coding, planning, and troubleshooting.
{lancet_workspace_note}
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
{lancet_workspace_section}
"#,
        lancet_workspace_note = LANCET_WORKSPACE_NOTE,
        lancet_workspace_section = LANCET_WORKSPACE_SECTION
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
