use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use lattice_agent::memory::Memory;
use lattice_agent::Agent;
use lattice_core::retry::RetryPolicy;
use lattice_core::router::ModelRouter;
use lattice_tools::{
    tool_registry::{merge_tool_definitions, ToolRegistry},
    DefaultToolExecutor,
};

use crate::bundle::behavior_to_behavior_trait;
use crate::erased_runner::run_plugin_loop;
use crate::orchestration::{HandoffTarget, PluginsConfig};
use crate::PluginConfig;

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
        source: crate::PluginError,
    },

    #[error("output JSON parse failed: {0}")]
    OutputParse(String),

    #[error("fork not supported in intra-agent DAG — use Pipeline fork:target")]
    ForkNotSupportedInDag,

    #[error("plugin registry not configured")]
    MissingPluginRegistry,

    #[error("tool registry not configured")]
    MissingToolRegistry,

    #[error("tool executor setup failed: {0}")]
    ToolExecutor(String),
}

impl DAGError {
    pub(crate) fn plugin_error(slot: &str, err: crate::PluginError) -> Self {
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
    plugin_registry: &'a crate::registry::PluginRegistry,
    tool_registry: &'a ToolRegistry,
    registry_tool_access: Option<Arc<ToolRegistry>>,
    retry_policy: RetryPolicy,
    shared_memory: Option<Arc<dyn Memory>>,
    model_router: Option<Arc<ModelRouter>>,
    credentials: Option<HashMap<String, String>>,
}

struct DagAgentFactory<'a> {
    tool_registry: &'a ToolRegistry,
    shared_tools: &'a [String],
    registry_tool_access: Option<Arc<ToolRegistry>>,
    shared_memory: Option<Arc<dyn Memory>>,
    model_router: Option<Arc<ModelRouter>>,
    credentials: Option<HashMap<String, String>>,
}

impl<'a> DagAgentFactory<'a> {
    fn from_runner(runner: &PluginDagRunner<'a>) -> Self {
        Self {
            tool_registry: runner.tool_registry,
            shared_tools: &runner.config.shared_tools,
            registry_tool_access: runner.registry_tool_access.clone(),
            shared_memory: runner.shared_memory.clone(),
            model_router: runner.model_router.clone(),
            credentials: runner.credentials.clone(),
        }
    }

    fn build_slot_agent(
        &self,
        model: &str,
        slot_tools: &[String],
        plugin_tools: &[lattice_core::types::ToolDefinition],
    ) -> Result<Agent, DAGError> {
        let mut agent = Agent::new(self.resolve_model(model)?);
        if let Some(ref mem) = self.shared_memory {
            agent = agent.with_memory(Arc::clone(mem));
        }

        let tools = merge_tool_definitions(
            self.tool_registry,
            self.shared_tools,
            slot_tools,
            plugin_tools,
        );
        let authorized_tool_names = tools
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<BTreeSet<_>>();
        agent = agent.with_tools(tools);
        agent =
            agent.with_tool_executor(Box::new(self.default_tool_executor(authorized_tool_names)?));

        Ok(agent)
    }

    fn resolve_model(&self, model: &str) -> Result<lattice_core::ResolvedModel, DAGError> {
        if let Some(router) = &self.model_router {
            return Ok(router.resolve(model, None)?);
        }
        let router = match &self.credentials {
            Some(creds) => ModelRouter::with_credentials(creds.clone()),
            None => ModelRouter::new(),
        };
        Ok(router.resolve(model, None)?)
    }

    fn default_tool_executor(
        &self,
        authorized_tool_names: BTreeSet<String>,
    ) -> Result<DefaultToolExecutor, DAGError> {
        let executor = match &self.registry_tool_access {
            Some(registry) if !registry.is_empty() => DefaultToolExecutor::new_with_registry(
                ".",
                Some(Arc::clone(registry) as Arc<dyn lattice_tools::RegistryToolAccess>),
            )
            .map_err(DAGError::ToolExecutor),
            _ => DefaultToolExecutor::new(".").map_err(DAGError::ToolExecutor),
        }?;
        Ok(executor.with_authorized_tools(authorized_tool_names))
    }
}

impl<'a> PluginDagRunner<'a> {
    pub fn new(
        config: &'a PluginsConfig,
        plugin_registry: &'a crate::registry::PluginRegistry,
        tool_registry: &'a ToolRegistry,
        retry_policy: RetryPolicy,
        shared_memory: Option<Arc<dyn Memory>>,
    ) -> Self {
        Self {
            config,
            plugin_registry,
            tool_registry,
            registry_tool_access: None,
            retry_policy,
            shared_memory,
            model_router: None,
            credentials: None,
        }
    }

    /// Attach registry-backed tool handlers so DAG slot tools can execute the
    /// same handlers advertised in their tool definitions.
    pub fn with_registry_tool_access(mut self, registry: Arc<ToolRegistry>) -> Self {
        self.registry_tool_access = Some(registry);
        self
    }

    /// Use a shared model router supplied by the top-level runtime.
    pub fn with_model_router(mut self, router: Arc<ModelRouter>) -> Self {
        self.model_router = Some(router);
        self
    }

    /// Set externally supplied credentials for model resolution.
    pub fn with_credentials(mut self, credentials: HashMap<String, String>) -> Self {
        self.credentials = Some(credentials);
        self
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
        let agent_factory = DagAgentFactory::from_runner(self);

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
            let mut agent =
                agent_factory.build_slot_agent(model, &slot.tools, bundle.plugin.tools())?;

            let behavior = slot
                .behavior
                .clone()
                .map(|b| behavior_to_behavior_trait(&b))
                .unwrap_or_else(|| behavior_to_behavior_trait(&bundle.default_behavior));

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
                crate::save_plugin_entry(
                    mem.as_ref(),
                    &current_name,
                    format!("dag-{transitions}"),
                    format!("{} output", current_name),
                    result.output,
                    vec![current_name.clone()],
                );
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
    use crate::orchestration::{
        DagEdgeConfig, HandoffCondition, HandoffRule, HandoffTarget, PluginsConfig,
    };
    use crate::registry::PluginRegistry;
    use lattice_core::types::ToolDefinition;
    use lattice_tools::tool_registry::ToolHandler;
    use lattice_tools::ToolExecutor;

    #[test]
    fn test_find_edge_first_match_wins() {
        let edges = vec![
            DagEdgeConfig {
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
            DagEdgeConfig {
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

    fn make_tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.into(),
            description: "test".into(),
            parameters: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn dag_executor_authorizes_only_merged_tool_names() {
        let mut definition_registry = ToolRegistry::new();
        definition_registry.register(
            "slot_tool",
            ToolHandler::Native(Arc::new(|_| Ok("slot ok".into()))),
            make_tool("slot_tool"),
        );
        let mut handler_registry = ToolRegistry::new();
        handler_registry.register(
            "slot_tool",
            ToolHandler::Native(Arc::new(|_| Ok("slot ok".into()))),
            make_tool("slot_tool"),
        );
        let factory = DagAgentFactory {
            tool_registry: &definition_registry,
            shared_tools: &[],
            registry_tool_access: Some(Arc::new(handler_registry)),
            shared_memory: None,
            model_router: None,
            credentials: None,
        };

        let executor = factory
            .default_tool_executor(BTreeSet::from(["slot_tool".to_string()]))
            .unwrap();

        let result = executor
            .execute(&lattice_core::types::ToolCall::new(
                "call_test".into(),
                lattice_core::types::FunctionCall::new("bash".into(), "{}".into()),
            ))
            .await;

        assert!(
            result.contains("__denial_report__"),
            "expected DenialReport JSON, got: {result}"
        );
    }
}
