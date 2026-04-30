use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Instant;

use artemis_agent::Agent;
use artemis_memory::Memory;

use crate::events::{EventBus, PipelineEvent};
use crate::handoff_rule::eval_rules;
use crate::profile::AgentProfile;
use crate::registry::AgentRegistry;
use crate::runner::AgentRunner;

// ---------------------------------------------------------------------------
// Pipeline — orchestrates multiple agents in sequence
// ---------------------------------------------------------------------------

/// Shared tokio runtime for async Memory operations.
static MEMORY_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("Failed to create memory tokio runtime")
});

pub struct Pipeline {
    pub name: String,
    pub registry: Arc<AgentRegistry>,
    pub shared_memory: Option<Arc<dyn Memory>>,
    pub event_bus: Option<Arc<EventBus>>,
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
    pub fn new(
        name: &str,
        registry: Arc<AgentRegistry>,
        memory: Option<Arc<dyn Memory>>,
        event_bus: Option<Arc<EventBus>>,
    ) -> Self {
        Self {
            name: name.to_string(),
            registry,
            shared_memory: memory,
            event_bus,
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

        let pipeline_max_agents: u32 = 10;

        for _turn in 0..pipeline_max_agents {
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

            // Per-agent max_turns controls the Agent's internal tool-calling loop.
            // The pipeline loop is bounded by pipeline_max_agents (a safety limit).
            let agent_max_turns = profile.handoff.max_turns.unwrap_or(10);

            let start = Instant::now();

            // Emit agent started event
            if let Some(ref bus) = self.event_bus {
                bus.send(PipelineEvent::AgentStarted {
                    agent: profile.agent.name.clone(),
                    input_size: current_input.len(),
                });
            }

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
                        if !handle_fallback(&profile, &self.registry, &mut current_agent) {
                            break;
                        }
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
                Ok(output) => {
                    let duration_ms = start.elapsed().as_millis() as u64;

                    // Evaluate handoff rules to determine next agent
                    let next = if profile.handoff.handoff_rules.is_empty() {
                        // No rules defined — use fallback or end pipeline
                        profile.handoff.fallback.clone()
                    } else {
                        eval_rules(&profile.handoff.handoff_rules, &output)
                    };

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
                        if let Ok(handle) = tokio::runtime::Handle::try_current() {
                            tokio::task::block_in_place(|| handle.block_on(mem.save_entry(entry)));
                        } else {
                            MEMORY_RT.block_on(mem.save_entry(entry));
                        }
                    }

                    // Emit agent completed event
                    if let Some(ref bus) = self.event_bus {
                        let preview: String = output.to_string().chars().take(500).collect();
                        bus.send(PipelineEvent::AgentCompleted {
                            agent: profile.agent.name.clone(),
                            output_preview: preview,
                            next: next.clone(),
                            duration_ms,
                        });
                    }

                    results.push(AgentResult {
                        agent_name: profile.agent.name.clone(),
                        output: output.clone(),
                        next: next.clone(),
                        duration_ms,
                    });

                    match next {
                        Some(ref n) => {
                            if let Some(ref bus) = self.event_bus {
                                bus.send(PipelineEvent::Handoff {
                                    from: profile.agent.name.clone(),
                                    to: n.clone(),
                                });
                            }
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
                    if let Some(ref bus) = self.event_bus {
                        bus.send(PipelineEvent::PipelineError {
                            agent: profile.agent.name.clone(),
                            message: e.to_string(),
                            skippable: profile.agent.skippable,
                        });
                    }

                    let err = AgentError {
                        agent_name: profile.agent.name.clone(),
                        message: e.to_string(),
                        skippable: profile.agent.skippable,
                    };
                    if profile.agent.skippable {
                        skipped.push(profile.agent.name.clone());
                        errors.push(err);
                        if !handle_fallback(&profile, &self.registry, &mut current_agent) {
                            break;
                        }
                        continue;
                    } else {
                        errors.push(err);
                        break;
                    }
                }
            }

            // Use agent-specific max_turns for results limit too
            if results.len() > agent_max_turns as usize * 5 {
                break;
            }
        }

        let duration_ms = pipeline_start.elapsed().as_millis() as u64;
        if let Some(ref bus) = self.event_bus {
            bus.send(PipelineEvent::PipelineCompleted {
                total_agents: results.len(),
                duration_ms,
            });
        }

        PipelineRun {
            results,
            errors,
            completed,
            skipped,
            duration_ms,
        }
    }

    /// Dry-run: validate the pipeline chain without calling any LLM.
    ///
    /// Checks that every agent in the chain exists, all handoff targets are
    /// registered, and there are no circular references.  Returns a list of
    /// issues found (empty = valid).
    pub fn dry_run(&self, start_agent: &str) -> DryRunReport {
        let mut report = DryRunReport::default();
        let mut visited = std::collections::HashSet::new();
        let mut current = start_agent.to_string();

        for _step in 0..100 {
            if visited.contains(&current) {
                report.circular = true;
                report.issues.push(format!(
                    "Circular reference detected: '{}' visited twice",
                    current
                ));
                break;
            }
            visited.insert(current.clone());

            let profile = match self.registry.get(&current) {
                Some(p) => p,
                None => {
                    report.issues.push(format!("Agent '{}' not found in registry", current));
                    break;
                }
            };

            report.agents_in_chain.push(profile.agent.name.clone());

            // Check each handoff rule target
            for (i, rule) in profile.handoff.handoff_rules.iter().enumerate() {
                if let Some(ref target) = rule.target {
                    if self.registry.get(target).is_none() {
                        report.issues.push(format!(
                            "Agent '{}' rule[{}] targets '{}' which is not registered",
                            profile.agent.name, i, target
                        ));
                    }
                }
            }

            // Check fallback target
            if let Some(ref fallback) = profile.handoff.fallback {
                if self.registry.get(fallback).is_none() {
                    report.issues.push(format!(
                        "Agent '{}' fallback '{}' is not registered",
                        profile.agent.name, fallback
                    ));
                }
            }

            // Determine next agent (first unconditional/default rule)
            let next = if profile.handoff.handoff_rules.is_empty() {
                profile.handoff.fallback.clone()
            } else {
                profile
                    .handoff
                    .handoff_rules
                    .iter()
                    .find(|r| r.default)
                    .and_then(|r| r.target.clone())
                    .or_else(|| profile.handoff.fallback.clone())
            };

            match next {
                Some(ref n) => {
                    if !self.registry.get(n).is_some() {
                        report.issues.push(format!(
                            "Default route from '{}' -> '{}' targets unregistered agent",
                            profile.agent.name, n
                        ));
                        break;
                    }
                    current = n.clone();
                }
                None => {
                    report.reachable_end = true;
                    break;
                }
            }
        }

        if report.agents_in_chain.len() >= 100 {
            report.issues.push("Chain exceeded 100 steps (infinite loop?)".into());
        }

        report.valid = report.issues.is_empty() && report.reachable_end && !report.circular;
        report
    }
}

/// Result of a pipeline dry-run validation.
#[derive(Debug, Default)]
pub struct DryRunReport {
    pub valid: bool,
    pub agents_in_chain: Vec<String>,
    pub reachable_end: bool,
    pub circular: bool,
    pub issues: Vec<String>,
}

/// Try to route to the fallback agent. Returns false if no valid fallback.
fn handle_fallback(
    profile: &AgentProfile,
    registry: &AgentRegistry,
    current_agent: &mut String,
) -> bool {
    if let Some(ref fallback) = profile.handoff.fallback {
        if registry.get(fallback).is_some() {
            *current_agent = fallback.clone();
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff_rule::HandoffRule;
    use std::sync::Arc;

    fn test_registry() -> Arc<AgentRegistry> {
        // Build registry in-memory without temp files
        let dir = std::env::temp_dir().join("artemis_test_dry_run");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("code-review")).unwrap();
        std::fs::create_dir_all(dir.join("refactor")).unwrap();

        std::fs::write(
            dir.join("code-review/agent.toml"),
            r#"
[agent]
name = "code-review"
model = "sonnet"

[system]
prompt = "Test"

[handoff]
fallback = "refactor"

[[handoff.rules]]
condition = { field = "confidence", op = ">", value = "0.5" }
target = "refactor"

[[handoff.rules]]
default = true
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("refactor/agent.toml"),
            r#"
[agent]
name = "refactor"
model = "sonnet"

[system]
prompt = "Test"

[handoff]
[[handoff.rules]]
default = true
"#,
        )
        .unwrap();

        let registry = Arc::new(AgentRegistry::load_dir(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        registry
    }

    #[test]
    fn test_dry_run_valid_pipeline() {
        let registry = test_registry();
        let pipeline = Pipeline::new("test", registry, None, None);
        let report = pipeline.dry_run("code-review");
        assert!(report.valid);
        assert!(report.reachable_end);
        assert!(!report.circular);
    }

    #[test]
    fn test_dry_run_missing_agent() {
        let registry = test_registry();
        let pipeline = Pipeline::new("test", registry, None, None);
        let report = pipeline.dry_run("nonexistent");
        assert!(!report.valid);
        assert!(!report.issues.is_empty());
    }
}
