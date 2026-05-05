use lattice_core::retry::RetryPolicy;
use std::collections::HashMap;

use crate::erased::ErasedPlugin;
use crate::{
    extract_confidence, save_memory_entries, Action, PluginConfig, PluginError, PluginHooks,
    RunResult,
};

/// Shared PluginRunner run loop used by both typed PluginRunner and
/// type-erased ErasedPluginRunner.
///
/// Returns PluginError — this crate does NOT know about DAGError.
#[allow(clippy::too_many_arguments)]
pub async fn run_plugin_loop(
    plugin: &dyn ErasedPlugin,
    behavior: &dyn crate::Behavior,
    agent: &mut dyn lattice_agent::PluginAgent,
    context: &serde_json::Value,
    config: &PluginConfig,
    hooks: Option<&dyn PluginHooks>,
    retry_policy: Option<&RetryPolicy>,
    memory: Option<&dyn lattice_agent::memory::Memory>,
) -> Result<RunResult, PluginError> {
    agent.set_system_prompt_delta(plugin.system_prompt_delta());

    // Inject plugin-specific tool definitions
    let plugin_tools = plugin.tools().to_vec();
    if !plugin_tools.is_empty() {
        agent.add_tools(plugin_tools);
    }

    // Log preferred model hint (the caller decides whether to honour it)
    let preferred = plugin.preferred_model();
    if !preferred.is_empty() {
        tracing::debug!(
            plugin = plugin.name(),
            model = preferred,
            "plugin prefers a specific model — external agent may override"
        );
    }

    let schema = plugin.output_schema();
    agent.set_output_contract_delta(schema.as_ref().map(|schema_json| {
        let output_schema =
            serde_json::to_string_pretty(schema_json).unwrap_or_else(|_| schema_json.to_string());
        lattice_agent::prompt::SystemPromptDelta::contract(
            "Output contract:\nReturn only valid JSON matching this schema:\n{{output_schema}}",
            HashMap::from([("output_schema".to_string(), output_schema)]),
        )
    }));

    let mut prompt = plugin.to_prompt_json(context)?;
    let mut attempt = 0u32;

    if let Some(h) = hooks {
        h.on_start(plugin.name(), (prompt.len() as u32).div_ceil(4));
    }

    loop {
        if attempt >= config.max_turns {
            return Err(PluginError::MaxTurnsExceeded(config.max_turns));
        }

        // L1 retry (chat_with_retry) handled inside Agent::run()
        // L2 retry (behavior loop) handled here

        // Apply timeout from config
        let timeout_duration = std::time::Duration::from_secs(config.timeout_per_call_secs);
        let raw = tokio::time::timeout(timeout_duration, agent.send_message_with_tools(&prompt))
            .await
            .map_err(|_| {
                PluginError::Other(format!(
                    "plugin '{}' timed out after {}s on attempt {}",
                    plugin.name(),
                    config.timeout_per_call_secs,
                    attempt,
                ))
            })?
            .map_err(|e| PluginError::Other(e.to_string()))?;

        match plugin.parse_output_json(&raw) {
            Ok(output) => {
                let confidence = extract_confidence(&raw);
                let action = behavior.decide(confidence);

                if let Some(h) = hooks {
                    h.on_turn(attempt, None, &action);
                }

                match action {
                    Action::Done => {
                        let json = serde_json::to_string(&output)
                            .map_err(|e| PluginError::Other(e.to_string()))?;
                        if json.len() > config.max_output_bytes {
                            return Err(PluginError::OutputTooLarge(
                                json.len(),
                                config.max_output_bytes,
                            ));
                        }
                        let result = RunResult {
                            output: json,
                            turns: attempt + 1,
                            final_action: Action::Done,
                        };
                        if let Some(h) = hooks {
                            h.on_complete(&result);
                        }
                        if let Some(mem) = memory {
                            save_memory_entries(mem, plugin.name(), &prompt, &result);
                        }
                        return Ok(result);
                    }
                    Action::Retry => {
                        attempt += 1;
                        if let Some(p) = retry_policy {
                            tokio::time::sleep(p.jittered_backoff(attempt)).await;
                        }
                    }
                }
            }
            Err(e) => {
                if let Some(h) = hooks {
                    h.on_error(attempt, &e);
                }
                match behavior.on_error(&e, attempt) {
                    crate::ErrorAction::Retry => {
                        // Append structured correction hint so the model
                        // doesn't repeat the same error on the next attempt.
                        let mut hint = String::new();
                        hint.push_str("\n\n[SYSTEM: The previous response could not be parsed.");
                        hint.push_str(&format!("\nParse error: {}", e));
                        // Truncate raw output to avoid blowing the context window
                        let truncated = if raw.len() > 2000 {
                            format!("{} ... [truncated {} bytes]", &raw[..2000], raw.len())
                        } else {
                            raw.clone()
                        };
                        hint.push_str(&format!("\nRaw output received:\n{}", truncated));
                        if let Some(ref s) = schema {
                            let schema_str = serde_json::to_string(s).unwrap_or_default();
                            let schema_truncated = if schema_str.len() > 1000 {
                                format!("{} ... [truncated]", &schema_str[..1000])
                            } else {
                                schema_str
                            };
                            hint.push_str(&format!(
                                "\nExpected output schema:\n{}",
                                schema_truncated
                            ));
                        }
                        hint.push_str("\nPlease correct the output to match the expected format.]");
                        prompt.push_str(&hint);

                        attempt += 1;
                        if let Some(p) = retry_policy {
                            tokio::time::sleep(p.jittered_backoff(attempt)).await;
                        }
                    }
                    crate::ErrorAction::Abort => return Err(e),
                    crate::ErrorAction::Escalate => {
                        return Err(PluginError::Escalated {
                            original: Box::new(e),
                            after_attempts: attempt,
                        });
                    }
                }
            }
        }
    }
}

/// Type-erased PluginRunner. Works with &dyn ErasedPlugin and &dyn PluginAgent.
pub struct ErasedPluginRunner<'a> {
    pub plugin: &'a dyn ErasedPlugin,
    pub behavior: &'a dyn crate::Behavior,
    pub agent: &'a mut dyn lattice_agent::PluginAgent,
    pub config: &'a PluginConfig,
    pub hooks: Option<&'a dyn PluginHooks>,
    pub retry_policy: Option<&'a RetryPolicy>,
    pub memory: Option<&'a dyn lattice_agent::memory::Memory>,
}

impl<'a> ErasedPluginRunner<'a> {
    pub fn new(
        plugin: &'a dyn ErasedPlugin,
        behavior: &'a dyn crate::Behavior,
        agent: &'a mut dyn lattice_agent::PluginAgent,
        config: &'a PluginConfig,
        hooks: Option<&'a dyn PluginHooks>,
        retry_policy: Option<&'a RetryPolicy>,
        memory: Option<&'a dyn lattice_agent::memory::Memory>,
    ) -> Self {
        Self {
            plugin,
            behavior,
            agent,
            config,
            hooks,
            retry_policy,
            memory,
        }
    }

    pub async fn run(&mut self, context: &serde_json::Value) -> Result<RunResult, PluginError> {
        run_plugin_loop(
            self.plugin,
            self.behavior,
            self.agent,
            context,
            self.config,
            self.hooks,
            self.retry_policy,
            self.memory,
        )
        .await
    }
}
