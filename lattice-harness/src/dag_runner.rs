use std::sync::Arc;

use lattice_agent::memory::Memory;
use lattice_agent::Agent;
use lattice_core::retry::RetryPolicy;
use lattice_plugin::erased_runner::run_plugin_loop;
use lattice_plugin::PluginConfig;

use crate::handoff_rule::HandoffTarget;
use crate::profile::PluginsConfig;
use crate::tools::ToolRegistry;

/// Slot transition limit. Not LLM calls — slot→slot transfers.
/// Orthogonal to PluginSlotConfig.max_turns (LLM turns within a slot).
const MAX_DAG_SLOT_TRANSITIONS: u32 = 50;

// ---------------------------------------------------------------------------
// DAGError
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum DAGError {
    #[error("entry slot '{0}' not found in [plugins.slots]")]
    EntrySlotNotFound(String),

    #[error("slot '{0}' not found")]
    SlotNotFound(String),

    #[error("plugin '{0}' not registered in PluginRegistry")]
    PluginNotFound(String),

    #[error("model resolve failed: {0}")]
    Resolve(#[from] lattice_core::errors::LatticeError),

    #[error("max slot transitions ({0}) exceeded — possible infinite DAG loop")]
    MaxSlotTransitionsExceeded(u32),

    #[error("plugin error in slot '{slot}': {source}")]
    Plugin {
        slot: String,
        #[source]
        source: lattice_plugin::PluginError,
    },

    #[error("output JSON parse failed: {0}")]
    OutputParse(String),

    #[error("fork not supported in intra-agent DAG — use Pipeline fork:target")]
    ForkNotSupportedInDag,

    #[error("plugin registry not configured")]
    MissingPluginRegistry,

    #[error("tool registry not configured")]
    MissingToolRegistry,
}

impl DAGError {
    pub(crate) fn plugin_error(slot: &str, err: lattice_plugin::PluginError) -> Self {
        DAGError::Plugin {
            slot: slot.into(),
            source: err,
        }
    }
}

// ---------------------------------------------------------------------------
// PluginDagRunner
// ---------------------------------------------------------------------------

pub struct PluginDagRunner<'a> {
    config: &'a PluginsConfig,
    plugin_registry: &'a lattice_plugin::registry::PluginRegistry,
    tool_registry: &'a ToolRegistry,
    retry_policy: RetryPolicy,
    shared_memory: Option<Arc<dyn Memory>>,
}

impl<'a> PluginDagRunner<'a> {
    pub fn new(
        config: &'a PluginsConfig,
        plugin_registry: &'a lattice_plugin::registry::PluginRegistry,
        tool_registry: &'a ToolRegistry,
        retry_policy: RetryPolicy,
        shared_memory: Option<Arc<dyn Memory>>,
    ) -> Self {
        Self {
            config,
            plugin_registry,
            tool_registry,
            retry_policy,
            shared_memory,
        }
    }

    /// Traverse edges in TOML definition order.
    /// First edge where from == current AND rule.eval(output) == true wins.
    /// Returns the edge's rule.target. None = DAG endpoint (no matching edge).
    pub fn find_edge(&self, from: &str, output: &serde_json::Value) -> Option<HandoffTarget> {
        self.config
            .edges
            .iter()
            .filter(|e| e.from == from)
            .find(|e| e.rule.eval(output))
            .and_then(|e| e.rule.target.clone())
    }

    pub async fn run(
        &mut self,
        initial_input: &str,
        default_model: &str,
    ) -> Result<serde_json::Value, DAGError> {
        let mut context = serde_json::json!({"input": initial_input});
        let mut current_name = self.config.entry.clone();
        let mut transitions = 0u32;

        loop {
            if transitions >= MAX_DAG_SLOT_TRANSITIONS {
                return Err(DAGError::MaxSlotTransitionsExceeded(
                    MAX_DAG_SLOT_TRANSITIONS,
                ));
            }

            let slot = self
                .config
                .slots
                .iter()
                .find(|s| s.name == current_name)
                .ok_or_else(|| DAGError::SlotNotFound(current_name.clone()))?;

            let bundle = self
                .plugin_registry
                .get(&slot.plugin)
                .ok_or_else(|| DAGError::PluginNotFound(slot.plugin.clone()))?;

            let model = slot.model_override.as_deref().unwrap_or(default_model);
            let resolved = lattice_core::resolve(model)?;
            let mut agent = Agent::new(resolved);
            agent.set_system_prompt(bundle.plugin.system_prompt());

            let tools = crate::tools::merge_tool_definitions(
                self.tool_registry,
                &self.config.shared_tools,
                &slot.tools,
                bundle.plugin.tools(),
            );
            agent = agent.with_tools(tools);

            let behavior = slot
                .behavior
                .clone()
                .map(|b| b.to_behavior())
                .unwrap_or_else(|| bundle.default_behavior.clone().to_behavior());

            let plugin_config = PluginConfig {
                max_turns: slot.max_turns.unwrap_or(10),
                ..Default::default()
            };

            let result = run_plugin_loop(
                bundle.plugin.as_ref(),
                behavior.as_ref(),
                &mut agent,
                &context,
                &plugin_config,
                None,
                Some(&self.retry_policy),
                self.shared_memory.as_deref().map(|m| m as &dyn Memory),
            )
            .await
            .map_err(|e| DAGError::plugin_error(&current_name, e))?;

            let output_json: serde_json::Value = serde_json::from_str(&result.output)
                .map_err(|e| DAGError::OutputParse(e.to_string()))?;

            context[current_name.as_str()] = output_json.clone();

            if let Some(ref mem) = self.shared_memory {
                use lattice_agent::memory::{EntryKind, MemoryEntry};
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_micros().to_string())
                    .unwrap_or_else(|_| "0".to_string());
                mem.save_entry(MemoryEntry {
                    id: format!("dag-{}-{}", current_name, transitions),
                    kind: EntryKind::SessionLog,
                    session_id: self.config.entry.clone(),
                    summary: format!("{} output", current_name),
                    content: result.output,
                    tags: vec![current_name.clone()],
                    created_at: ts,
                });
            }

            let next = self.find_edge(&current_name, &output_json);

            match next {
                Some(HandoffTarget::Single(next_name)) => {
                    current_name = next_name;
                    transitions += 1;
                }
                Some(HandoffTarget::Fork(_)) => return Err(DAGError::ForkNotSupportedInDag),
                None => return Ok(output_json),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handoff_rule::{HandoffCondition, HandoffRule, HandoffTarget};
    use crate::profile::{AgentEdgeConfig, PluginSlotConfig, PluginsConfig};
    use lattice_plugin::registry::PluginRegistry;

    #[test]
    fn test_find_edge_first_match_wins() {
        let edges = vec![
            AgentEdgeConfig {
                from: "review".into(),
                rule: HandoffRule {
                    condition: Some(HandoffCondition {
                        field: "confidence".into(),
                        op: ">".into(),
                        value: serde_json::json!(0.5),
                    }),
                    all: None,
                    any: None,
                    default: false,
                    target: Some(HandoffTarget::Single("refactor".into())),
                },
            },
            AgentEdgeConfig {
                from: "review".into(),
                rule: HandoffRule {
                    condition: None,
                    all: None,
                    any: None,
                    default: true,
                    target: None,
                },
            },
        ];
        let config = PluginsConfig {
            entry: "review".into(),
            slots: vec![],
            edges,
            shared_tools: vec![],
        };
        let registry = PluginRegistry::new();
        let tool_registry = ToolRegistry::new();
        let dag = PluginDagRunner::new(
            &config,
            &registry,
            &tool_registry,
            RetryPolicy::default(),
            None,
        );

        let output = serde_json::json!({"confidence": 0.9});
        let next = dag.find_edge("review", &output);
        assert_eq!(next, Some(HandoffTarget::Single("refactor".into())));

        let output = serde_json::json!({"confidence": 0.3});
        let next = dag.find_edge("review", &output);
        assert_eq!(next, None);
    }

    #[test]
    fn test_dag_error_display() {
        let err = DAGError::SlotNotFound("test".into());
        assert!(err.to_string().contains("test"));
        let err = DAGError::ForkNotSupportedInDag;
        assert!(err.to_string().contains("fork"));
    }
}
