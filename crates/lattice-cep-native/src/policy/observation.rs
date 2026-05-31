//! CEP workspace observation path policy configuration and resolution.

use std::path::{Component, Path, PathBuf};

use lattice_cep_protocol::error::{PermissionErrorData, ValidationErrorData};
use lattice_cep_protocol::{CepError, CepErrorCode, CepErrorData, Retriability};

use super::CepPolicyConfig;

/// Immutable observation policy derived from profile `[cep.project_root]` and `[cep.observation]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationPolicyConfig {
    pub allowed_roots: Vec<PathBuf>,
    pub max_single_read_bytes: usize,
    pub max_turn_observation_bytes: usize,
    pub max_grep_matches: usize,
    pub max_list_entries: usize,
    pub max_find_paths: usize,
    pub max_todo_ops: usize,
}

impl ObservationPolicyConfig {
    pub fn defaults_for_root(project_root: &Path) -> Self {
        Self {
            allowed_roots: vec![project_root.to_path_buf()],
            max_single_read_bytes: 10 * 1024 * 1024,
            max_turn_observation_bytes: 10 * 1024 * 1024,
            max_grep_matches: 1000,
            max_list_entries: 1000,
            max_find_paths: 1000,
            max_todo_ops: 100,
        }
    }
}

/// Resolved observation target after CEP path policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedObservationTarget {
    pub request_target: String,
    pub canonical_target: PathBuf,
    pub allowed_root: PathBuf,
}

/// Reject explicit parent traversal in path text (Validation-family).
pub fn reject_parent_traversal(path: &str) -> Result<(), CepError> {
    for component in Path::new(path).components() {
        if matches!(component, Component::ParentDir) {
            return Err(path_containment(
                "path",
                format!("path traversal denied: '{path}' contains '..' component"),
            ));
        }
    }
    Ok(())
}

/// Resolve and validate an observation path before backend filesystem work.
pub async fn resolve_observation_target(
    policy: &CepPolicyConfig,
    path: &str,
) -> Result<ResolvedObservationTarget, CepError> {
    if path.contains('\0') {
        return Err(validation_error(
            "path",
            "path contains NUL byte",
            CepErrorCode::TypeError,
        ));
    }
    if path.trim().is_empty() {
        return Err(validation_error(
            "path",
            "path must not be empty",
            CepErrorCode::TypeError,
        ));
    }
    reject_parent_traversal(path)?;

    let request_target = path.to_string();
    let requested = resolve_user_path(&policy.project_root, path);
    let canonical = tokio::fs::canonicalize(&requested)
        .await
        .map_err(|_| not_found(&request_target))?;

    let allowed_root = policy
        .observation
        .allowed_roots
        .iter()
        .find(|root| path_under_root(&canonical, root))
        .cloned()
        .ok_or_else(|| project_root_violation(&canonical, &policy.project_root))?;

    Ok(ResolvedObservationTarget {
        request_target,
        canonical_target: canonical,
        allowed_root,
    })
}

/// Backend safety-net denial after CEP policy allowed invocation (Permission-family).
pub fn backend_safety_net_denial(path: &str) -> CepError {
    project_root_violation_message(format!(
        "backend safety net denied path '{}': outside effective observation scope",
        path
    ))
}

fn resolve_user_path(project_root: &Path, path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

fn path_under_root(canonical: &Path, root: &Path) -> bool {
    let Ok(root) = std::fs::canonicalize(root) else {
        return canonical == root || canonical.starts_with(root);
    };
    if canonical == root {
        return true;
    }
    canonical.starts_with(&root)
}

fn path_containment(field: &str, reason: impl Into<String>) -> CepError {
    let reason = reason.into();
    CepError::new(
        CepErrorCode::PathContainment,
        reason.clone(),
        CepErrorData::Validation(ValidationErrorData {
            field: field.into(),
            reason,
        }),
        Retriability::Permanent,
    )
}

fn validation_error(field: &str, reason: impl Into<String>, code: CepErrorCode) -> CepError {
    let reason = reason.into();
    CepError::new(
        code,
        reason.clone(),
        CepErrorData::Validation(ValidationErrorData {
            field: field.into(),
            reason,
        }),
        Retriability::Permanent,
    )
}

fn not_found(path: &str) -> CepError {
    validation_error(
        "path",
        format!("not_found: '{path}'"),
        CepErrorCode::TypeError,
    )
}

fn project_root_violation(canonical: &Path, project_root: &Path) -> CepError {
    project_root_violation_message(format!(
        "path '{}' is outside allowed observation roots (effective project root '{}')",
        canonical.display(),
        project_root.display()
    ))
}

fn project_root_violation_message(message: String) -> CepError {
    CepError::new(
        CepErrorCode::ProjectRootViolation,
        message.clone(),
        CepErrorData::Permission(PermissionErrorData {
            required_capability: "workspace_observation".into(),
            active_envelope_id: None,
            granted_capabilities: vec![],
        }),
        Retriability::Permanent,
    )
}

#[cfg(test)]
pub(crate) const BACKEND_SAFETY_NET_TEST_PATH: &str = "__cep_backend_safety_net__";

/// Test-only hook simulating a backend-time denial after CEP policy allowed the path.
#[cfg(test)]
pub(crate) fn check_backend_safety_net(path: &str) -> Option<CepError> {
    if path == BACKEND_SAFETY_NET_TEST_PATH {
        Some(backend_safety_net_denial(path))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::CepPolicyConfig;
    use std::fs;
    use tempfile::TempDir;

    fn policy_for(dir: &TempDir) -> CepPolicyConfig {
        CepPolicyConfig::defaults_for_project_root(dir.path())
    }

    #[tokio::test]
    async fn observation_policy_rejects_parent_traversal() {
        let dir = TempDir::new().unwrap();
        let policy = policy_for(&dir);
        let err = resolve_observation_target(&policy, "../outside")
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::PathContainment);
        assert_eq!(
            err.code.family(),
            lattice_cep_protocol::CepErrorFamily::Validation
        );
    }

    #[tokio::test]
    async fn observation_policy_allows_relative_inside_root() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), "hi").unwrap();
        let policy = policy_for(&dir);
        let resolved = resolve_observation_target(&policy, "a.txt").await.unwrap();
        assert!(resolved.canonical_target.ends_with("a.txt"));
    }

    #[tokio::test]
    async fn observation_policy_allows_absolute_inside_root() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("abs.txt");
        fs::write(&file, "x").unwrap();
        let policy = policy_for(&dir);
        let resolved = resolve_observation_target(&policy, file.to_str().unwrap())
            .await
            .unwrap();
        assert_eq!(resolved.canonical_target, file.canonicalize().unwrap());
    }

    #[tokio::test]
    async fn observation_policy_denies_absolute_outside_root() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let file = outside.path().join("secret.txt");
        fs::write(&file, "no").unwrap();
        let policy = policy_for(&dir);
        let err = resolve_observation_target(&policy, file.to_str().unwrap())
            .await
            .unwrap_err();
        assert_eq!(err.code, CepErrorCode::ProjectRootViolation);
        assert_eq!(
            err.code.family(),
            lattice_cep_protocol::CepErrorFamily::Permission
        );
    }

    #[tokio::test]
    async fn observation_policy_denies_symlink_escape() {
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "secret").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside_file, dir.path().join("link.txt")).unwrap();
            let policy = policy_for(&dir);
            let err = resolve_observation_target(&policy, "link.txt")
                .await
                .unwrap_err();
            assert_eq!(err.code, CepErrorCode::ProjectRootViolation);
        }
    }

    #[test]
    fn backend_safety_net_denial_is_permission_family() {
        let err = backend_safety_net_denial(BACKEND_SAFETY_NET_TEST_PATH);
        assert_eq!(err.code, CepErrorCode::ProjectRootViolation);
        assert_eq!(
            err.code.family(),
            lattice_cep_protocol::CepErrorFamily::Permission
        );
    }
}
