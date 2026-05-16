//! Strict single-agent profile schema, loading, discovery, defaults, and pure lowering.
//!
//! This crate defines the v1 `agent.toml` contract and performs read-only loading
//! with strict allowlist validation. It owns no runtime, orchestrator, agent, session,
//! plugin, CLI, or TUI state.

mod defaults;
mod discovery;
mod error;
mod load;
mod lowering;
mod schema;
mod spec;

pub use defaults::{
    default_agent_template, default_dev_template, default_editor_template, default_reader_template,
};
pub use discovery::{resolve_agent_profile, DiscoveryPaths, ProfileSource};
pub use error::ProfileError;
pub use load::{load_agent_profile, LoadedAgentProfile};
pub use lowering::lower_profile_to_spec;
pub use schema::{
    AgentProfile, AgentSection, BehaviorSection, ContextSection, PromptSection, SandboxMode,
    SecuritySection, ToolsSection,
};
pub use spec::{BehaviorSpec, ContextSpec, GenericAgentSpec, SecuritySpec, ToolSelectionSpec};
