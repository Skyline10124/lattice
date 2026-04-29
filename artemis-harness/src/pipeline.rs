use std::sync::Arc;
use std::time::Instant;

use artemis_agent::Agent;
use artemis_memory::Memory;

use crate::profile::AgentProfile;
use crate::registry::AgentRegistry;
use crate::runner::AgentRunner;

// ---------------------------------------------------------------------------
// Pipeline — orchestrates multiple agents in sequence
// ---------------------------------------------------------------------------

pub struct Pipeline {
    pub name: String,
    pub registry: Arc<AgentRegistry>,
    pub shared_memory: Option<Arc<dyn Memory>>,
}

pub struct PipelineRun {
    pub results: Vec<AgentResult>,
    pub errors: Vec<AgentError>,
    pub completed: bool,
    pub skipped: Vec<String>,
    pub duration_ms: u64,
}

pub struct AgentResult {
    pub agent_name: String,
    pub output: serde_json::Value,
    pub next: Option<String>,
    pub duration_ms: u64,
}

pub struct AgentError {
    pub agent_name: String,
    pub message: String,
    pub skippable: bool,
}

impl Pipeline {
    pub fn new(name: &str, registry: Arc<AgentRegistry>, memory: Option<Arc<dyn Memory>>) -> Self {
        Self {
            name: name.to_string(),
            registry,
            shared_memory: memory,
        }
    }

    /// Run the pipeline starting from the given agent name.
    pub fn run(&mut self, start_agent: &str, input: &str) -> PipelineRun {
        let pipeline_start = Instant::now();
        let mut results = Vec::new();
        let mut errors = Vec::new();
        let mut skipped = Vec::new();
        let mut current_agent = start_agent.to_string();
        let mut current_input = input.to_string();
        let mut completed = false;

        for _turn in 0..10 {
            let profile = match self.registry.get(&current_agent) {
                Some(p) => p.clone(),
                None => {
                    errors.push(AgentError {
                        agent_name: current_agent.clone(),
                        message: format!("Agent '{}' not found in registry", current_agent),
                        skippable: false,
                    });
                    break;
                }
            };

            let start = Instant::now();

            // Resolve model and create Agent
            let resolved = match artemis_core::resolve(&profile.agent.model) {
                Ok(r) => r,
                Err(e) => {
                    let err = AgentError {
                        agent_name: profile.agent.name.clone(),
                        message: format!("Resolve failed: {}", e),
                        skippable: profile.agent.skippable,
                    };
                    if profile.agent.skippable {
                        skipped.push(profile.agent.name.clone());
                        errors.push(err);
                        handle_fallback(
                            &profile,
                            &self.registry,
                            &mut current_agent,
                            &mut current_input,
                        );
                        continue;
                    } else {
                        errors.push(err);
                        break;
                    }
                }
            };

            let mut agent = Agent::new(resolved);
            if let Some(ref mem) = self.shared_memory {
                agent = agent.with_memory(mem.clone_box());
            }

            let mut runner = AgentRunner::from_profile(profile.clone(), agent);
            if let Some(ref mem) = self.shared_memory {
                runner = runner.with_memory(Arc::clone(mem));
            }

            match runner.run(&current_input) {
                Ok((output, next)) => {
                    let duration_ms = start.elapsed().as_millis() as u64;
                    results.push(AgentResult {
                        agent_name: profile.agent.name.clone(),
                        output: output.clone(),
                        next: next.clone(),
                        duration_ms,
                    });

                    // Save session log to shared memory
                    if let Some(ref mem) = self.shared_memory {
                        let entry = artemis_memory::MemoryEntry {
                            id: format!(
                                "{}-{}",
                                profile.agent.name,
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                            ),
                            kind: artemis_memory::EntryKind::SessionLog,
                            session_id: profile.agent.name.clone(),
                            summary: format!(
                                "{}: {} chars output",
                                profile.agent.name,
                                output.to_string().len()
                            ),
                            content: output.to_string(),
                            tags: profile.agent.tags.clone(),
                            created_at: format!(
                                "{}",
                                std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs()
                            ),
                        };
                        futures::executor::block_on(mem.save_entry(entry));
                    }

                    match next {
                        Some(ref n) => {
                            current_input = output.to_string();
                            current_agent = n.clone();
                        }
                        None => {
                            completed = true;
                            break;
                        }
                    }
                }
                Err(e) => {
                    let err = AgentError {
                        agent_name: profile.agent.name.clone(),
                        message: e.to_string(),
                        skippable: profile.agent.skippable,
                    };
                    if profile.agent.skippable {
                        skipped.push(profile.agent.name.clone());
                        errors.push(err);
                        handle_fallback(
                            &profile,
                            &self.registry,
                            &mut current_agent,
                            &mut current_input,
                        );
                        continue;
                    } else {
                        errors.push(err);
                        break;
                    }
                }
            }

            // Prevent infinite loops
            if results.len() > 50 {
                break;
            }
        }

        PipelineRun {
            results,
            errors,
            completed,
            skipped,
            duration_ms: pipeline_start.elapsed().as_millis() as u64,
        }
    }
}

fn handle_fallback(
    profile: &AgentProfile,
    registry: &AgentRegistry,
    current_agent: &mut String,
    _current_input: &mut String,
) {
    if let Some(ref fallback) = profile.handoff.fallback {
        if registry.get(fallback).is_some() {
            *current_agent = fallback.clone();
            return;
        }
    }
    // No valid fallback — pipeline stops by not updating current_agent
}
