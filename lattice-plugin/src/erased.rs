use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;

use lattice_core::types::ToolDefinition;

use crate::Plugin;
use crate::PluginError;

/// Type-erased Plugin. Accepts and returns serde_json::Value instead of
/// typed Input/Output. Used by PluginRegistry for heterogeneous storage.
pub trait ErasedPlugin: Send + Sync {
    fn name(&self) -> &str;
    fn system_prompt(&self) -> &str;
    fn system_prompt_delta(&self) -> Option<lattice_agent::prompt::SystemPromptDelta> {
        let prompt = self.system_prompt();
        if prompt.trim().is_empty() {
            None
        } else {
            Some(lattice_agent::prompt::SystemPromptDelta::agent(
                prompt,
                HashMap::new(),
            ))
        }
    }
    fn to_prompt_json(&self, context: &serde_json::Value) -> Result<String, PluginError>;
    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError>;
    fn tools(&self) -> &[ToolDefinition];
    fn preferred_model(&self) -> &str;
    fn output_schema(&self) -> Option<serde_json::Value>;
}

impl<T: Plugin> ErasedPlugin for T
where
    T::Input: DeserializeOwned,
    T::Output: Serialize,
{
    fn name(&self) -> &str {
        Plugin::name(self)
    }

    fn system_prompt(&self) -> &str {
        Plugin::system_prompt(self)
    }

    fn system_prompt_delta(&self) -> Option<lattice_agent::prompt::SystemPromptDelta> {
        Plugin::system_prompt_delta(self)
    }

    fn to_prompt_json(&self, context: &serde_json::Value) -> Result<String, PluginError> {
        let typed: T::Input = serde_json::from_value(context.clone()).map_err(|e| {
            PluginError::Parse(format!(
                "{}: failed to deserialize input from context: {}",
                self.name(),
                e
            ))
        })?;
        Ok(self.to_prompt(&typed))
    }

    fn parse_output_json(&self, raw: &str) -> Result<serde_json::Value, PluginError> {
        let typed = self.parse_output(raw)?;
        serde_json::to_value(typed).map_err(|e| {
            PluginError::Parse(format!(
                "{}: failed to serialize output: {}",
                self.name(),
                e
            ))
        })
    }

    fn tools(&self) -> &[ToolDefinition] {
        Plugin::tools(self)
    }

    fn preferred_model(&self) -> &str {
        Plugin::preferred_model(self)
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        Plugin::output_schema(self)
    }
}
