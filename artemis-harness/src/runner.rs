use artemis_agent::Agent;

use crate::handoff::run_python_handoff;
use crate::profile::AgentProfile;

// ---------------------------------------------------------------------------
// AgentRunner — wires AgentProfile + Agent + Python handoff
// ---------------------------------------------------------------------------

/// A runner that uses an AgentProfile to create and run an Agent.
pub struct AgentRunner {
    pub profile: AgentProfile,
    pub agent: Agent,
    pub handoff_script: Option<String>,
}

impl AgentRunner {
    /// Create a runner from a profile, resolving the model and loading tools/handoff.
    pub fn from_profile(profile: AgentProfile, agent: Agent) -> Self {
        let handoff_script = profile
            .handoff
            .handoff_file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok());

        Self {
            profile,
            agent,
            handoff_script,
        }
    }

    /// Run the agent with the given input. Returns the output and optional next agent.
    pub fn run(
        &mut self,
        input: &str,
    ) -> Result<(serde_json::Value, Option<String>), Box<dyn std::error::Error>> {
        let events = self.agent.run(input, 10);
        // Extract text content from events
        let mut content = String::new();
        for event in &events {
            if let artemis_agent::LoopEvent::Token { text } = event {
                content.push_str(text);
            }
        }

        // Parse as JSON
        let output: serde_json::Value = serde_json::from_str(&content)
            .unwrap_or_else(|_| serde_json::json!({"content": content}));

        // Run handoff if configured
        let next = if let Some(ref script) = self.handoff_script {
            // Write script to temp file so run_python_handoff can read it
            let tmp = std::env::temp_dir()
                .join(format!("artemis_handoff_{}.py", self.profile.agent.name));
            std::fs::write(&tmp, script)?;
            let result = run_python_handoff(&tmp, &output, 0.8)?; // default confidence
            let _ = std::fs::remove_file(&tmp);
            result
        } else {
            self.profile.handoff.fallback.clone()
        };

        Ok((output, next))
    }
}
