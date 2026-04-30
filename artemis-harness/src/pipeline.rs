use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Instant;

use artemis_agent::Agent;
use artemis_memory::Memory;

use crate::events::{EventBus, PipelineEvent};
use crate::handoff_rule::{eval_rules, HandoffTarget};
use crate::profile::AgentProfile;
use crate::registry::AgentRegistry;
use crate::runner::AgentRunner;

// ---------------------------------------------------------------------------
// Pipeline — orchestrates multiple agents in sequence or parallel (fork)
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
    pub next: Option<HandoffTarget>,
    pub duration_ms: u64,
}

/// Result from a single fork branch.
pub struct ForkBranchResult {
    pub agent_name: String,
    pub output: serde_json::Value,
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

        let pipeline_max_agents: u32 = 20;

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
                        if let Some(next_name) =
                            handle_fallback(&profile, &self.registry, &mut current_agent)
                        {
                            current_agent = next_name;
                        } else {
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

            match runner.run(&current_input, agent_max_turns) {
                Ok(output) => {
                    let duration_ms = start.elapsed().as_millis() as u64;

                    // Evaluate handoff rules to determine next agent(s)
                    let next = if profile.handoff.handoff_rules.is_empty() {
                        profile.handoff.fallback.clone()
                    } else {
                        eval_rules(&profile.handoff.handoff_rules, &output)
                    };

                    // Save session log to shared memory
                    self.save_memory_entry(&profile, &output);

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
                        Some(HandoffTarget::Single(n)) => {
                            if let Some(ref bus) = self.event_bus {
                                bus.send(PipelineEvent::Handoff {
                                    from: profile.agent.name.clone(),
                                    to: HandoffTarget::Single(n.clone()),
                                });
                            }
                            current_input = output.to_string();
                            current_agent = n;
                        }
                        Some(HandoffTarget::Fork(targets)) => {
                            if let Some(ref bus) = self.event_bus {
                                bus.send(PipelineEvent::Fork {
                                    from: profile.agent.name.clone(),
                                    branches: targets.clone(),
                                });
                            }

                            // Run fork branches in parallel
                            let fork_results = self.run_fork(
                                &targets,
                                &output.to_string(),
                                agent_max_turns,
                                &mut errors,
                                &mut skipped,
                            );

                            // Merge fork outputs into a single JSON
                            let merged = self.merge_fork_outputs(&fork_results);

                            // Find a common next agent from the fork branches
                            // (use the first non-None next target, or fallback from the originating profile)
                            let fork_next = fork_results
                                .iter()
                                .find_map(|r| r.next.clone())
                                .or_else(|| profile.handoff.fallback.clone());

                            current_input = merged.to_string();

                            match fork_next {
                                Some(HandoffTarget::Single(n)) => {
                                    current_agent = n;
                                }
                                Some(HandoffTarget::Fork(_)) => {
                                    // Nested forks — continue the outer loop
                                    current_agent =
                                        fork_results[0].next.clone().unwrap().agent_names()[0]
                                            .to_string();
                                }
                                None => {
                                    completed = true;
                                    break;
                                }
                            }
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
                        if let Some(next_name) =
                            handle_fallback(&profile, &self.registry, &mut current_agent)
                        {
                            current_agent = next_name;
                        } else {
                            break;
                        }
                        continue;
                    } else {
                        errors.push(err);
                        break;
                    }
                }
            }

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

    /// Run fork branches in parallel using std::thread::spawn.
    fn run_fork(
        &self,
        targets: &[String],
        input: &str,
        max_turns: u32,
        errors: &mut Vec<AgentError>,
        skipped: &mut Vec<String>,
    ) -> Vec<AgentResult> {
        let registry = self.registry.clone();
        let memory_box = self.shared_memory.clone();

        type ForkBranchOutput = (
            String,
            Result<(serde_json::Value, u64, Option<HandoffTarget>), AgentError>,
        );

        let handles: Vec<std::thread::JoinHandle<ForkBranchOutput>> = targets
            .iter()
            .map(|agent_name| {
                let agent_name = agent_name.clone();
                let input = input.to_string();
                let registry = registry.clone();
                let memory_box = memory_box.clone();

                std::thread::spawn(move || {
                    let name_for_msg = agent_name.clone();
                    let profile = match registry.get(&agent_name) {
                        Some(p) => p.clone(),
                        None => {
                            let msg = format!("Agent '{}' not found in registry", agent_name);
                            return (
                                agent_name,
                                Err(AgentError {
                                    agent_name: name_for_msg,
                                    message: msg,
                                    skippable: false,
                                }),
                            );
                        }
                    };

                    let resolved = match artemis_core::resolve(&profile.agent.model) {
                        Ok(r) => r,
                        Err(e) => {
                            let msg = format!("Resolve failed: {}", e);
                            return (
                                agent_name,
                                Err(AgentError {
                                    agent_name: name_for_msg,
                                    message: msg,
                                    skippable: profile.agent.skippable,
                                }),
                            );
                        }
                    };

                    let mut agent = Agent::new(resolved);
                    if let Some(ref mem) = memory_box {
                        agent = agent.with_memory(mem.clone_box());
                    }

                    let mut runner = AgentRunner::from_profile(profile.clone(), agent);
                    if let Some(ref mem) = memory_box {
                        runner = runner.with_memory(Arc::clone(mem));
                    }

                    let max_turns = profile.handoff.max_turns.unwrap_or(max_turns);

                    let start = Instant::now();
                    match runner.run(&input, max_turns) {
                        Ok(output) => {
                            let duration_ms = start.elapsed().as_millis() as u64;
                            let next = if profile.handoff.handoff_rules.is_empty() {
                                profile.handoff.fallback.clone()
                            } else {
                                eval_rules(&profile.handoff.handoff_rules, &output)
                            };
                            (agent_name, Ok((output, duration_ms, next)))
                        }
                        Err(e) => (
                            agent_name,
                            Err(AgentError {
                                agent_name: name_for_msg,
                                message: e.to_string(),
                                skippable: profile.agent.skippable,
                            }),
                        ),
                    }
                })
            })
            .collect();

        // Collect results
        let mut fork_results = Vec::new();
        for (i, handle) in handles.into_iter().enumerate() {
            let (agent_name, result) = match handle.join() {
                Ok(v) => v,
                Err(_) => panic!("Fork branch thread {} panicked", i),
            };
            match result {
                Ok((output, duration_ms, next)) => {
                    fork_results.push(AgentResult {
                        agent_name,
                        output,
                        next,
                        duration_ms,
                    });
                }
                Err(err) => {
                    if err.skippable {
                        skipped.push(err.agent_name.clone());
                        errors.push(err);
                    } else {
                        errors.push(err);
                        // Non-skippable error in a fork branch stops the whole pipeline
                        return fork_results;
                    }
                }
            }
        }

        fork_results
    }

    /// Merge fork branch outputs into a single JSON: {branch_name: output}
    fn merge_fork_outputs(&self, fork_results: &[AgentResult]) -> serde_json::Value {
        let mut merged = serde_json::Map::new();
        for r in fork_results {
            merged.insert(r.agent_name.clone(), r.output.clone());
        }
        serde_json::Value::Object(merged)
    }

    /// Save a session log entry to shared memory.
    fn save_memory_entry(&self, profile: &AgentProfile, output: &serde_json::Value) {
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
    }

    /// Dry-run: validate the pipeline chain without calling any LLM.
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
                    report
                        .issues
                        .push(format!("Agent '{}' not found in registry", current));
                    break;
                }
            };

            report.agents_in_chain.push(profile.agent.name.clone());

            // Check each handoff rule target
            for (i, rule) in profile.handoff.handoff_rules.iter().enumerate() {
                if let Some(ref target) = rule.target {
                    for name in target.agent_names() {
                        if self.registry.get(name).is_none() {
                            report.issues.push(format!(
                                "Agent '{}' rule[{}] targets '{}' which is not registered",
                                profile.agent.name, i, name
                            ));
                        }
                    }
                }
            }

            // Check fallback target
            if let Some(ref fallback) = profile.handoff.fallback {
                for name in fallback.agent_names() {
                    if self.registry.get(name).is_none() {
                        report.issues.push(format!(
                            "Agent '{}' fallback '{}' is not registered",
                            profile.agent.name, name
                        ));
                    }
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

            match &next {
                Some(HandoffTarget::Single(n)) => {
                    if self.registry.get(n).is_none() {
                        report.issues.push(format!(
                            "Default route from '{}' -> '{}' targets unregistered agent",
                            profile.agent.name, n
                        ));
                        break;
                    }
                    current = n.clone();
                }
                Some(HandoffTarget::Fork(targets)) => {
                    // Validate each fork branch, then follow the first one for chain analysis
                    for name in targets {
                        if self.registry.get(name).is_none() {
                            report.issues.push(format!(
                                "Fork from '{}' -> '{}' targets unregistered agent",
                                profile.agent.name, name
                            ));
                        }
                    }
                    // For chain analysis, follow the first fork target
                    if let Some(first) = targets.first() {
                        if self.registry.get(first).is_some() {
                            current = first.clone();
                        } else {
                            break;
                        }
                    }
                }
                None => {
                    report.reachable_end = true;
                    break;
                }
            }
        }

        if report.agents_in_chain.len() >= 100 {
            report
                .issues
                .push("Chain exceeded 100 steps (infinite loop?)".into());
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

/// Try to route to the fallback agent. Returns the fallback agent name if valid.
fn handle_fallback(
    profile: &AgentProfile,
    registry: &AgentRegistry,
    current_agent: &mut String,
) -> Option<String> {
    if let Some(ref fallback) = profile.handoff.fallback {
        match fallback {
            HandoffTarget::Single(name) => {
                if registry.get(name).is_some() {
                    *current_agent = name.clone();
                    return Some(name.clone());
                }
            }
            HandoffTarget::Fork(names) => {
                // For fallback, use the first valid fork target
                if let Some(first) = names.first() {
                    if registry.get(first).is_some() {
                        *current_agent = first.clone();
                        return Some(first.clone());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff_rule::HandoffRule;
    use std::sync::Arc;

    fn test_registry() -> Arc<AgentRegistry> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("artemis_test_dry_run_{ts}"));
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

    fn test_registry_with_fork() -> Arc<AgentRegistry> {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("artemis_test_fork_{ts}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("code-review")).unwrap();
        std::fs::create_dir_all(dir.join("security")).unwrap();
        std::fs::create_dir_all(dir.join("performance")).unwrap();
        std::fs::create_dir_all(dir.join("merge")).unwrap();

        std::fs::write(
            dir.join("code-review/agent.toml"),
            r#"
[agent]
name = "code-review"
model = "sonnet"

[system]
prompt = "Test"

[[handoff.rules]]
condition = { field = "confidence", op = ">", value = "0.5" }
target = "fork:security,performance"

[[handoff.rules]]
default = true
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("security/agent.toml"),
            r#"
[agent]
name = "security"
model = "sonnet"

[system]
prompt = "Test"

[handoff]

[[handoff.rules]]
target = "merge"
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("performance/agent.toml"),
            r#"
[agent]
name = "performance"
model = "sonnet"

[system]
prompt = "Test"

[handoff]

[[handoff.rules]]
target = "merge"
"#,
        )
        .unwrap();

        std::fs::write(
            dir.join("merge/agent.toml"),
            r#"
[agent]
name = "merge"
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

    #[test]
    fn test_dry_run_fork_valid() {
        let registry = test_registry_with_fork();
        let pipeline = Pipeline::new("test", registry, None, None);
        let report = pipeline.dry_run("code-review");
        // Fork targets security and performance are registered, merge is registered
        assert!(report.issues.is_empty());
    }

    #[test]
    fn test_dry_run_fork_invalid_target() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("artemis_test_fork_invalid_{ts}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("starter")).unwrap();

        std::fs::write(
            dir.join("starter/agent.toml"),
            r#"
[agent]
name = "starter"
model = "sonnet"

[system]
prompt = "Test"

[handoff]

[[handoff.rules]]
default = true
target = "fork:missing-a,missing-b"
"#,
        )
        .unwrap();

        let registry = Arc::new(AgentRegistry::load_dir(&dir).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let pipeline = Pipeline::new("test", registry, None, None);
        let report = pipeline.dry_run("starter");
        assert!(!report.issues.is_empty());
        assert!(!report.valid);
    }

    #[test]
    fn test_merge_fork_outputs() {
        let pipeline = Pipeline::new(
            "test",
            Arc::new(AgentRegistry::load_dir(std::path::Path::new("/tmp/nonexistent")).unwrap()),
            None,
            None,
        );
        let fork_results = vec![
            AgentResult {
                agent_name: "security".into(),
                output: serde_json::json!({"issues": ["sql-injection"]}),
                next: None,
                duration_ms: 100,
            },
            AgentResult {
                agent_name: "performance".into(),
                output: serde_json::json!({"score": 85}),
                next: None,
                duration_ms: 200,
            },
        ];
        let merged = pipeline.merge_fork_outputs(&fork_results);
        assert_eq!(merged["security"]["issues"][0], "sql-injection");
        assert_eq!(merged["performance"]["score"], 85);
    }
}
