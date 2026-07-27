// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Construction inputs for a LANCET runtime.

use std::path::{Path, PathBuf};

use lancet_authority::policy::default::PolicyConfig;
use lancet_protocol::evidence::CapabilityAdmissionPolicy;
use lancet_protocol::policy::CepMode;

// External project-keyed audit store path resolution moved to
// `lancet_audit::store_path` (issue #39); re-exported here so both the public
// `lancet_api::*` paths and internal `crate::config::*` users stay unchanged.
pub use lancet_audit::{
    ExternalAuditPathError, audit_db_path_for_project, audit_db_path_for_project_in,
    audit_state_root, project_storage_key,
};

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
    ///
    /// Legacy embedder/tests helper only; production readers/writers use
    /// [`Self::external_for_project`].
    pub fn default_under(project_root: impl Into<PathBuf>) -> Self {
        AuditBacking::File(project_root.into().join(".lattice").join("cep_audit.db"))
    }

    /// Persistent external backing under `external_state_root` keyed by `project_root`.
    pub fn external_for_project_in(
        external_state_root: impl AsRef<Path>,
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ExternalAuditPathError> {
        Ok(AuditBacking::File(audit_db_path_for_project_in(
            external_state_root.as_ref(),
            project_root.as_ref(),
        )?))
    }

    /// Persistent external backing under the platform state directory.
    pub fn external_for_project(
        project_root: impl AsRef<Path>,
    ) -> Result<Self, ExternalAuditPathError> {
        Ok(AuditBacking::File(audit_db_path_for_project(
            project_root.as_ref(),
        )?))
    }
}

/// Inputs for [`crate::LancetRuntime::build`].
///
/// Policy is supplied as fully-resolved LANCET-native types: this crate does not
/// know about any host application's configuration schema. Translating an
/// application profile into [`PolicyConfig`] is the host's responsibility,
/// keeping LANCET decoupled from LATTICE.
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
    /// Component Tool admission inputs (#28). Fail-closed by default: no
    /// Component Tool is callable until the host supplies its operator policy.
    pub capability_admission: CapabilityAdmissionPolicy,
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
            capability_admission: CapabilityAdmissionPolicy::default(),
            cep_mode: CepMode::Enforce,
            audit: AuditBacking::InMemory,
            model_id: String::new(),
            runtime_version: String::new(),
        }
    }
}

#[cfg(test)]
mod external_audit_tests {
    use super::*;
    use tempfile::TempDir;

    // Path-resolution behavior tests moved to `lancet-audit`
    // (`src/store_path.rs`) with the implementation; only the `AuditBacking`
    // wrapper is exercised here.

    #[test]
    fn external_audit_external_for_project_in_resolves_under_state_root() {
        let state = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let backing = AuditBacking::external_for_project_in(state.path(), project.path()).unwrap();
        match backing {
            AuditBacking::File(path) => {
                assert!(path.starts_with(state.path()));
                assert!(path.ends_with("cep_audit.db"));
            }
            AuditBacking::InMemory => panic!("expected file backing"),
        }
    }
}
