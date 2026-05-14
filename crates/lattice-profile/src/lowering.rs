//! Pure profile lowering — converts a `LoadedAgentProfile` into a `GenericAgentSpec`.
//!
//! Lowering copies already-loaded profile data into pure data specs. It must not
//! resolve providers, credentials, tool bundles, sandbox configs, or construct
//! any live `Agent`, runtime handle, session store, or plugin state (D-18, D-19).

use crate::error::ProfileError;
use crate::load::LoadedAgentProfile;
use crate::schema::SandboxMode;
use crate::spec::{BehaviorSpec, ContextSpec, GenericAgentSpec, SecuritySpec, ToolSelectionSpec};

/// Lower a loaded profile into a pure, runtime-independent `GenericAgentSpec`.
///
/// Validates that required fields (profile name, model, system prompt) are
/// non-empty. The resulting spec is a plain data copy with no live assembly.
pub fn lower_profile_to_spec(
    loaded: &LoadedAgentProfile,
) -> Result<GenericAgentSpec, ProfileError> {
    let profile = &loaded.profile;

    // Required field validation.
    if profile.agent.name.is_empty() {
        return Err(ProfileError::InvalidField {
            path: loaded.path.clone(),
            section: "agent".into(),
            field: "name".into(),
            value: "(empty)".into(),
            reason: "agent name must not be empty".into(),
            expected: "a non-empty agent name string".into(),
        });
    }
    if profile.agent.model.is_empty() {
        return Err(ProfileError::InvalidField {
            path: loaded.path.clone(),
            section: "agent".into(),
            field: "model".into(),
            value: "(empty)".into(),
            reason: "agent model must not be empty".into(),
            expected: "a non-empty model identifier string".into(),
        });
    }
    if loaded.system_prompt.is_empty() {
        return Err(ProfileError::InvalidField {
            path: loaded.path.clone(),
            section: "prompt".into(),
            field: "system".into(),
            value: "(empty)".into(),
            reason: "system prompt must not be empty".into(),
            expected: "a non-empty inline system prompt or system_path file contents".into(),
        });
    }

    let sandbox_mode = profile
        .security
        .as_ref()
        .map(|s| match s.sandbox_mode {
            SandboxMode::Project => "project",
            SandboxMode::Strict => "strict",
            SandboxMode::Permissive => "permissive",
        })
        .unwrap_or("project");

    let context = profile.context.as_ref().map_or(
        ContextSpec {
            project_instructions: true,
            workspace_context: true,
        },
        |c| ContextSpec {
            project_instructions: c.project_instructions,
            workspace_context: c.workspace_context,
        },
    );

    let behavior = profile.behavior.as_ref().map_or(
        BehaviorSpec {
            max_turns: None,
            reasoning_effort: None,
        },
        |b| BehaviorSpec {
            max_turns: b.max_turns,
            reasoning_effort: b.reasoning_effort.clone(),
        },
    );

    Ok(GenericAgentSpec {
        profile_name: profile.agent.name.clone(),
        model: profile.agent.model.clone(),
        description: profile.agent.description.clone(),
        system_prompt: loaded.system_prompt.clone(),
        tools: ToolSelectionSpec {
            bundles: profile.tools.bundles.clone(),
            enabled: profile.tools.enabled.clone(),
        },
        security: SecuritySpec {
            sandbox_mode: sandbox_mode.to_string(),
        },
        context,
        behavior,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::load_agent_profile;

    fn write_and_load(toml: &str) -> Result<LoadedAgentProfile, ProfileError> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.toml");
        std::fs::write(&path, toml).unwrap();
        load_agent_profile(&path)
    }

    #[test]
    fn lower_coding_capable_profile_has_no_preset_field() {
        let toml = r#"
schema_version = 1

[agent]
name = "code-helper"
model = "sonnet"
description = "helps with code"

[prompt]
system = "You are a coding assistant."

[tools]
bundles = ["workspace", "filesystem", "editing", "shell", "planning", "web"]
enabled = ["grep"]

[security]
sandbox_mode = "strict"
"#;
        let loaded = write_and_load(toml).unwrap();
        let spec = lower_profile_to_spec(&loaded).unwrap();

        assert_eq!(spec.profile_name, "code-helper");
        assert_eq!(spec.model, "sonnet");
        assert_eq!(spec.description.as_deref(), Some("helps with code"));
        assert_eq!(spec.system_prompt, "You are a coding assistant.");
        assert_eq!(spec.tools.bundles.len(), 6);
        assert!(spec.tools.bundles.contains(&"workspace".to_string()));
        assert!(spec.tools.enabled.contains(&"grep".to_string()));
        assert_eq!(spec.security.sandbox_mode, "strict");
    }

    #[test]
    fn lower_rejects_empty_name() {
        let toml = r#"
schema_version = 1

[agent]
name = ""
model = "sonnet"

[prompt]
system = "hi"
"#;
        let loaded = write_and_load(toml).unwrap();
        let err = lower_profile_to_spec(&loaded).unwrap_err();
        assert!(err.to_string().contains("name"), "got: {err}");
    }

    #[test]
    fn lower_rejects_empty_model() {
        let toml = r#"
schema_version = 1

[agent]
name = "test"
model = ""

[prompt]
system = "hi"
"#;
        let loaded = write_and_load(toml).unwrap();
        let err = lower_profile_to_spec(&loaded).unwrap_err();
        assert!(err.to_string().contains("model"), "got: {err}");
    }

    #[test]
    fn lower_rejects_empty_system_prompt() {
        // Load a valid profile first, then craft a LoadedAgentProfile with empty prompt.
        let toml = r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "hi"
"#;
        let loaded = write_and_load(toml).unwrap();
        // Override with empty system prompt to test lowering rejection directly.
        let mut empty_loaded = loaded.clone();
        empty_loaded.system_prompt = String::new();
        let err = lower_profile_to_spec(&empty_loaded).unwrap_err();
        assert!(err.to_string().contains("system"), "got: {err}");
    }

    #[test]
    fn lower_minimal_profile_with_defaults() {
        let toml = r#"
schema_version = 1

[agent]
name = "mini"
model = "haiku"

[prompt]
system = "Be brief."
"#;
        let loaded = write_and_load(toml).unwrap();
        let spec = lower_profile_to_spec(&loaded).unwrap();

        assert_eq!(spec.tools.bundles, Vec::<String>::new());
        assert_eq!(spec.tools.enabled, Vec::<String>::new());
        assert_eq!(spec.security.sandbox_mode, "project");
        assert!(spec.context.project_instructions);
        assert!(spec.context.workspace_context);
        assert!(spec.behavior.max_turns.is_none());
        assert!(spec.behavior.reasoning_effort.is_none());
    }

    #[test]
    fn lowered_spec_contains_no_cwd_or_project_root() {
        let toml = r#"
schema_version = 1

[agent]
name = "test"
model = "sonnet"

[prompt]
system = "hi"
"#;
        let loaded = write_and_load(toml).unwrap();
        let spec = lower_profile_to_spec(&loaded).unwrap();

        // The spec struct has exactly 8 fields — verify via debug output
        // that no cwd/project_root/handoff/orchestrator fields exist.
        let debug = format!("{spec:#?}");
        assert!(!debug.contains("cwd"));
        assert!(!debug.contains("project_root"));
        assert!(!debug.contains("handoff"));
        assert!(!debug.contains("orchestrator"));
        assert!(!debug.contains("preset"));
        assert!(!debug.contains("Coding"));
        assert!(!debug.contains("code_agent"));
        assert!(!debug.contains("plugins"));
        assert!(!debug.contains("subagents"));
        assert!(!debug.contains("memory"));
    }

    /// Source guard: no forbidden imports in lowering.rs production code.
    #[test]
    fn lowering_source_free_of_forbidden_imports() {
        let source = include_str!("lowering.rs");
        let test_start = source.find("#[cfg(test)]").unwrap_or(source.len());
        let prod = &source[..test_start];
        let forbidden = [
            "lattice_runtime",
            "lattice_orchestrator",
            "lattice_agent",
            "lattice_session",
            "lattice_plugin",
            "lattice_cli",
            "AgentPreset",
            "ToolPreset",
            "code_agent",
            "code-agent",
        ];
        for pattern in forbidden {
            if prod.contains(pattern) {
                panic!("lowering.rs production code must not contain '{pattern}'");
            }
        }
    }
}
