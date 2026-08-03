// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Edit I/O against the one concrete Local Workspace.
//!
//! All edit preflight/commit/rollback reads and writes go through
//! [`LocalWorkspace`] Structured Workspace Operations — there is no
//! pluggable backend. The authorized region carries the user-controlled
//! write-deny blacklist.

use std::sync::Arc;

use lancet_authority::effect::region::AuthorizedEffectRegion;
use lancet_authority::guard::validate_path_facts;

use crate::workspace::{
    InstalledFile, LocalWorkspace, LocalWorkspaceError, PathFacts, PathIntent, WorkspacePath,
};

use super::types::{PatchDiagnostic, PatchErrorCode};

/// Where HashLine edit preflight/commit/rollback reads and writes.
///
/// Holds the one concrete [`LocalWorkspace`] plus the authorized effect
/// region carrying the user-controlled write-deny blacklist.
#[derive(Clone)]
pub(crate) struct EditStorage {
    workspace: Arc<LocalWorkspace>,
    authorized_region: AuthorizedEffectRegion,
}

impl EditStorage {
    pub(crate) fn new(
        workspace: Arc<LocalWorkspace>,
        authorized_region: AuthorizedEffectRegion,
    ) -> Self {
        Self {
            workspace,
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
        let facts = self.preflight_path(&ws_path, PathIntent::Read).await?;
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
        self.workspace
            .read_file(target)
            .await
            .map_err(|err| local_io_error(path, operation_index, err))
    }

    /// Atomic install over an existing target.
    ///
    /// Returns the [`InstalledFile`] record: the post-install content hash is
    /// the Local Revision observed immediately after the rename.
    pub(crate) async fn write_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        content: &[u8],
        operation_index: usize,
    ) -> Result<InstalledFile, PatchDiagnostic> {
        self.workspace
            .install_atomic(target, content)
            .await
            .map_err(|err| local_io_error(path, operation_index, err))
    }

    pub(crate) async fn remove_edit_target(
        &self,
        path: &str,
        target: &WorkspacePath,
        operation_index: usize,
    ) -> Result<(), PatchDiagnostic> {
        self.workspace
            .remove_file(target)
            .await
            .map_err(|err| local_io_error(path, operation_index, err))
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
            | (super::types::FileOperationKind::Delete, Some(bytes)) => self
                .write_edit_target(path, target, bytes, operation_index)
                .await
                .map(|_| ()),
            (super::types::FileOperationKind::Delete, None) => {
                self.remove_edit_target(path, target, operation_index).await
            }
            _ => Ok(()),
        }
    }

    async fn preflight_path(
        &self,
        path: &WorkspacePath,
        intent: PathIntent,
    ) -> Result<PathFacts, PatchDiagnostic> {
        self.workspace
            .resolve_facts(path, intent)
            .await
            .map_err(|err| {
                PatchDiagnostic::new(
                    PatchErrorCode::IoError,
                    format!("secure path resolution failed: {err}"),
                )
                .with_path(path.as_str().to_string())
            })
    }
}

fn validate_facts(
    path: &str,
    facts: &PathFacts,
    authorized_region: &AuthorizedEffectRegion,
) -> Result<(), PatchDiagnostic> {
    validate_path_facts(path, &facts.to_guard(), authorized_region).map_err(|err| {
        PatchDiagnostic::new(PatchErrorCode::BoundaryDenied, err.message.clone())
            .with_path(path.to_string())
    })
}

fn local_io_error(path: &str, operation_index: usize, err: LocalWorkspaceError) -> PatchDiagnostic {
    PatchDiagnostic::new(
        PatchErrorCode::IoError,
        format!("local workspace I/O failed: {err}"),
    )
    .with_path(path.to_string())
    .with_operation_index(operation_index)
}
