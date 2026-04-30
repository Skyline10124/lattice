use std::sync::Arc;
use std::sync::LazyLock;

use artemis_agent::Agent;
use artemis_memory::{EntryKind, Memory};
use tracing::warn;

use crate::profile::AgentProfile;

// ---------------------------------------------------------------------------
// AgentRunner — wires AgentProfile + Agent
// ---------------------------------------------------------------------------

/// Shared tokio runtime for async Memory operations.
static MEMORY_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Runtime::new().expect("Failed to create memory tokio runtime")
});

/// Max retries for JSON schema validation failures.
const MAX_SCHEMA_RETRIES: u32 = 2;

/// A runner that executes an agent per its profile.
pub struct AgentRunner {
    pub profile: AgentProfile,
    pub agent: Agent,
    pub shared_memory: Option<Arc<dyn Memory>>,
}

impl AgentRunner {
    /// Create a runner from a profile and resolved agent.
    pub fn from_profile(profile: AgentProfile, agent: Agent) -> Self {
        Self {
            profile,
            agent,
            shared_memory: None,
        }
    }

    /// Attach shared memory for implicit recall before each run.
    pub fn with_memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.shared_memory = Some(memory);
        self
    }

    /// Run the agent with the given input. Returns the JSON output.
    ///
    /// If `output_schema` is configured on the profile, the output is validated
    /// against the JSON Schema and retried with format hints on failure.
    ///
    /// Handoff routing is NOT done here — the caller (Pipeline) evaluates
    /// `handoff_rules` against the returned output to determine the next agent.
    pub fn run(
        &mut self,
        input: &str,
        max_turns: u32,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let schema = self
            .profile
            .handoff
            .output_schema
            .as_ref()
            .and_then(|s| {
                match serde_json::from_str::<serde_json::Value>(s) {
                    Ok(schema_json) => {
                        match jsonschema::validator_for(&schema_json) {
                            Ok(validator) => Some((schema_json, validator)),
                            Err(e) => {
                                warn!("Invalid output_schema for {}: {e}", self.profile.agent.name);
                                None
                            }
                        }
                    }
                    Err(e) => {
                        warn!("output_schema is not valid JSON for {}: {e}", self.profile.agent.name);
                        None
                    }
                }
            });

        let enriched_input = self.enrich_input(input);
        let mut output = self.run_once(&enriched_input, max_turns)?;

        // JSON Schema validation + retry loop
        if let Some((ref schema_json, ref validator)) = schema {
            for retry in 0..MAX_SCHEMA_RETRIES {
                let mut errors = validator.iter_errors(&output);
                let first_error = errors.next();

                if first_error.is_none() {
                    break; // Valid ✓
                }

                let error_messages: Vec<String> = std::iter::once(first_error.unwrap())
                    .chain(errors)
                    .take(3)
                    .map(|e| format!("- {}", e))
                    .collect();

                warn!(
                    "Output validation failed for {} (attempt {}/{}):\n{}",
                    self.profile.agent.name,
                    retry + 1,
                    MAX_SCHEMA_RETRIES,
                    error_messages.join("\n")
                );

                let correction_hint = format!(
                    "{}\n\nYour previous response did not match the required JSON format. \
                     Errors:\n{}\n\nExpected schema:\n{}\n\nPlease correct your response. \
                     Return ONLY valid JSON that matches the schema, no markdown.",
                    enriched_input,
                    error_messages.join("\n"),
                    serde_json::to_string_pretty(schema_json).unwrap_or_default()
                );

                output = self.run_once(&correction_hint, max_turns)?;
            }
        }

        Ok(output)
    }

    /// Enrich input with memory recall context.
    fn enrich_input(&self, input: &str) -> String {
        if let Some(ref mem) = self.shared_memory {
            let recall = if let Ok(handle) = tokio::runtime::Handle::try_current() {
                tokio::task::block_in_place(|| handle.block_on(mem.recall(input, 5)))
            } else {
                MEMORY_RT.block_on(mem.recall(input, 5))
            };

            if !recall.is_empty() {
                let context: String = recall
                    .iter()
                    .map(|e| {
                        format!(
                            "- {}: {} (session: {})",
                            match e.kind {
                                EntryKind::Fact => "Fact",
                                EntryKind::Decision => "Decision",
                                _ => "Log",
                            },
                            e.summary,
                            e.session_id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                return format!(
                    "[Relevant past context from memory:]\n{}\n\n[Current task:]\n{}",
                    context, input
                );
            }
        }
        input.to_string()
    }

    /// Run the agent once and parse the output as JSON.
    fn run_once(
        &mut self,
        input: &str,
        max_turns: u32,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let events = self.agent.run(input, max_turns);
        let mut content = String::new();
        for event in &events {
            if let artemis_agent::LoopEvent::Token { text } = event {
                content.push_str(text);
            }
        }

        // Try to parse as JSON. Strip markdown code fences if present.
        let trimmed = content.trim();
        let json_str = if trimmed.starts_with("```") {
            // Find the end of the opening fence and strip closing fence
            trimmed
                .lines()
                .skip(1) // skip ```json or ```
                .collect::<Vec<_>>()
                .join("\n")
                .trim_end_matches("```")
                .trim()
                .to_string()
        } else {
            trimmed.to_string()
        };

        let output: serde_json::Value =
            serde_json::from_str(&json_str).unwrap_or_else(|_| {
                serde_json::json!({"content": content})
            });

        Ok(output)
    }
}
