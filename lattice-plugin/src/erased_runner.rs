use lattice_core::retry::RetryPolicy;

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
    let prompt = plugin.to_prompt_json(context)?;
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
        let raw = agent
            .send_message_with_tools(&prompt)
            .await
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
