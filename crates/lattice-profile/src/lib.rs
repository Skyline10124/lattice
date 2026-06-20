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

pub use defaults::{
    default_agent_template, default_dev_template, default_editor_template, default_reader_template,
};
pub use discovery::{DiscoveryPaths, ProfileSource, resolve_agent_profile};
pub use error::ProfileError;
pub use load::{LoadedAgentProfile, load_agent_profile};
pub use schema::{
    AgentProfile, AgentSection, CredentialRef, EgressPolicyConfig, LancetCommandIsolationSection,
    LancetCommandSection, LancetIsolationMode, LancetIsolationSection, LancetLandlockBehavior,
    LancetNetworkSection, LancetObservationSection, LancetPolicySection, LancetProjectRootSection,
    LancetSection, LancetUnsupportedHostBehavior, LancetVerifySection,
    LancetWorkspacePolicySection, LlmRetrySection, LlmSection, PromptSection, ToolsSection,
    WorkspaceSection,
};
