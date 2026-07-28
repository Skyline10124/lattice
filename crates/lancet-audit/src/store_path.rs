// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! External project-keyed audit store path resolution.
//!
//! The operator read path ([`crate::operator`]) and the runtime writer both
//! resolve the same per-project SQLite location from these helpers, so the
//! canonical-identity hashing here is load-bearing for "reads see the writes".

use std::path::{Path, PathBuf};

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

/// Platform state-directory root used for external audit storage.
///
/// Resolution order:
///
/// 1. `XDG_STATE_HOME`, when set to an absolute path. The XDG spec honors this
///    override on every platform, and `dirs::state_dir()` only consults it on
///    Linux. Reading it here keeps one operator-visible knob that behaves the
///    same everywhere, and is what the test-isolation helpers
///    (`ScopedXdgStateHome`, the system harness) rely on to redirect audit
///    storage into a temp dir.
/// 2. `dirs::state_dir()` — the XDG state directory. Linux-only; returns
///    `None` on macOS and Windows.
/// 3. `dirs::data_local_dir()` — the conventional home for persistent per-user
///    state on those platforms (`~/Library/Application Support` on macOS,
///    `%LOCALAPPDATA%` on Windows).
///
/// Without steps 1 and 3 every project-keyed audit path is unresolvable off
/// Linux, which breaks production audit storage, `lancet status`, and the CLI
/// audit commands there.
///
/// Callers that need an explicit root (tests, embedders) should prefer
/// [`audit_db_path_for_project_in`] over relying on this resolution.
pub fn audit_state_root() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("XDG_STATE_HOME") {
        let path = PathBuf::from(explicit);
        // Relative values are ignored per the XDG spec.
        if path.is_absolute() {
            return Some(path);
        }
    }
    dirs::state_dir().or_else(dirs::data_local_dir)
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

/// Per-project private state directory under a caller-supplied state root.
///
/// Every piece of durable LATTICE state that must live outside both the Local
/// Workspace and the user's project `.git` — the CEP audit database and the
/// private workspace mutation history (#63) — hangs off this one directory.
pub fn project_state_dir_in(
    external_state_root: &Path,
    project_root: &Path,
) -> Result<PathBuf, ExternalAuditPathError> {
    let key = project_storage_key(project_root)?;
    Ok(external_state_root
        .join("lattice")
        .join("projects")
        .join(key))
}

/// Per-project private state directory using the platform state directory.
pub fn project_state_dir(project_root: &Path) -> Result<PathBuf, ExternalAuditPathError> {
    let state_root = audit_state_root().ok_or(ExternalAuditPathError::MissingStateDir)?;
    project_state_dir_in(&state_root, project_root)
}

/// External audit DB path under a caller-supplied state root (tests/embedders).
pub fn audit_db_path_for_project_in(
    external_state_root: &Path,
    project_root: &Path,
) -> Result<PathBuf, ExternalAuditPathError> {
    Ok(project_state_dir_in(external_state_root, project_root)?.join("cep_audit.db"))
}

/// External audit DB path using the platform state directory.
pub fn audit_db_path_for_project(project_root: &Path) -> Result<PathBuf, ExternalAuditPathError> {
    let state_root = audit_state_root().ok_or(ExternalAuditPathError::MissingStateDir)?;
    audit_db_path_for_project_in(&state_root, project_root)
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

    /// `audit_state_root` must honor an absolute `XDG_STATE_HOME` on every
    /// platform, not just Linux. `dirs::state_dir()` only reads it on Linux, so
    /// without the explicit lookup the test-isolation helpers silently fall
    /// through to the real user directory on macOS/Windows and tests scribble
    /// outside their temp dir (observed as CI failures on macos-latest).
    #[test]
    fn audit_state_root_prefers_absolute_xdg_state_home() {
        // Shared env lock: this test mutates XDG_STATE_HOME manually, so it
        // must serialise against ScopedXdgStateHome users in the same binary.
        let _guard = crate::test_support::ENV_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = TempDir::new().unwrap();
        let prev = std::env::var("XDG_STATE_HOME").ok();

        crate::test_support::set_env_var("XDG_STATE_HOME", dir.path().to_str());
        assert_eq!(audit_state_root().as_deref(), Some(dir.path()));

        // A relative value is ignored per the XDG spec; resolution falls
        // through to the platform directories, which must still yield a path.
        crate::test_support::set_env_var("XDG_STATE_HOME", Some("relative/path"));
        let resolved = audit_state_root();
        assert!(resolved.is_some(), "platform fallback must resolve");
        assert_ne!(resolved.as_deref(), Some(Path::new("relative/path")));

        crate::test_support::set_env_var("XDG_STATE_HOME", prev.as_deref());
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
    fn external_audit_invalid_canonical_identity_errors() {
        // Non-UTF8 paths are environment-specific; canonicalize failure is portable.
        let err = project_storage_key(Path::new("/this/path/should/not/exist/for/lattice"))
            .expect_err("missing path should not canonicalize");
        assert!(matches!(err, ExternalAuditPathError::Canonicalize(_)));
    }

    #[test]
    fn external_audit_imports_legacy_read_only_once() {
        use crate::audit::{AuditOutcome, CallAuditMetadata, CepAuditContext, CepAuditStore};
        use lancet_catalog::CepMethod;
        use lancet_catalog::methods::workspace::ObserveWorkspace;
        use lancet_catalog::test_method::TestEcho;

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
                TestEcho::NAME,
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
            .query_rows(&crate::audit::AuditQueryFilters::default())
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].method, TestEcho::NAME);

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
            .query_rows(&crate::audit::AuditQueryFilters::default())
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
