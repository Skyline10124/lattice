// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Default agent templates — generated text only, no filesystem writes.

/// 生成严格符合 v1 `agent.toml` schema 的默认 Agent 模板。
///
/// 模板可由严格 schema 直接加载，仅包含受支持的配置节：`[agent]`、`[prompt]` 与 `[llm.retry]`。
pub fn default_agent_template(name: &str, model: &str) -> String {
    let name = toml_string(name);
    let model = toml_string(model);

    format!(
        r#"# LATTICE Agent Profile (v1)
# Auto-generated template — edit for your project's role and prompt.
#
# Sections:
#   [agent]     — 名称，以及可选的 model、effort 和 description
#   [prompt]    — system prompt (inline `system` or file `system_path`, not both)
#   [llm.retry] — provider stream-open retry policy

schema_version = 1

[agent]
name = {name}
model = {model}

[prompt]
system = "You are a helpful assistant. Be concise and accurate."

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
    use crate::{DiscoveryPaths, load_agent_profile};
    use lattice_conversation::ModelRef;
    use lattice_session::{AgentName, RetryPolicy, TurnTimeout};
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn default_template_loads_and_roundtrips() {
        for (raw_name, raw_model) in [
            ("assistant", "anthropic/claude-sonnet-4-6"),
            ("agent \"alpha\" \\ 1", "local/model\"v1"),
        ] {
            let rendered = default_agent_template(raw_name, raw_model);
            assert!(
                !rendered.contains("[tools]") && !rendered.contains("lancet"),
                "默认模板不应再包含已移除的配置节: {rendered}"
            );

            let dir = tempfile::tempdir().unwrap();
            let uid = std::fs::symlink_metadata(dir.path()).unwrap().uid();
            let paths = DiscoveryPaths {
                project_root: dir.path().join("project"),
                global_agents_dir: dir.path().join("global-agents"),
            };
            let agent_name = AgentName::parse(raw_name).unwrap();
            let package = paths.project_agents_dir().join(agent_name.as_str());
            std::fs::create_dir_all(&package).unwrap();
            std::fs::write(package.join("agent.toml"), &rendered).unwrap();

            let loaded = load_agent_profile(&agent_name, &paths, uid).unwrap();
            assert_eq!(loaded.profile.agent.name, agent_name);
            assert_eq!(
                loaded.profile.agent.model,
                Some(ModelRef::parse(raw_model).unwrap())
            );
            assert_eq!(loaded.profile.agent.effort, None);
            assert_eq!(loaded.profile.agent.description, None);
            assert_eq!(
                loaded.system_prompt,
                "You are a helpful assistant. Be concise and accurate."
            );
            assert_eq!(loaded.profile.llm.timeout_secs, TurnTimeout::default());
            assert_eq!(
                loaded.profile.llm.retry,
                RetryPolicy::new(3, 500, 3000).unwrap()
            );
        }
    }
}
