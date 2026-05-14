use std::sync::Arc;
use std::time::Instant;

use lattice_core::handoff::HandoffTarget;

use crate::agent_spec::{lower_profile_to_turn_spec, AgentTurnExecutor, AgentTurnRequest};
use crate::assembly::AgentFactory;
use crate::handoff_rule::eval_rules;
use crate::pipeline::{AgentError, AgentResult};
use crate::profile::AgentProfile;
use crate::registry::AgentRegistry;

#[derive(Clone)]
pub struct AgentExecutor {
    registry: Arc<AgentRegistry>,
    factory: AgentFactory,
    turn_executor: Option<Arc<dyn AgentTurnExecutor>>,
}

pub struct ExecutionOutput {
    pub profile: AgentProfile,
    pub output: serde_json::Value,
    pub next: Option<HandoffTarget>,
    pub duration_ms: u64,
}

impl AgentExecutor {
    pub fn new(
        registry: Arc<AgentRegistry>,
        factory: AgentFactory,
        turn_executor: Option<Arc<dyn AgentTurnExecutor>>,
    ) -> Self {
        Self {
            registry,
            factory,
            turn_executor,
        }
    }

    pub fn profile(&self, agent_name: &str) -> Result<AgentProfile, AgentError> {
        self.registry
            .get(agent_name)
            .cloned()
            .ok_or_else(|| AgentError {
                agent_name: agent_name.to_string(),
                message: format!("Agent '{}' not found in registry", agent_name),
                skippable: false,
            })
    }

    pub async fn run_standard(
        &self,
        profile: AgentProfile,
        input: &str,
        default_max_turns: u32,
    ) -> Result<ExecutionOutput, AgentError> {
        // Use runtime-owned turn executor when available
        if let Some(ref executor) = self.turn_executor {
            let spec = lower_profile_to_turn_spec(&profile).map_err(|e| AgentError {
                agent_name: profile.agent.name.clone(),
                message: e.to_string(),
                skippable: profile.agent.skippable,
            })?;
            let request = AgentTurnRequest {
                input: input.to_string(),
                max_turns: profile.handoff.max_turns.unwrap_or(default_max_turns),
                session_id: None,
                persist_session: false,
                cancellation_id: None,
            };
            let start = Instant::now();
            let output = executor
                .run_turn(spec, request)
                .await
                .map_err(|e| AgentError {
                    agent_name: profile.agent.name.clone(),
                    message: e.to_string(),
                    skippable: profile.agent.skippable,
                })?;
            let duration_ms = start.elapsed().as_millis() as u64;
            let next = next_target(&profile, &output.json_output);
            return Ok(ExecutionOutput {
                profile,
                output: output.json_output,
                next,
                duration_ms,
            });
        }

        // Fallback: direct AgentRunner path (legacy, for tests without runtime)
        let mut runner = self
            .factory
            .assemble(&profile)
            .map_err(|e| AgentError {
                agent_name: profile.agent.name.clone(),
                message: e.to_string(),
                skippable: profile.agent.skippable,
            })?
            .runner;

        let max_turns = profile.handoff.max_turns.unwrap_or(default_max_turns);
        let start = Instant::now();
        let output = runner.run(input, max_turns).await.map_err(|e| AgentError {
            agent_name: profile.agent.name.clone(),
            message: e.to_string(),
            skippable: profile.agent.skippable,
        })?;
        let duration_ms = start.elapsed().as_millis() as u64;
        let next = next_target(&profile, &output);

        Ok(ExecutionOutput {
            profile,
            output,
            next,
            duration_ms,
        })
    }

    pub async fn run_standard_by_name(
        &self,
        agent_name: &str,
        input: &str,
        default_max_turns: u32,
    ) -> (String, Result<ExecutionOutput, AgentError>) {
        let profile = match self.profile(agent_name) {
            Ok(profile) => profile,
            Err(err) => return (agent_name.to_string(), Err(err)),
        };
        let name = profile.agent.name.clone();
        (
            name,
            self.run_standard(profile, input, default_max_turns).await,
        )
    }
}

impl From<ExecutionOutput> for AgentResult {
    fn from(output: ExecutionOutput) -> Self {
        Self {
            agent_name: output.profile.agent.name,
            output: output.output,
            next: output.next,
            duration_ms: output.duration_ms,
        }
    }
}

pub fn next_target(profile: &AgentProfile, output: &serde_json::Value) -> Option<HandoffTarget> {
    if profile.handoff.handoff_rules.is_empty() {
        profile.handoff.fallback.clone()
    } else {
        eval_rules(&profile.handoff.handoff_rules, output)
    }
}
