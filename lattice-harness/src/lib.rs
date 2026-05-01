pub mod dispatch;
pub mod events;
pub mod handoff_rule;
pub mod lattice_dir;
pub mod micro_agent;
pub mod pipeline;
pub mod profile;
pub mod registry;
pub mod runner;
pub mod watcher;

pub use dispatch::HarnessAgentDispatcher;
pub use events::{EventBus, PipelineEvent};
pub use handoff_rule::{HandoffCondition, HandoffRule, HandoffTarget};
pub use lattice_dir::{BusToml, LatticeDir};
pub use micro_agent::{MicroAgent, MicroAgentHandle};
pub use pipeline::{AgentError, AgentResult, DryRunReport, Pipeline, PipelineRun};
pub use profile::{
    AgentConfig, AgentProfile, BehaviorConfig, BusConfigProfile, HandoffConfig,
    MemoryConfigProfile, SystemConfig, ToolsConfig,
};
pub use registry::AgentRegistry;
pub use runner::AgentRunner;
pub use watcher::Watcher;

#[cfg(feature = "axum")]
pub mod ws;
