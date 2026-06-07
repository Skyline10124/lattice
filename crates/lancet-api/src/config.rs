// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Construction inputs for a LANCET runtime.

use std::path::PathBuf;

use lancet_authority::policy::default::PolicyConfig;
use lancet_native::enforcement::isolation::CommandIsolationRuntimeConfig;
use lancet_protocol::envelope::CepMode;

/// Where CEP audit records are persisted for the runtime.
#[derive(Debug, Clone)]
pub enum AuditBacking {
    /// Ephemeral in-memory store (tests, smoke runs).
    InMemory,
    /// Persistent SQLite store at the given path; created if absent.
    File(PathBuf),
}

impl AuditBacking {
    /// Conventional persistent backing under a project root: `<root>/.lattice/cep_audit.db`.
    pub fn default_under(project_root: impl Into<PathBuf>) -> Self {
        AuditBacking::File(project_root.into().join(".lattice").join("cep_audit.db"))
    }
}

/// Inputs for [`crate::LancetRuntime::build`].
///
/// Policy is supplied as fully-resolved LANCET-native types: this crate does not
/// know about any host application's configuration schema. Translating an
/// application profile into [`PolicyConfig`] / [`CommandIsolationRuntimeConfig`]
/// is the host's responsibility, keeping LANCET decoupled from LATTICE.
///
/// `project_root` is the runtime workspace root used for the base observation
/// envelope and audit DB path. The guard's effective root travels inside
/// `policy.project_root` and may differ (e.g. a configured sub-root); both are
/// honored independently, matching prior behavior.
pub struct LancetRuntimeConfig {
    /// Runtime workspace root (base envelope, audit path).
    pub project_root: PathBuf,
    /// Catalog method names the runtime should expose to callers.
    pub enabled_tools: Vec<String>,
    /// Fully-resolved authority/policy configuration.
    pub policy: PolicyConfig,
    /// Command physical-boundary isolation configuration.
    pub command_isolation: CommandIsolationRuntimeConfig,
    /// CEP enforcement mode.
    pub cep_mode: CepMode,
    /// Audit store backing.
    pub audit: AuditBacking,
    /// Model identifier recorded in the audit context.
    pub model_id: String,
    /// Runtime version string recorded in the audit context and attestations.
    pub runtime_version: String,
}

impl LancetRuntimeConfig {
    /// A minimal config rooted at `project_root` with safe policy defaults.
    ///
    /// Audit defaults to in-memory and `cep_mode` to [`CepMode::Enforce`]; callers
    /// override fields as needed before [`crate::LancetRuntime::build`].
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        let project_root = project_root.into();
        let policy = PolicyConfig::defaults_for_project_root(project_root.clone());
        Self {
            project_root,
            enabled_tools: Vec::new(),
            policy,
            command_isolation: CommandIsolationRuntimeConfig::default(),
            cep_mode: CepMode::Enforce,
            audit: AuditBacking::InMemory,
            model_id: String::new(),
            runtime_version: String::new(),
        }
    }
}
