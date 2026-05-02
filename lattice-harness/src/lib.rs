pub mod dag_runner;
pub mod events;
pub mod handoff_rule;
pub mod lattice_dir;
pub mod memory;
pub mod micro_agent;
pub mod pipeline;
pub mod profile;
pub mod registry;
pub mod runner;
pub mod tools;
pub mod watcher;

pub use dag_runner::{DAGError, PluginDagRunner};
pub use events::{EventBus, PipelineEvent};
pub use handoff_rule::{HandoffCondition, HandoffRule, HandoffTarget};
pub use lattice_dir::{BusToml, LatticeDir};
pub use micro_agent::{MicroAgent, MicroAgentHandle};
pub use pipeline::{AgentError, AgentResult, DryRunReport, Pipeline, PipelineRun};
pub use profile::{
    AgentConfig, AgentEdgeConfig, AgentProfile, BehaviorConfig, BusConfigProfile, HandoffConfig,
    MemoryConfigProfile, PluginSlotConfig, PluginsConfig, SystemConfig, ToolsConfig,
};
pub use registry::AgentRegistry;
pub use runner::AgentRunner;
pub use tools::{merge_tool_definitions, ToolRegistry};
pub use watcher::Watcher;

#[cfg(feature = "axum")]
pub mod ws;
