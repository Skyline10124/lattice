// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Backend-aware edit I/O: Envelope host-root joins vs Direct Upper primitives.

use std::path::{Component, Path, PathBuf};

use crate::execution_backend::{
    ExecutionBackendError, ExecutionRouting, PathFacts, PathIntent, WorkspacePath,
};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::{DirectPathFacts, DirectPathKind, validate_direct_path_facts};

use super::types::{PatchDiagnostic, PatchErrorCode};
use crate::executor::error::ToolError;

/// Where HashLine edit preflight/commit/rollback reads and writes.
#[derive(Clone)]
pub(crate) enum EditStorage {
    EnvelopeHost {
        project_root: PathBuf,
    },
    Direct {
        execution: ExecutionRouting,
        authorized_region: AuthorizedEffectRegion,
    },
}

impl EditStorage {
    pub(crate) fn envelope_host(project_root: impl Into<PathBuf>) -> Self {
        Self::EnvelopeHost {
            project_root: project_root.into(),
        }
    }

    pub(crate) fn direct(
        execution: ExecutionRouting,
        authorized_region: AuthorizedEffectRegion,
    ) -> Self {
        Self::Direct {
            execution,
            authorized_region,
        }
    }

    pub(crate) fn parse_relative_path(path: &str) -> Result<WorkspacePath, PatchDiagnostic> {
        WorkspacePath::parse(path).map_err(|err| {
            PatchDiagnostic::new(
                PatchErrorCode::BoundaryDenied,
                format!("patch path '{path}' rejected: {err}"),
            )
            .with_path(path.to_string())
        })
    }

    fn host_path(&self, target: &WorkspacePath) -> PathBuf {
        match self {
            Self::EnvelopeHost { project_root } => {
                if target.is_project_root() {
                    project_root.clone()
                } else {
                    project_root.join(target.as_str())
                }
            }
            Self::Direct { .. } => unreachable!("host_path is Envelope-only"),
        }
    }

    pub(crate) async fn resolve_update_target(
        &self,
        path: &str,
        operation_index: usize,
    ) -> Result<WorkspacePath, PatchDiagnostic> {
        let ws_path = Self::parse_relative_path(path).map_err(|mut d| {
            d.operation_index = Some(operation_index);
            d
        })?;

        match self {
            Self::EnvelopeHost { project_root } => {
                let relative = Path::new(path);
                if relative.is_absolute() || has_disallowed_component(relative) {
                    return Err(PatchDiagnostic::new(
                        PatchErrorCode::BoundaryDenied,
                        format!(
                            "patch path '{path}' must be relative and stay under the project root"
                        ),
                    )
                    .with_path(path.to_string())
                    .with_operation_index(operation_index));
                }
                let requested = project_root.join(relative);
                let canonical = tokio::fs::canonicalize(&requested).await.map_err(|error| {
                    PatchDiagnostic::new(
                        PatchErrorCode::MissingFile,
                        ToolError::IoError {
                            path: requested.display().to_string(),
                            error,
                        }
                        .to_string(),
                    )
                    .with_path(path.to_string())
                    .with_operation_index(operation_index)
                })?;
                if !canonical.starts_with(project_root) {
                    return Err(PatchDiagnostic::new(
                        PatchErrorCode::BoundaryDenied,
                        format!("patch path '{path}' resolves outside the project root"),
                    )
                    .with_path(path.to_string())
                    .with_operation_index(operation_index));
                }
                Ok(ws_path)
            }
            Self::Direct {
                execution,
                authorized_region,
            } => {
                ws_path.require_file_path().map_err(|err| {
                    PatchDiagnostic::new(
                        PatchErrorCode::BoundaryDenied,
                        format!("patch path '{path}' rejected: {err}"),
                    )
                    .with_path(path.to_string())
                    .with_operation_index(operation_index)
                })?;
                let facts = preflight_path(execution, &ws_path, PathIntent::Read).await?;
                validate_facts(ws_path.as_str(), &facts, authorized_region)?;
                if !facts.exists {
                    return Err(PatchDiagnostic::new(
                        PatchErrorCode::MissingFile,
                        format!("patch target '{path}' does not exist"),
                    )
                    .with_path(path.to_string())
                    .with_operation_index(operation_index));
                }
                Ok(ws_path)
            }
        }
    }

    pub(crate) async fn read_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        operation_index: usize,
    ) -> Result<Vec<u8>, PatchDiagnostic> {
        match self {
            Self::EnvelopeHost { .. } => {
                let host = self.host_path(target);
                tokio::fs::read(&host).await.map_err(|error| {
                    PatchDiagnostic::new(
                        PatchErrorCode::IoError,
                        ToolError::IoError {
                            path: host.display().to_string(),
                            error,
                        }
                        .to_string(),
                    )
                    .with_path(path.to_string())
                    .with_operation_index(operation_index)
                })
            }
            Self::Direct { execution, .. } => execution
                .backend
                .read_file(target)
                .await
                .map_err(|err| backend_io_error(path, operation_index, err)),
        }
    }

    pub(crate) async fn write_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        content: &[u8],
        operation_index: usize,
    ) -> Result<(), PatchDiagnostic> {
        match self {
            Self::EnvelopeHost { .. } => {
                let host = self.host_path(target);
                tokio::fs::write(&host, content).await.map_err(|error| {
                    PatchDiagnostic::new(
                        PatchErrorCode::IoError,
                        ToolError::IoError {
                            path: host.display().to_string(),
                            error,
                        }
                        .to_string(),
                    )
                    .with_path(path.to_string())
                    .with_operation_index(operation_index)
                })
            }
            Self::Direct { execution, .. } => execution
                .backend
                .write_file(target, content)
                .await
                .map_err(|err| backend_io_error(path, operation_index, err)),
        }
    }

    pub(crate) async fn remove_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        operation_index: usize,
    ) -> Result<(), PatchDiagnostic> {
        match self {
            Self::EnvelopeHost { .. } => {
                let host = self.host_path(target);
                tokio::fs::remove_file(&host).await.map_err(|error| {
                    PatchDiagnostic::new(
                        PatchErrorCode::IoError,
                        ToolError::IoError {
                            path: host.display().to_string(),
                            error,
                        }
                        .to_string(),
                    )
                    .with_path(path.to_string())
                    .with_operation_index(operation_index)
                })
            }
            Self::Direct { execution, .. } => execution
                .backend
                .remove_file(target)
                .await
                .map_err(|err| backend_io_error(path, operation_index, err)),
        }
    }

    pub(crate) async fn rollback_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        previous_bytes: Option<&[u8]>,
        operation: super::types::FileOperationKind,
        operation_index: usize,
    ) -> Result<(), PatchDiagnostic> {
        match (operation, previous_bytes) {
            (super::types::FileOperationKind::Update, Some(bytes))
            | (super::types::FileOperationKind::Delete, Some(bytes)) => {
                self.write_edit_target(path, target, bytes, operation_index)
                    .await
            }
            (super::types::FileOperationKind::Delete, None) => {
                self.remove_edit_target(path, target, operation_index).await
            }
            _ => Ok(()),
        }
    }

    pub(crate) async fn preflight_create(
        &self,
        path: &str,
    ) -> Result<WorkspacePath, PatchDiagnostic> {
        let ws_path = Self::parse_relative_path(path)?;
        match self {
            Self::EnvelopeHost { project_root } => {
                let full = project_root.join(path);
                if full.exists() {
                    return Err(PatchDiagnostic::new(
                        PatchErrorCode::IoError,
                        format!("target file '{path}' already exists"),
                    )
                    .with_path(path.to_string()));
                }
                Ok(ws_path)
            }
            Self::Direct {
                execution,
                authorized_region,
            } => {
                let facts = preflight_path(execution, &ws_path, PathIntent::Create).await?;
                validate_facts(ws_path.as_str(), &facts, authorized_region)?;
                if facts.exists {
                    return Err(PatchDiagnostic::new(
                        PatchErrorCode::IoError,
                        format!("target file '{path}' already exists"),
                    )
                    .with_path(path.to_string()));
                }
                Ok(ws_path)
            }
        }
    }

    pub(crate) async fn read_for_compile(&self, path: &str) -> Result<String, PatchDiagnostic> {
        let ws_path = Self::parse_relative_path(path)?;
        ws_path.require_file_path().map_err(|err| {
            PatchDiagnostic::new(
                PatchErrorCode::BoundaryDenied,
                format!("edit path '{path}' rejected: {err}"),
            )
            .with_path(path.to_string())
        })?;
        if let Self::Direct {
            execution,
            authorized_region,
        } = self
        {
            let facts = preflight_path(execution, &ws_path, PathIntent::Read).await?;
            validate_facts(path, &facts, authorized_region)?;
        }
        let bytes = self.read_edit_target(path, &ws_path, 0).await?;
        String::from_utf8(bytes).map_err(|error| {
            PatchDiagnostic::new(
                PatchErrorCode::IoError,
                format!("edit target '{path}' is not valid UTF-8: {error}"),
            )
            .with_path(path.to_string())
        })
    }
}

async fn preflight_path(
    execution: &ExecutionRouting,
    path: &WorkspacePath,
    intent: PathIntent,
) -> Result<PathFacts, PatchDiagnostic> {
    execution
        .backend
        .resolve_path(path, intent)
        .await
        .map_err(|err| {
            PatchDiagnostic::new(
                PatchErrorCode::IoError,
                format!("adapter preflight failed: {err}"),
            )
            .with_path(path.as_str().to_string())
        })
}

fn validate_facts(
    path: &str,
    facts: &PathFacts,
    authorized_region: &AuthorizedEffectRegion,
) -> Result<(), PatchDiagnostic> {
    let direct_facts = DirectPathFacts {
        normalized_path: facts.normalized_path.clone(),
        exists: facts.exists,
        kind: facts.kind.map(path_kind),
        parent_exists: facts.parent_exists,
        parent_kind: facts.parent_kind.map(path_kind),
        canonical_under_workspace: facts.canonical_under_workspace,
    };
    validate_direct_path_facts(path, &direct_facts, authorized_region).map_err(|err| {
        PatchDiagnostic::new(PatchErrorCode::BoundaryDenied, err.message.clone())
            .with_path(path.to_string())
    })
}

fn path_kind(kind: crate::execution_backend::PathKind) -> DirectPathKind {
    match kind {
        crate::execution_backend::PathKind::File => DirectPathKind::File,
        crate::execution_backend::PathKind::Directory => DirectPathKind::Directory,
        crate::execution_backend::PathKind::Symlink => DirectPathKind::Symlink,
    }
}

fn backend_io_error(
    path: &str,
    operation_index: usize,
    err: ExecutionBackendError,
) -> PatchDiagnostic {
    PatchDiagnostic::new(
        PatchErrorCode::IoError,
        format!("backend I/O failed: {err}"),
    )
    .with_path(path.to_string())
    .with_operation_index(operation_index)
}

fn has_disallowed_component(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}
