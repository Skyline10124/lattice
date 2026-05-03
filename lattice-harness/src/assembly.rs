//! Agent assembly from profile — the canonical construction path.
//!
//! Every entry point (Pipeline, fork, MicroAgent, CLI) should use
//! `assemble_agent()` to ensure consistent wiring of system prompt,
//! tools, executor, and memory.

use std::sync::Arc;

use lattice_agent::{memory::Memory, Agent, DefaultToolExecutor, default_tool_definitions};
use tracing::warn;

use crate::profile::AgentProfile;
use crate::runner::AgentRunner;

/// Assemble an AgentRunner from a profile and resolved model.
///
/// This is the single canonical path for constructing a fully-wired agent.
/// Applies system prompt, tool definitions, tool executor, and memory
/// from the profile configuration.
pub fn assemble_agent(
    profile: &AgentProfile,
    resolved: lattice_core::ResolvedModel,
    memory: Option<Arc<dyn Memory>>,
) -> AgentRunner {
    let mut agent = Agent::new(resolved);

    // Apply system prompt from profile
    let system_prompt = profile.system_prompt();
    agent.set_system_prompt(&system_prompt);

    // Apply tools from profile — filter default tool definitions by enabled names
    if !profile.tools.enabled.is_empty() {
        let all_tools = default_tool_definitions();
        let enabled_tools: Vec<_> = all_tools
            .into_iter()
            .filter(|td| profile.tools.enabled.contains(&td.name))
            .collect();
        if enabled_tools.is_empty() {
            warn!(
                "Agent '{}' has tools enabled but no matching tool definitions found",
                profile.agent.name
            );
        } else {
            agent = agent.with_tools(enabled_tools);
            let executor = DefaultToolExecutor::new(".");
            agent = agent.with_tool_executor(Box::new(executor));
        }
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