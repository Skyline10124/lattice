//! Pure profile-derived generic agent spec.
//!
//! This module defines data types that represent a loaded profile's
//! contract as plain owned data, with zero upper-layer
//! semantics. The spec crosses into later runtime phases where
//! model, tool, and sandbox semantics are validated (D-19).

/// Pure-data spec derived from a loaded agent profile.
///
/// Contains no live `Agent`, runtime handle, provider resolution,
/// credential state, sandbox allowlists, session store, plugin
/// references, legacy routing rules, presets, or process working
/// directory / project root (those come from the runtime invocation).
#[derive(Debug, Clone)]
pub struct GenericAgentSpec {
    pub profile_name: String,
    pub model: String,
    pub description: Option<String>,
    pub system_prompt: String,
    pub tools: ToolSelectionSpec,
    pub security: SecuritySpec,
    pub context: ContextSpec,
    pub behavior: BehaviorSpec,
}

#[derive(Debug, Clone)]
pub struct ToolSelectionSpec {
    pub bundles: Vec<String>,
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SecuritySpec {
    pub sandbox_mode: String,
}

#[derive(Debug, Clone)]
pub struct ContextSpec {
    pub project_instructions: bool,
    pub workspace_context: bool,
}

#[derive(Debug, Clone)]
pub struct BehaviorSpec {
    pub max_turns: Option<u32>,
    pub reasoning_effort: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_agent_spec_has_no_cwd_or_project_root_fields() {
        // Compile-time check: these types must not carry runtime invocation context.
        let spec = GenericAgentSpec {
            profile_name: "t".into(),
            model: "sonnet".into(),
            description: None,
            system_prompt: "hi".into(),
            tools: ToolSelectionSpec {
                bundles: vec![],
                enabled: vec![],
            },
            security: SecuritySpec {
                sandbox_mode: "project".into(),
            },
            context: ContextSpec {
                project_instructions: true,
                workspace_context: true,
            },
            behavior: BehaviorSpec {
                max_turns: None,
                reasoning_effort: None,
            },
        };

        // Verify field count: profile_name, model, description, system_prompt,
        // tools, security, context, behavior = 8 fields.
        // No cwd, project_root, provider_override, sandbox_allowlist, etc.
        assert_eq!(spec.profile_name, "t");
        assert_eq!(spec.model, "sonnet");
    }

    /// Source guard: no forbidden symbols in spec.rs.
    #[test]
    fn spec_source_free_of_forbidden_symbols() {
        let source = include_str!("spec.rs");
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
            "cwd",
            "project_root",
            "handoff",
            "orchestrator",
            "plugins",
            "subagents",
            "micro_agents",
            "memory",
        ];
        for pattern in forbidden {
            if prod.contains(pattern) {
                panic!("spec.rs production code must not contain '{pattern}'");
            }
        }
    }
}
