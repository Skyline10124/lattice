// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

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

pub use defaults::default_agent_template;
pub use discovery::{DiscoveryPaths, ProfileSource, resolve_agent_profile};
pub use error::ProfileError;
pub use load::{LoadedAgentProfile, load_agent_profile, read_regular_file};
pub use schema::{
    AgentProfile, AgentSection, LancetCapabilitiesSection, LancetNetworkSection,
    LancetObservationSection, LancetPolicySection, LancetProjectRootSection, LancetSection,
    LancetWorkspacePolicySection, LlmRetrySection, LlmSection, PromptSection, ToolsSection,
};
