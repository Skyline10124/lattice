use std::sync::Arc;

use lattice_agent::memory::Memory;
use lattice_agent::Agent;
use tracing::warn;

use crate::json_output::{parse_assistant_output, CompiledOutputSchema, MAX_SCHEMA_RETRIES};
use crate::profile::AgentProfile;

// ---------------------------------------------------------------------------
// AgentRunner — wires AgentProfile + Agent
// ---------------------------------------------------------------------------

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

    /// Access the inner Agent for bus registration or direct manipulation.
    pub fn agent(&self) -> &Agent {
        &self.agent
    }

    pub fn agent_mut(&mut self) -> &mut Agent {
        &mut self.agent
    }

    /// Run the agent with the given input. Returns the JSON output.
    ///
    /// If `output_schema` is configured on the profile, the output is validated
    /// against the JSON Schema and retried with format hints on failure.
    ///
    /// Handoff routing is NOT done here — the caller (Pipeline) evaluates
    /// `handoff_rules` against the returned output to determine the next agent.
    pub async fn run(
        &mut self,
        input: &str,
        max_turns: u32,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let schema = self
            .profile
            .handoff
            .output_schema
            .as_ref()
            .map(|schema| CompiledOutputSchema::compile(&self.profile.agent.name, schema))
            .transpose()
            .map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;

        let mut output = self.run_once(input, max_turns).await?;

        // JSON Schema validation + retry loop
        if let Some(ref schema) = schema {
            let mut schema_valid = false;
            for retry in 0..MAX_SCHEMA_RETRIES {
                let error_messages = schema.validation_errors(&output);
                if error_messages.is_empty() {
                    schema_valid = true;
                    break; // Valid ✓
                }

                warn!(
                    "Output validation failed for {} (attempt {}/{}):\n{}",
                    self.profile.agent.name,
                    retry + 1,
                    MAX_SCHEMA_RETRIES,
                    error_messages.join("\n")
                );

                let correction_hint = schema.correction_hint(input, &error_messages);

                output = self.run_once(&correction_hint, max_turns).await?;
            }

            if !schema_valid {
                return Err(format!(
                    "Agent '{}' output still fails schema validation after {} retries",
                    self.profile.agent.name, MAX_SCHEMA_RETRIES
                )
                .into());
            }
        }

        Ok(output)
    }

    /// Run the agent once and parse the output as JSON.
    async fn run_once(
        &mut self,
        input: &str,
        max_turns: u32,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let events = self.agent.run(input, max_turns).await;
        // Propagate error events immediately
        for event in &events {
            if let lattice_agent::LoopEvent::Error { message, .. } = event {
                return Err(format!("Agent error: {}", message).into());
            }
        }
        let mut content = String::new();
        for event in &events {
            if let lattice_agent::LoopEvent::Token { text } = event {
                content.push_str(text);
            }
        }

        parse_assistant_output(&content, self.profile.handoff.output_schema.is_some())
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use crate::json_output::looks_like_error;

    #[test]
    fn detects_provider_and_transport_error_text() {
        assert!(looks_like_error(
            r#"{"error":{"message":"invalid_api_key"}}"#
        ));
        assert!(looks_like_error(
            "503 service unavailable: upstream overloaded"
        ));
        assert!(looks_like_error("Network error: connection reset by peer"));
        assert!(looks_like_error("context_length_exceeded: prompt too long"));
    }

    #[test]
    fn does_not_treat_normal_text_as_error() {
        assert!(!looks_like_error(
            "Here is the requested implementation summary."
        ));
    }
}
