// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Construction inputs for a LANCET runtime.

use std::path::{Path, PathBuf};

use lancet_authority::policy::default::PolicyConfig;
use lancet_protocol::policy::CepMode;
use sha2::{Digest, Sha256};

/// Failure resolving the external project-keyed CEP audit store path.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExternalAuditPathError {
    #[error("platform state directory unavailable")]
    MissingStateDir,
    #[error("project root path is not representable as UTF-8")]
    InvalidCanonicalIdentity,
    #[error("cannot canonicalize project root: {0}")]
    Canonicalize(String),
}

/// Platform state-directory root used for external audit storage (`dirs::state_dir()`).
pub fn audit_state_root() -> Option<PathBuf> {
    dirs::state_dir()
}

/// Deterministic SHA-256 project key from the canonical project root identity.
pub fn project_storage_key(project_root: &Path) -> Result<String, ExternalAuditPathError> {
    let canonical = std::fs::canonicalize(project_root)
        .map_err(|err| ExternalAuditPathError::Canonicalize(err.to_string()))?;
    let identity = canonical
        .to_str()
        .ok_or(ExternalAuditPathError::InvalidCanonicalIdentity)?;
    let mut hasher = Sha256::new();
    hasher.update(identity.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// External audit DB path under a caller-supplied state root (tests/embedders).
pub fn audit_db_path_for_project_in(
    external_state_root: &Path,
    project_root: &Path,
) -> Result<PathBuf, ExternalAuditPathError> {
    let key = project_storage_key(project_root)?;
    Ok(external_state_root
        .join("lattice")
        .join("projects")
        .join(key)
        .join("cep_audit.db"))
}

/// External audit DB path using the platform state directory.
pub fn audit_db_path_for_project(project_root: &Path) -> Result<PathBuf, ExternalAuditPathError> {
    let state_root = dirs::state_dir().ok_or(ExternalAuditPathError::MissingStateDir)?;
    audit_db_path_for_project_in(&state_root, project_root)
}

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
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn legacy_audit_snapshot(path: &Path) -> (Option<u64>, SystemTime) {
        let meta = std::fs::metadata(path).ok();
        (
            meta.as_ref().map(|m| m.len()),
            meta.map(|m| m.modified().unwrap_or(SystemTime::UNIX_EPOCH))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        )
    }

    #[test]
    fn external_audit_distinct_project_keys() {
        let state = TempDir::new().unwrap();
        let a = TempDir::new().unwrap();
        let b = TempDir::new().unwrap();
        std::fs::write(a.path().join("marker"), b"a").unwrap();
        std::fs::write(b.path().join("marker"), b"b").unwrap();
        let path_a = audit_db_path_for_project_in(state.path(), a.path()).unwrap();
        let path_b = audit_db_path_for_project_in(state.path(), b.path()).unwrap();
        assert_ne!(path_a, path_b);
        assert!(
            !path_a
                .to_string_lossy()
                .contains(a.path().to_str().unwrap())
        );
    }

    #[test]
    fn external_audit_symlink_canonical_identity() {
        let state = TempDir::new().unwrap();
        let real = TempDir::new().unwrap();
        let link_parent = TempDir::new().unwrap();
        let link = link_parent.path().join("proj-link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let via_real = audit_db_path_for_project_in(state.path(), real.path()).unwrap();
        let via_link = audit_db_path_for_project_in(state.path(), &link).unwrap();
        assert_eq!(via_real, via_link);
    }

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

    #[test]
    fn external_audit_invalid_canonical_identity_errors() {
        // Non-UTF8 paths are environment-specific; canonicalize failure is portable.
        let err = project_storage_key(Path::new("/this/path/should/not/exist/for/lattice"))
            .expect_err("missing path should not canonicalize");
        assert!(matches!(err, ExternalAuditPathError::Canonicalize(_)));
    }

    #[test]
    fn external_audit_imports_legacy_read_only_once() {
        use lancet_audit::{AuditOutcome, CallAuditMetadata, CepAuditContext, CepAuditStore};
        use lancet_catalog::CepMethod;
        use lancet_catalog::methods::session::TodoWrite;
        use lancet_catalog::methods::workspace::ObserveWorkspace;

        let state = TempDir::new().unwrap();
        let project = TempDir::new().unwrap();
        let legacy_dir = project.path().join(".lattice");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy_path = legacy_dir.join("cep_audit.db");

        let legacy_store = CepAuditStore::open(&legacy_path).unwrap();
        let ctx = CepAuditContext {
            turn_id: "legacy-turn".into(),
            session_id: "legacy-session".into(),
            model_id: "legacy-model".into(),
            runtime_version: "legacy-runtime".into(),
        };
        let start = legacy_store
            .start_call(
                &ctx,
                1,
                TodoWrite::NAME,
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        legacy_store
            .finalize_call(
                start,
                AuditOutcome::Success {
                    summary: "legacy ok".into(),
                },
                None,
                Some(r#"{"required_capability":"mutate_session_state"}"#),
            )
            .unwrap();
        let (legacy_size_before, legacy_mtime_before) = legacy_audit_snapshot(&legacy_path);

        let external_path = audit_db_path_for_project_in(state.path(), project.path()).unwrap();
        let external = CepAuditStore::open(&external_path).unwrap();
        external
            .import_legacy_read_only_if_empty(&legacy_path)
            .unwrap();

        let rows = external
            .query_rows(&lancet_audit::AuditQueryFilters::default())
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].method, TodoWrite::NAME);

        let ctx2 = CepAuditContext {
            turn_id: "direct-turn".into(),
            session_id: "direct-session".into(),
            ..ctx.clone()
        };
        let start2 = external
            .start_call(
                &ctx2,
                2,
                ObserveWorkspace::NAME,
                &CallAuditMetadata::native_policy_enforced(Some("started")),
            )
            .unwrap();
        external
            .finalize_call(
                start2,
                AuditOutcome::Success {
                    summary: "direct ok".into(),
                },
                None,
                None,
            )
            .unwrap();
        let all = external
            .query_rows(&lancet_audit::AuditQueryFilters::default())
            .unwrap();
        assert_eq!(all.len(), 2);

        let (legacy_size_after, legacy_mtime_after) = legacy_audit_snapshot(&legacy_path);
        assert_eq!(legacy_size_before, legacy_size_after);
        assert_eq!(legacy_mtime_before, legacy_mtime_after);
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", legacy_path.display()));
            if sidecar.exists() {
                let before = legacy_audit_snapshot(&sidecar);
                let after = legacy_audit_snapshot(&sidecar);
                assert_eq!(before, after);
            }
        }
    }
}
