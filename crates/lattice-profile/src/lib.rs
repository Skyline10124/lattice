//! Strict single-agent profile schema, loading, discovery, and defaults.
//!
//! This crate defines the v1 `agent.toml` contract and performs read-only loading
//! with strict allowlist validation. It owns no runtime, orchestrator, agent, session,
//! plugin, CLI, or TUI state.

mod defaults;
mod discovery;
mod error;
mod load;
mod schema;

pub use defaults::{
    default_agent_template, default_dev_template, default_editor_template, default_reader_template,
};
pub use discovery::{resolve_agent_profile, DiscoveryPaths, ProfileSource};
pub use error::ProfileError;
pub use load::{load_agent_profile, LoadedAgentProfile};
pub use schema::{
    AgentProfile, AgentSection, CepCommandSection, CepNetworkSection, CepObservationSection,
    CepProjectRootSection, CepSection, PromptSection, ToolsSection,
};
