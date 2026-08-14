// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Project-store composition (ADR 0005 §3, GitHub #143).
//!
//! `session new` is the only entry point that may first-create durable
//! stores. It resolves the Local Workspace root through
//! [`crate::LocalWorkspaceRoot`], derives the `ProjectStoreKey`, creates (or
//! validates) the project's stores, and only then publishes the SessionState
//! that is permanently bound to that key. Every other path strictly opens:
//! an expected-missing store fails closed.
//!
//! `invocations.git` is deliberately absent — #139-A owns its creation, and
//! this ticket does not leave a placeholder behind.

use lancet_api::{WorkspaceHistory, WorkspaceHistoryBacking};
use lattice_session::{ProjectStoreKey, StateLayout};

use crate::session::{ApiError, Result};

/// First-create (or validate) every project-scoped store for `key`.
///
/// Idempotent: an existing, identity-valid store is validated and left
/// untouched, so re-running `session new` in the same workspace is safe.
pub fn create_project_stores(layout: &StateLayout, key: ProjectStoreKey) -> Result<()> {
    WorkspaceHistory::create(WorkspaceHistoryBacking::for_key(layout.clone(), key)).map_err(
        |err| ApiError::Internal(format!("create project workspace-history store: {err}")),
    )?;
    Ok(())
}

/// Strictly open the workspace-history store for `key`.
///
/// A missing store is [`ApiError::StoreUnavailable`]: the project's stores are
/// created by `session new`, never lazily on a Turn.
pub fn open_workspace_history(
    layout: &StateLayout,
    key: ProjectStoreKey,
) -> Result<WorkspaceHistory> {
    WorkspaceHistory::open(WorkspaceHistoryBacking::for_key(layout.clone(), key)).map_err(|err| {
        ApiError::StoreUnavailable(format!("open project workspace-history store: {err}"))
    })
}
