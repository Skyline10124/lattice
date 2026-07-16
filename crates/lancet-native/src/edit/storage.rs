// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Backend-aware edit I/O via Upper (MSB) primitives.

use crate::execution_backend::{
    ExecutionBackendError, ExecutionRouting, PathFacts as BackendPathFacts, PathIntent,
    WorkspacePath,
};
use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::{PathFacts, PathKind, validate_path_facts};

use super::types::{PatchDiagnostic, PatchErrorCode};

/// Where HashLine edit preflight/commit/rollback reads and writes.
///
/// All edit I/O goes through the Upper (MSB) execution backend — there is no
/// host-local envelope mode. The authorized region carries the user-controlled
/// write-deny blacklist.
#[derive(Clone)]
pub(crate) struct EditStorage {
    execution: ExecutionRouting,
    authorized_region: AuthorizedEffectRegion,
}

impl EditStorage {
    pub(crate) fn new(
        execution: ExecutionRouting,
        authorized_region: AuthorizedEffectRegion,
    ) -> Self {
        Self {
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

    pub(crate) async fn resolve_update_target(
        &self,
        path: &str,
        operation_index: usize,
    ) -> Result<WorkspacePath, PatchDiagnostic> {
        let ws_path = Self::parse_relative_path(path).map_err(|mut d| {
            d.operation_index = Some(operation_index);
            d
        })?;

        ws_path.require_file_path().map_err(|err| {
            PatchDiagnostic::new(
                PatchErrorCode::BoundaryDenied,
                format!("patch path '{path}' rejected: {err}"),
            )
            .with_path(path.to_string())
            .with_operation_index(operation_index)
        })?;
        let facts = preflight_path(&self.execution, &ws_path, PathIntent::Read).await?;
        validate_facts(ws_path.as_str(), &facts, &self.authorized_region)?;
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

    pub(crate) async fn read_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        operation_index: usize,
    ) -> Result<Vec<u8>, PatchDiagnostic> {
        self.execution
            .backend
            .read_file(target)
            .await
            .map_err(|err| backend_io_error(path, operation_index, err))
    }

    pub(crate) async fn write_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        content: &[u8],
        operation_index: usize,
    ) -> Result<(), PatchDiagnostic> {
        self.execution
            .backend
            .write_file(target, content)
            .await
            .map_err(|err| backend_io_error(path, operation_index, err))
    }

    /// No-replace create install (ADR 0003 decision 8).
    ///
    /// Distinct from [`write_edit_target`]: the install must fail if the
    /// target exists at install time, so a concurrent writer between
    /// `preflight_create` and the install surfaces as a create error rather
    /// than a silent overwrite.
    pub(crate) async fn create_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        content: &[u8],
    ) -> Result<(), PatchDiagnostic> {
        self.execution
            .backend
            .create_file(target, content)
            .await
            .map_err(|err| backend_io_error(path, 0, err))
    }

    pub(crate) async fn remove_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        operation_index: usize,
    ) -> Result<(), PatchDiagnostic> {
        self.execution
            .backend
            .remove_file(target)
            .await
            .map_err(|err| backend_io_error(path, operation_index, err))
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
        let facts = preflight_path(&self.execution, &ws_path, PathIntent::Create).await?;
        validate_facts(ws_path.as_str(), &facts, &self.authorized_region)?;
        if facts.exists {
            return Err(PatchDiagnostic::new(
                PatchErrorCode::IoError,
                format!("target file '{path}' already exists"),
            )
            .with_path(path.to_string()));
        }
        Ok(ws_path)
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
        let facts = preflight_path(&self.execution, &ws_path, PathIntent::Read).await?;
        validate_facts(path, &facts, &self.authorized_region)?;
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
) -> Result<BackendPathFacts, PatchDiagnostic> {
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
    facts: &BackendPathFacts,
    authorized_region: &AuthorizedEffectRegion,
) -> Result<(), PatchDiagnostic> {
    let path_facts = PathFacts {
        normalized_path: facts.normalized_path.clone(),
        exists: facts.exists,
        kind: facts.kind.map(path_kind),
        parent_exists: facts.parent_exists,
        parent_kind: facts.parent_kind.map(path_kind),
        canonical_under_workspace: facts.canonical_under_workspace,
    };
    validate_path_facts(path, &path_facts, authorized_region).map_err(|err| {
        PatchDiagnostic::new(PatchErrorCode::BoundaryDenied, err.message.clone())
            .with_path(path.to_string())
    })
}

fn path_kind(kind: crate::execution_backend::PathKind) -> PathKind {
    match kind {
        crate::execution_backend::PathKind::File => PathKind::File,
        crate::execution_backend::PathKind::Directory => PathKind::Directory,
        crate::execution_backend::PathKind::Symlink => PathKind::Symlink,
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
