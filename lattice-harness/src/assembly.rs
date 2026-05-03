//! Agent assembly from profile — the canonical construction path.
//!
//! Every entry point (Pipeline, fork, MicroAgent, CLI) should use
//! `assemble_agent()` to ensure consistent wiring of system prompt,
//! tools, executor, and memory.

use std::sync::Arc;

use lattice_agent::{default_tool_definitions, memory::Memory, Agent, DefaultToolExecutor};
use tracing::warn;

use crate::profile::AgentProfile;
use crate::runner::AgentRunner;
use crate::tools::ToolRegistry;

/// Assemble an AgentRunner from a profile and resolved model.
///
/// This is the single canonical path for constructing a fully-wired agent.
/// Applies system prompt, tool definitions, tool executor, and memory
/// from the profile configuration.
///
/// When `tool_registry` is provided and non-empty, custom tool definitions
/// from the registry are merged with the default tools, and the executor
/// checks the registry first for custom tool handlers.
pub fn assemble_agent(
    profile: &AgentProfile,
    resolved: lattice_core::ResolvedModel,
    memory: Option<Arc<dyn Memory>>,
    tool_registry: Option<Arc<ToolRegistry>>,
) -> AgentRunner {
    let mut agent = Agent::new(resolved);

    // Apply system prompt from profile
    let system_prompt = profile.system_prompt();
    agent.set_system_prompt(&system_prompt);

    // Apply tools from profile — filter default tool definitions by enabled names
    let all_defaults = default_tool_definitions();
    let enabled_tools: Vec<_> = if profile.tools.enabled.is_empty() {
        all_defaults
    } else {
        all_defaults
            .into_iter()
            .filter(|td| profile.tools.enabled.contains(&td.name))
            .collect()
    };

    // Merge with registry tools if present
    let (final_tools, executor) = if let Some(reg) = &tool_registry {
        if !reg.is_empty() {
            let registry_defs: Vec<lattice_core::types::ToolDefinition> = reg
                .definitions()
                .into_iter()
                .filter(|d| !enabled_tools.iter().any(|et| et.name == d.name))
                .collect();
            let merged = enabled_tools.into_iter().chain(registry_defs).collect();
            let exec = DefaultToolExecutor::new_with_registry(
                ".",
                Some(reg.clone() as Arc<dyn lattice_agent::RegistryToolAccess>),
            );
            (merged, exec)
        } else {
            let exec = DefaultToolExecutor::new(".");
            (enabled_tools, exec)
        }
    } else {
        let exec = DefaultToolExecutor::new(".");
        (enabled_tools, exec)
    };

    if !final_tools.is_empty() {
        agent = agent.with_tools(final_tools);
        agent = agent.with_tool_executor(Box::new(executor));
    } else {
        warn!("Agent '{}' has no tool definitions", profile.agent.name);
    }

    if let Some(ref mem) = memory {
        agent = agent.with_memory(mem.clone_box());
    }

    let mut runner = AgentRunner::from_profile(profile.clone(), agent);
    if let Some(ref mem) = memory {
        runner = runner.with_memory(Arc::clone(mem));
    }
    runner
}
