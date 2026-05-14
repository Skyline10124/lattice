use async_trait::async_trait;

use crate::{prompt, Agent, LoopEvent};

/// Tool loop max turns per Agent::run() call for send_message_with_tools.
const MAX_TOOL_TURNS: u32 = 10;

/// Minimal interface for an LLM-calling agent.
/// Used by PluginRunner to call any agent that implements send + system_prompt.
#[async_trait(?Send)]
pub trait PluginAgent {
    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>>;
    /// Send a user message and automatically handle tool calls via Agent::run().
    async fn send_message_with_tools(
        &mut self,
        message: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        // Default: delegate to send() for backward compat with non-Agent impls
        self.send(message).await
    }
    fn set_system_prompt(&mut self, _prompt: &str) {}
    fn set_system_prompt_delta(&mut self, _delta: Option<crate::prompt::SystemPromptDelta>) {}
    fn set_output_contract_delta(&mut self, _delta: Option<crate::prompt::SystemPromptDelta>) {}
    fn add_tools(&mut self, _tools: Vec<lattice_core::types::ToolDefinition>) {}
    fn token_usage(&self) -> u64 {
        0
    }
}

#[async_trait(?Send)]
impl PluginAgent for Agent {
    fn set_system_prompt(&mut self, prompt: &str) {
        Agent::set_system_prompt(self, prompt);
    }

    fn set_system_prompt_delta(&mut self, delta: Option<prompt::SystemPromptDelta>) {
        Agent::set_system_prompt_delta(self, delta);
    }

    fn set_output_contract_delta(&mut self, delta: Option<prompt::SystemPromptDelta>) {
        Agent::set_output_contract_delta(self, delta);
    }

    async fn send(&mut self, message: &str) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.send_message(message).await;
        let mut content = String::new();
        let mut has_error = false;
        for event in &events {
            match event {
                LoopEvent::Token { text } => content.push_str(text),
                LoopEvent::Error { .. } => has_error = true,
                _ => {}
            }
        }
        if has_error && content.is_empty() {
            Err("Agent returned an error with no content".into())
        } else {
            if has_error {
                tracing::warn!("Agent returned content but also had error event(s)");
            }
            Ok(content)
        }
    }

    async fn send_message_with_tools(
        &mut self,
        message: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let events = self.run(message, MAX_TOOL_TURNS).await;
        let mut content = String::new();
        let mut has_error = false;
        let mut error_messages = Vec::new();
        for event in &events {
            match event {
                LoopEvent::Token { text } => content.push_str(text),
                LoopEvent::Error { message, .. } => {
                    has_error = true;
                    error_messages.push(message.clone());
                }
                _ => {}
            }
        }
        if has_error && content.is_empty() {
            Err(format!("Agent errors: {}", error_messages.join("; ")).into())
        } else {
            if has_error {
                tracing::warn!(
                    "Agent returned content but also had error(s): {}",
                    error_messages.join("; ")
                );
            }
            Ok(content)
        }
    }
}
