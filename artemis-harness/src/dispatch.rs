use std::sync::Arc;

use artemis_agent::AgentDispatcher;
use artemis_memory::Memory;

use crate::registry::AgentRegistry;
use crate::runner::AgentRunner;

// ---------------------------------------------------------------------------
// HarnessAgentDispatcher — lets DefaultToolExecutor launch sub-agents
// ---------------------------------------------------------------------------

/// Implements `AgentDispatcher` so that `agent_call:name` tool calls
/// resolve agents from an `AgentRegistry`, run them, and return output.
pub struct HarnessAgentDispatcher {
    pub registry: Arc<AgentRegistry>,
    pub memory: Option<Arc<dyn Memory>>,
}

impl AgentDispatcher for HarnessAgentDispatcher {
    fn dispatch(&self, agent_name: &str, input: &str) -> String {
        let profile = match self.registry.get(agent_name) {
            Some(p) => p.clone(),
            None => {
                return format!(
                    "Error: agent '{}' not found. Available agents: {:?}",
                    agent_name,
                    self.registry
                        .list()
                        .iter()
                        .map(|p| p.agent.name.as_str())
                        .collect::<Vec<_>>()
                );
            }
        };

        let resolved = match artemis_core::resolve(&profile.agent.model) {
            Ok(r) => r,
            Err(e) => {
                return format!("Error resolving model for '{}': {}", agent_name, e);
            }
        };

        let mut agent = artemis_agent::Agent::new(resolved);
        if let Some(ref mem) = self.memory {
            agent = agent.with_memory(mem.clone_box());
        }

        let mut runner = AgentRunner::from_profile(profile.clone(), agent);
        runner.shared_memory = self.memory.clone();

        match runner.run(input) {
            Ok((output, _)) => output.to_string(),
            Err(e) => format!("Error running agent '{}': {}", agent_name, e),
        }
    }
}
